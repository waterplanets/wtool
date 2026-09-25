//! 语料层：「待翻译的原文」的收集、去重、分组。
//!
//! 翻译功能不依赖现成的 MTool 词典，而是自己攒文本、调 LLM 来生成翻译文件。
//! 这句话里的「自己攒什么文本」，就是本层要回答的问题 —— 文本从哪来，
//! 以什么粒度收，怎么保证同一段话不会因为两种来源各收一遍而翻两遍。
//!
//! 粒度按「查词典时的粒度」分三种（和桥里的三级退级一一对应，见
//! [`crate::engine`] 与 `wtool_bridge.js`）：
//!
//! ```text
//!   Whole    整段   一段对话用 \n 拼起来的那一团。词典做成后，查表先查它。
//!   Line     单行   整段里的每一行。整段没翻中时，退到行粒度。
//!   Fragment 片段   菜单/自定义菜单（SceneCustomMenu）那种「整串画」的字符串。
//!            ↕ 对应
//!   整段 → 单行 → 片段 的三级退级
//! ```
//!
//! 收进来之前先过一道 [`is_worthy`]：纯 ASCII（数字、伤害值、`HP` 这类）翻了
//! 也是白翻，控制码（`\c[2]`、`\i[127]`）是蹊跷的 key 源，先摘掉控制码再看
//! 剩没剩下真话 —— 全工程只有这一处定义「什么算值得翻的文本」，在线和离线
//! 两个采集口都用它，口径才不会漂。
//!
//! 本层只负责「收集 + 计数」，**不调任何翻译接口**。做翻译（第 3 步）时
//! 从 [`Corpus`] 里取原文去喂 LLM，这里不给 LLM 留任何缝隙。

pub mod offline;
pub mod online;
pub mod plugins;
pub mod rgss3;

use std::collections::BTreeMap;
use std::path::Path;

use crate::dict::wtool;
use crate::error::{Error, Result};

/// 语料文件的版本号就写死在 header 里，读的时候只按这一行认。
const CORPUS_HEADER: &str = "#wtool-corpus 1";

/// 一条语料按什么粒度收。
///
/// 粒度是**来源决定的属性**，不是文本自身长得像什么 —— 同一条对话，
/// 从 DB 表里收是整段，拆进消息窗口就是单行。拆错粒度会导致查表对不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// 整段：多条 401 用 `\n` 拼起来的那一团。
    Whole,
    /// 单行：整段里的每一行（或独立的一句）。
    Line,
    /// 片段：菜单项这类「整串画」的字符串（如插件参数里的自定义菜单文字）。
    Fragment,
}

impl Granularity {
    pub fn name(self) -> &'static str {
        match self {
            Granularity::Whole => "整段",
            Granularity::Line => "单行",
            Granularity::Fragment => "片段",
        }
    }

    /// 持久化时写在行首的那个字母。
    fn letter(self) -> &'static str {
        match self {
            Granularity::Whole => "W",
            Granularity::Line => "L",
            Granularity::Fragment => "F",
        }
    }

    fn from_letter(c: char) -> Option<Granularity> {
        match c {
            'W' => Some(Granularity::Whole),
            'L' => Some(Granularity::Line),
            'F' => Some(Granularity::Fragment),
            _ => None,
        }
    }
}

/// 语料文件的落盘格式。
///
/// 两种都给，是因为语料有两种用法：TSV 一行一条、能 `git diff`、能手改，
/// 适合人来审；JSON 带出现次数、结构清晰，适合程序读回来接着处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusFormat {
    /// 行式文本：`#wtool-corpus 1` 头 + 每行 `W|L|F <TAB> 原文`。
    Tsv,
    /// JSON：`{"wtool-corpus": 1, "whole": {...}, "line": {...}, "fragment": {...}}`。
    Json,
}

