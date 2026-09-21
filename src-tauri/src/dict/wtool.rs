//! wtool 自己的翻译文件格式 —— 读和写都在这里。
//!
//! 两种格式，给不同的人用：
//!
//! - **行式文本**（[`read_tsv`] / [`write_tsv`]）：一行一条 `原文<TAB>译文`，
//!   按原文排序。能二分、能流式读、能 `git diff`、用户拿记事本就能改。
//! - **JSON**（[`read_json`] / [`write_json`]）：`{"wtool": 1, "entries": {...}}`。
//!   外面多一层壳是为了带版本号 —— 将来格式要改，读的时候有地方判断。
//!
//! 两种格式的转义规则一样，和 JSON 对齐，省得记两套：
//!
//! ```text
//!   \\  ->  \        \t  ->  制表符        \n  ->  换行        \r  ->  回车
//! ```
//!
//! 之所以非转义不可：词典的 key 里**真的会出现**换行（一段对话是几条 401 指令
//! 用 `\n` 拼起来的，[`crate::model::Dialogue::text`] 就是那个样子）和反斜杠
//! （游戏文本里的控制码 `\c[2]`、`\i[127]` 原样留着）。不转义的话一条词条会被
//! 拆成两行，读回来就全乱了。
//!
//! 空译文按「未翻译」处理，**不写出去、读进来也不算词条** —— 和
//! [`crate::dict::mtool::Dict::exact`] 对空值的约定保持一致。

use std::collections::BTreeMap;
use std::path::Path;

use crate::dict::format::read_text;
use crate::error::{Error, Result};

/// wtool JSON 的版本号。格式将来有破坏性改动就 +1，读的时候认这个数。
const JSON_VERSION: u64 = 1;

/// 行式文本的头部标记行。写出去时带上，读的时候不强制要求 ——
/// 手写的、或者从别处导出的 TSV 没有这一行也该能读。
const TSV_HEADER: &str = "#wtool-tsv 1";

/// 读一个行式文本词典。
pub fn read_tsv(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw = read_text(path)?;
    let mut map = BTreeMap::new();

    for (i, line) in raw.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        // 注释和空行是给人看的，不参与词条。
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // 少一个 Tab 说明这行不是「原文<TAB>译文」，与其猜，不如指出第几行。
        let Some((src, dst)) = line.split_once('\t') else {
            return Err(Error::new(format!(
                "{} 第 {} 行没有 Tab 分隔：{line}",
                path.display(),
                i + 1
            )));
        };

        let src = unescape(src)?;
        let dst = unescape(dst)?;
        if src.is_empty() || dst.is_empty() {
            continue;
        }
        map.insert(src, dst);
    }

    Ok(map)
}

/// 写一个行式文本词典，返回写出去几条。
///
/// `BTreeMap` 天然按 key 排序，所以输出是确定的：同一份词典写两次逐字节相同，
/// diff 里只会出现真的变化。
pub fn write_tsv(path: &Path, map: &BTreeMap<String, String>) -> Result<usize> {
    let mut out = String::new();
    out.push_str(TSV_HEADER);
    out.push('\n');

    let mut n = 0;
    for (src, dst) in entries(map) {
        out.push_str(&escape(src));
        out.push('\t');
        out.push_str(&escape(dst));
        out.push('\n');
        n += 1;
    }

    std::fs::write(path, out)?;
    Ok(n)
}

/// 读一个 wtool JSON 词典。
pub fn read_json(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw = read_text(path)?;
    let doc: serde_json::Value = serde_json::from_str(&raw)?;

    let version = doc
        .get("wtool")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            Error::new(format!(
                "{} 有 wtool 标记，但不是一个版本号（wtool: {}）",
                path.display(),
                doc.get("wtool").unwrap_or(&serde_json::Value::Null)
            ))
        })?;
    // 版本比我们认识的还新，说明文件是更新版 wtool 写的，读下去只会读错。
    if version != JSON_VERSION {
        return Err(Error::new(format!(
            "{} 是 wtool 格式第 {version} 版，本版 wtool 只认第 {JSON_VERSION} 版",
            path.display()
        )));
    }

    let entries = doc
        .get("entries")
        .ok_or_else(|| Error::new(format!("{} 缺 entries 字段", path.display())))?;
    let map: BTreeMap<String, String> = serde_json::from_value(entries.clone())?;

    Ok(map
        .into_iter()
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect())
}

/// 写一个 wtool JSON 词典，返回写出去几条。
pub fn write_json(path: &Path, map: &BTreeMap<String, String>) -> Result<usize> {
    let kept: BTreeMap<&str, &str> = entries(map).collect();
    let n = kept.len();

    let doc = serde_json::json!({ "wtool": JSON_VERSION, "entries": kept });
    std::fs::write(path, serde_json::to_string(&doc)?)?;
    Ok(n)
}