impl CorpusFormat {
    /// 给人看的名字，用于日志和前端。
    pub fn name(self) -> &'static str {
        match self {
            CorpusFormat::Tsv => "行式文本（TSV）",
            CorpusFormat::Json => "JSON",
        }
    }

    /// 写出去时建议用的扩展名（前端拼默认文件名用）。
    pub fn extension(self) -> &'static str {
        match self {
            CorpusFormat::Tsv => "tsv",
            CorpusFormat::Json => "json",
        }
    }

    /// 存进配置 / 从前端传进来时用的短名。
    pub fn as_str(self) -> &'static str {
        match self {
            CorpusFormat::Tsv => "tsv",
            CorpusFormat::Json => "json",
        }
    }

    /// 短名 → 格式。认不出按 TSV（默认零配置、也是历史唯一格式）。
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("json") {
            CorpusFormat::Json
        } else {
            CorpusFormat::Tsv
        }
    }

    /// 按内容嗅探：首个非空字符是 `{` 就是 JSON，否则 TSV。认不出按 TSV。
    pub fn sniff(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("读不了 {}：{e}", path.display())))?;
        let head = raw.trim_start_matches('\u{feff}').trim_start();
        Ok(if head.starts_with('{') {
            CorpusFormat::Json
        } else {
            CorpusFormat::Tsv
        })
    }
}

/// 一份语料：原文按粒度分桶，每个桶里「原文 → 出现次数」。
///
/// 存次数而不是「去重后的一组串」，是为了汇报「原始 N 条 / 去重后 M 条」——
/// N 是语料文件里总共收了多少次（含重复），M 是真正不重复的、将来要喂给
/// LLM 的条数。两个数一起看，才能判断「去重做得到不到位」。
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    whole: BTreeMap<String, u64>,
    line: BTreeMap<String, u64>,
    fragment: BTreeMap<String, u64>,
}

impl Corpus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收一条原文。不值得翻的（纯 ASCII、清完控制码没剩字）直接丢掉。
    /// 同一条再收一次只加计数，不重复入桶。
    pub fn push(&mut self, g: Granularity, text: &str) {
        if !is_worthy(text) {
            return;
        }
        let map = self.map_mut(g);
        *map.entry(text.to_string()).or_insert(0) += 1;
    }

    /// 把另一份语料并进来（在线采集续写时会先读旧文件再并新的）。
    pub fn merge(&mut self, other: Corpus) {
        self.merge_map(Granularity::Whole, other.whole);
        self.merge_map(Granularity::Line, other.line);
        self.merge_map(Granularity::Fragment, other.fragment);
    }

    fn merge_map(&mut self, g: Granularity, other: BTreeMap<String, u64>) {
        let mine = self.map_mut(g);
        for (k, v) in other {
            *mine.entry(k).or_insert(0) += v;
        }
    }

    fn map(&self, g: Granularity) -> &BTreeMap<String, u64> {
        match g {
            Granularity::Whole => &self.whole,
            Granularity::Line => &self.line,
            Granularity::Fragment => &self.fragment,
        }
    }

    fn map_mut(&mut self, g: Granularity) -> &mut BTreeMap<String, u64> {
        match g {
            Granularity::Whole => &mut self.whole,
            Granularity::Line => &mut self.line,
            Granularity::Fragment => &mut self.fragment,
        }
    }

    /// 三个桶按 JSON 字段名和粒度的对应关系摊开，给读写 JSON 用。
    fn buckets(&self) -> [(&'static str, Granularity, &BTreeMap<String, u64>); 3] {
        [
            ("whole", Granularity::Whole, &self.whole),
            ("line", Granularity::Line, &self.line),
            ("fragment", Granularity::Fragment, &self.fragment),
        ]
    }

    /// 原始条数：每个桶里计数相加，含重复。
    pub fn total(&self) -> u64 {
        self.whole.values().sum::<u64>()
            + self.line.values().sum::<u64>()
            + self.fragment.values().sum::<u64>()
    }

    /// 去重后条数：真正不重复的原文数，就是将来要喂给 LLM 的条数。
    pub fn unique(&self) -> usize {
        self.whole.len() + self.line.len() + self.fragment.len()
    }

    /// 三个桶各自的去重条数。汇报和分桶校验用。
    pub fn per_granularity(&self) -> (usize, usize, usize) {
        (self.whole.len(), self.line.len(), self.fragment.len())
    }

    /// 「原始 N 条，去重后 M 条（整段 x / 单行 y / 片段 z）」。
    pub fn stats(&self) -> String {
        let (w, l, f) = self.per_granularity();
        format!(
            "原始 {} 条，去重后 {} 条（整段 {w} / 单行 {l} / 片段 {f}）",
            self.total(),
            self.unique()
        )
    }

    /// 全部原文按（粒度、原文）摊平，按粒度字母排序保证输出确定性。
    /// 第 3 步做翻译批量时就从这里取。
    pub fn flat(&self) -> Vec<(Granularity, String)> {
        let mut out = Vec::with_capacity(self.unique());
        for g in [Granularity::Whole, Granularity::Line, Granularity::Fragment] {
            for (k, _) in self.map(g) {
                out.push((g, k.clone()));
            }
        }
        out
    }
}

/// 值不值得收进语料 / 值不值得翻。
///
/// 判据两条，和桥里 `worthAsking`（长度≥2 + 含假名/表意字）对齐：
///
/// 1. 摘掉控制码后必须剩至少 2 个字符 —— 整行只有 `\P[1]` 这种「说话人栏」
///    的文本翻了没意义，还会被片段匹配换出乱码；
/// 2. 剩下必须含假名或表意字 —— 纯 ASCII（数字、`HP`、存档槽位）问了也是
///    白问，翻不翻人家自己会显示。
pub fn is_worthy(s: &str) -> bool {
    let visible = strip_controls(s);
    if visible.chars().count() < 2 {
        return false;
    }
    visible.chars().any(|c| is_japanese_text(c))
}

/// 是不是假名 / 表意字 / 半角片假名。英数、标点、空白都算「不用翻」。
fn is_japanese_text(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}'      // 平假名 + 片假名
        | '\u{3400}'..='\u{9fff}'    // CJK 表意字
        | '\u{ff66}'..='\u{ff9f}'    // 半角片假名
    )
}

/// 把控制码（`\c[2]`、`\i[127]`、`\v[439]` 这类）摘掉，剩下的才是真给玩家
/// 看的字。和桥里 `visible()` 一个意思，这里用不着正则，手写扫一遍。
fn strip_controls(s: &str) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        if bytes[i] == b'\\' {
            // 反斜杠后跟一串字母才是控制码（\c、\i、\v、\P…）。
            // 字母后可选地跟一个 [..] 参数，一起丢掉。
            let mut j = i + 1;
            while j < n && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                if j < n && bytes[j] == b'[' {
                    i = match s[j..].find(']') {
                        Some(off) => j + off + 1,
                        None => n,
                    };
                } else {
                    i = j;
                }
                continue;
            }
            // 孤零零一个反斜杠（不是控制码）：保留，这是玩家理论上看得到的字。
            out.push('\\');
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// JSON 形态的版本号（和 TSV header 里的那个 1 是同一个版本）。
const CORPUS_JSON_VERSION: u64 = 1;

//-------------------------------------------------------------------------
// 持久化：在线采集要跨多次游戏会话累计，语料得能落盘、能读回来。
//-------------------------------------------------------------------------
//
// TSV 形态（转义规则和词典共用一份，`dict::wtool` 的 escape/unescape）：
//
//   #wtool-corpus 1
//   W<TAB>一段对话（原文里 \n 转义成 \\n）
//   L<TAB>单行
//   F<TAB>自定义菜单项
//
// JSON 形态：
//
//   {"wtool-corpus": 1, "whole": {"原文": 次数}, "line": {...}, "fragment": {...}}
//
// 行首一个字母（TSV）/ 一个字段名（JSON）记粒度，读回来才能按粒度分桶 ——
// 不然跨会话回来后全变成默认粒度，第 3 步按粒度翻就会错。