/// 真正写进文件的那些词条：空 key、空译文一律不算（空译文 = 未翻译）。
fn entries<'a>(
    map: &'a BTreeMap<String, String>,
) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    map.iter()
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .map(|(k, v)| (k.as_str(), v.as_str()))
}

/// 把一条文本转义成「一行装得下」的样子。
/// `pub(crate)`：语料层也用它写持久化文件，转义规则全工程只有这一份。
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// [`escape`] 的逆运算。
///
/// 不认识的转义直接报错，而不是把反斜杠吞掉：`\c[2]` 这种「反斜杠本来就有
/// 意义」的文本在词典里满地都是，静悄悄地按「未知转义」处理会把控制码吃掉，
/// 游戏里那一段就没颜色/没图标了 —— 还不如当场说「要写反斜杠请用 `\\`」。
pub(crate) fn unescape(s: &str) -> Result<String> {
    if !s.contains('\\') {
        return Ok(s.to_string());
    }

    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                return Err(Error::new(format!(
                    "不认识的转义 `\\{other}`（要写反斜杠本身请用 `\\\\`）"
                )))
            }
            None => return Err(Error::new("行尾有一个孤零零的 `\\`".to_string())),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时文件。项目一贯不新增 crate，测试也不引 `tempfile`。
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wtool_dict_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 专门挑难对付的内容：真换行、制表符、反斜杠控制码、日语。
    fn tricky() -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        map.insert("こんにちは".to_string(), "你好".to_string());
        map.insert("一行目\n二行目".to_string(), "第一行\n第二行".to_string());
        map.insert("タブ\t入り".to_string(), "带\t制表符".to_string());
        map.insert("\\c[2]赤い文字\\c[0]".to_string(), "\\c[2]红字\\c[0]".to_string());
        map.insert("\\".to_string(), "反斜杠".to_string());
        map
    }

    #[test]
    fn tsv_roundtrip_keeps_every_byte() {
        let dir = scratch("tsv_rt");
        let path = dir.join("d.tsv");
        let src = tricky();

        let n = write_tsv(&path, &src).unwrap();
        assert_eq!(n, src.len());
        // 换行必须被转义，否则一条词条会被拆成两行。
        assert!(std::fs::read_to_string(&path).unwrap().contains("一行目\\n二行目"));

        assert_eq!(read_tsv(&path).unwrap(), src);
    }

    #[test]
    fn json_roundtrip_keeps_every_byte() {
        let dir = scratch("json_rt");
        let path = dir.join("d.json");
        let src = tricky();

        let n = write_json(&path, &src).unwrap();
        assert_eq!(n, src.len());
        assert_eq!(read_json(&path).unwrap(), src);
    }

    /// 空译文按「未翻译」处理：不写出去，读回来也不算词条。
    #[test]
    fn empty_target_is_not_an_entry() {
        let dir = scratch("empty");
        let path = dir.join("d.tsv");

        let mut src = BTreeMap::new();
        src.insert("まだ".to_string(), String::new());
        src.insert("done".to_string(), "已完成".to_string());
        assert_eq!(write_tsv(&path, &src).unwrap(), 1);

        let back = read_tsv(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert!(back.contains_key("done"));
    }

    /// 注释和空行是给人看的，不该变成词条。
    #[test]
    fn tsv_skips_comments_and_blank_lines() {
        let dir = scratch("comment");
        let path = dir.join("d.tsv");
        std::fs::write(&path, "#wtool-tsv 1\n\n# 随手写点备注\nアイテム\t道具\n").unwrap();

        let map = read_tsv(&path).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("アイテム").unwrap(), "道具");
    }

    /// 少一个 Tab 要说清是第几行，不能猜。
    #[test]
    fn tsv_reports_the_bad_line() {
        let dir = scratch("badline");
        let path = dir.join("d.tsv");
        std::fs::write(&path, "アイテム\t道具\n这一行忘了 Tab\n").unwrap();

        let msg = read_tsv(&path).unwrap_err().message;
        assert!(msg.contains("第 2 行"), "{msg}");
    }

    /// 裸反斜杠要报错，不能被静悄悄吞掉 —— 那会把控制码改掉。
    #[test]
    fn tsv_rejects_unknown_escape() {
        let dir = scratch("esc");
        let path = dir.join("d.tsv");
        std::fs::write(&path, "\\c[2]赤\t红\n").unwrap();

        let msg = read_tsv(&path).unwrap_err().message;
        assert!(msg.contains("转义"), "{msg}");
    }

    /// 版本号对不上要拒绝，别按老格式硬读。
    #[test]
    fn json_rejects_unknown_version() {
        let dir = scratch("ver");
        let path = dir.join("d.json");
        std::fs::write(&path, "{\"wtool\": 99, \"entries\": {}}").unwrap();

        let msg = read_json(&path).unwrap_err().message;
        assert!(msg.contains("99"), "{msg}");
    }
}