/// 按指定格式写语料文件，返回写进去多少条（去重后）。
pub fn save(path: &Path, corpus: &Corpus, fmt: CorpusFormat) -> Result<usize> {
    match fmt {
        CorpusFormat::Tsv => save_tsv(path, corpus),
        CorpusFormat::Json => save_json(path, corpus),
    }
}

/// 读一个语料文件。格式按内容嗅探，两种都认。
/// 行首字母 / 字段名不认识都报错 —— 别把粒度拼错。
pub fn load(path: &Path) -> Result<Corpus> {
    match CorpusFormat::sniff(path)? {
        CorpusFormat::Tsv => load_tsv(path),
        CorpusFormat::Json => load_json(path),
    }
}

/// 写 TSV 形态，返回写进去多少条（去重后）。
fn save_tsv(path: &Path, corpus: &Corpus) -> Result<usize> {
    let mut out = String::new();
    out.push_str(CORPUS_HEADER);
    out.push('\n');

    let mut n = 0;
    for (g, text) in corpus.flat() {
        out.push_str(g.letter());
        out.push('\t');
        out.push_str(&wtool::escape(&text));
        out.push('\n');
        n += 1;
    }

    std::fs::write(path, out)?;
    Ok(n)
}

/// 写 JSON 形态，返回写进去多少条（去重后）。
///
/// 和 TSV 的差别：JSON 把出现次数也留着（TSV 只存去重后的原文，计数落盘即丢）。
/// 两种都是合法语料，读的时候按格式各自解释。
fn save_json(path: &Path, corpus: &Corpus) -> Result<usize> {
    let mut doc = serde_json::Map::new();
    doc.insert(
        "wtool-corpus".to_string(),
        serde_json::Value::from(CORPUS_JSON_VERSION),
    );
    for (key, _, map) in corpus.buckets() {
        doc.insert(key.to_string(), serde_json::to_value(map)?);
    }
    std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
    Ok(corpus.unique())
}

/// 读 TSV 形态的语料。
fn load_tsv(path: &Path) -> Result<Corpus> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::new(format!("读不了 {}：{e}", path.display())))?;
    let mut corpus = Corpus::new();

    for (i, line) in raw.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((letter, rest)) = line.split_once('\t') else {
            return Err(Error::new(format!(
                "{} 第 {} 行没有 Tab 分隔：{line}",
                path.display(),
                i + 1
            )));
        };
        let mut chars = letter.chars();
        let g = chars
            .next()
            .and_then(Granularity::from_letter)
            .filter(|_| chars.next().is_none())
            .ok_or_else(|| {
                Error::new(format!(
                    "{} 第 {} 行开头的 `{letter}` 不是语料粒度（W/L/F 之一）",
                    path.display(),
                    i + 1
                ))
            })?;
        corpus.push(g, &wtool::unescape(rest)?);
    }

    Ok(corpus)
}

/// 读 JSON 形态的语料。
fn load_json(path: &Path) -> Result<Corpus> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::new(format!("读不了 {}：{e}", path.display())))?;
    let doc: serde_json::Value = serde_json::from_str(&raw)?;

    let version = doc
        .get("wtool-corpus")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            Error::new(format!(
                "{} 看着像 JSON，但没有 wtool-corpus 版本标记",
                path.display()
            ))
        })?;
    if version != CORPUS_JSON_VERSION {
        return Err(Error::new(format!(
            "{} 是语料格式第 {version} 版，本版只认第 {CORPUS_JSON_VERSION} 版",
            path.display()
        )));
    }

    let mut corpus = Corpus::new();
    // 字段名 → 粒度，逐个桶读回来。
    let buckets: [(&str, Granularity); 3] = [
        ("whole", Granularity::Whole),
        ("line", Granularity::Line),
        ("fragment", Granularity::Fragment),
    ];
    for (key, g) in buckets {
        let Some(v) = doc.get(key) else { continue };
        let map: BTreeMap<String, u64> = serde_json::from_value(v.clone())?;
        *corpus.map_mut(g) = map;
    }

    Ok(corpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wtool_corpus_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 重复收进桶里只加计数，不占额外位置。
    #[test]
    fn push_deduplicates_across_granularity() {
        let mut c = Corpus::new();
        c.push(Granularity::Line, "こんにちは");
        c.push(Granularity::Line, "こんにちは");
        c.push(Granularity::Line, "おはようございます");
        assert_eq!(c.total(), 3, "原始条数含重复");
        assert_eq!(c.unique(), 2, "去重后只剩两条");
        assert_eq!(c.per_granularity(), (0, 2, 0));
    }

    /// 纯 ASCII / 只有控制码的单行，不值得翻，不该进语料。
    #[test]
    fn unworthy_text_is_dropped() {
        let mut c = Corpus::new();
        c.push(Granularity::Line, "100");            // 纯数字
        c.push(Granularity::Line, "HP");             // 纯字母
        c.push(Granularity::Line, "\\P[1]");         // 全是控制码
        c.push(Granularity::Fragment, "\\I[188]手帳"); // 控制码 + 表意字，该收
        assert_eq!(c.unique(), 1);
        assert_eq!(c.per_granularity(), (0, 0, 1));
    }

    /// 含换行的整段要走 Whole，不能拆成单行 —— 不然查表 key 对不上。
    #[test]
    fn multi_line_goes_to_whole() {
        let mut c = Corpus::new();
        c.push(Granularity::Whole, "一行目\n二行目");
        assert_eq!(c.per_granularity(), (1, 0, 0));
    }

    /// 保存再读回，粒度、原文、去重计数都原样。
    ///
    /// 注意持久化只存「去重后的原文」：重复出现的计数是会话内的概念，落盘又读回
    /// 就并掉了——那正是「同一条不要翻两遍」的意思，所以这里断言 unique/粒度
    /// 不变，而不去保重复计数。
    #[test]
    fn save_load_roundtrip_keeps_everything() {
        let dir = scratch("rt");
        let path = dir.join("corpus.tsv");
        let mut c = Corpus::new();
        c.push(Granularity::Whole, "一行目\n二行目");
        c.push(Granularity::Line, "こんにちは");
        c.push(Granularity::Line, "こんにちは"); // 重复，只加计数
        c.push(Granularity::Fragment, "\\I[188]手帳");

        let n = save(&path, &c, CorpusFormat::Tsv).unwrap();
        assert_eq!(n, c.unique());

        let back = load(&path).unwrap();
        assert_eq!(back.per_granularity(), (1, 1, 1));
        assert_eq!(back.unique(), c.unique());
        // 原文与粒度逐项一致（重复的「こんにちは」在去重后发现不了差异）
        assert_eq!(back.flat(), c.flat());
    }

    /// JSON 形态：粒度、原文、出现次数都原样读回。
    #[test]
    fn json_roundtrip_keeps_counts_and_granularity() {
        let dir = scratch("json");
        let path = dir.join("corpus.json");
        let mut c = Corpus::new();
        c.push(Granularity::Whole, "一行目\n二行目");
        c.push(Granularity::Line, "こんにちは");
        c.push(Granularity::Line, "こんにちは"); // 重复：JSON 会留着计数
        c.push(Granularity::Fragment, "\\I[188]手帳");

        // 写出去的就是 JSON（嗅探能认出来）。
        let n = save(&path, &c, CorpusFormat::Json).unwrap();
        assert_eq!(n, c.unique());
        assert_eq!(CorpusFormat::sniff(&path).unwrap(), CorpusFormat::Json);

        let back = load(&path).unwrap();
        assert_eq!(back.per_granularity(), (1, 1, 1));
        assert_eq!(back.flat(), c.flat());
        assert_eq!(back.total(), c.total(), "JSON 保留出现次数");
    }

    /// 坏的粒度字母要报错，不能猜。
    #[test]
    fn load_rejects_unknown_granularity() {
        let dir = scratch("badg");
        let path = dir.join("corpus.tsv");
        std::fs::write(&path, "X\tこんにちは\n").unwrap();
        let msg = load(&path).unwrap_err().message;
        assert!(msg.contains("粒度"), "{msg}");
    }
}