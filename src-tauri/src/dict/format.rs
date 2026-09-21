//! 词典文件的格式识别与分发。
//!
//! wtool 现在能读三种词典文件，靠内容自己认，不让用户先选格式：
//!
//! - **MTool 词典**（`TrsData.json`）：一个扁平的 `{"原文": "译文"}` JSON 对象。
//!   这是外部契约，**只读不写** —— 写出去的文件不该冒充它。
//! - **wtool 行式文本**（`.tsv`）：一行一条 `原文<TAB>译文`，可排序、可 diff、
//!   能手改。
//! - **wtool JSON**：带 `wtool` 版本标记的对象，词条在 `entries` 里。
//!   和 MTool 那个扁平表只差一层外壳，好处是能带上版本号，将来格式要改有地方说。
//!
//! 认不出来时报「不认识」并列出支持哪些格式，而不是硬套一个解析器上去 ——
//! 加密的 MTool 文件尤其要说清楚，不然用户看到的是「JSON 解析失败」这种
//! 什么都指不出来的话。

use std::collections::BTreeMap;
use std::path::Path;

use crate::dict::{mtool, wtool};
use crate::error::{Error, Result};

/// 给用户看的「wtool 认识哪些格式」，拼错误信息时用。
pub const SUPPORTED: &str = "\
  · MTool 词典：TrsData.json，扁平的 {\"原文\": \"译文\"} JSON 对象\n\
  · wtool 行式文本：一行一条「原文<TAB>译文」，制表符与换行分别转义成 \\t 与 \\n\n\
  · wtool JSON：{\"wtool\": 1, \"entries\": {\"原文\": \"译文\"}}";

/// 认出来的词典文件格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictFormat {
    /// MTool 的 `TrsData.json`。
    MtoolJson,
    /// wtool 的行式文本。
    WtoolTsv,
    /// wtool 的 JSON。
    WtoolJson,
}

impl DictFormat {
    /// 给人看的名字，用于日志和前端。
    pub fn name(self) -> &'static str {
        match self {
            DictFormat::MtoolJson => "MTool 词典",
            DictFormat::WtoolTsv => "wtool 行式文本",
            DictFormat::WtoolJson => "wtool JSON",
        }
    }

    /// 写出去时建议用的扩展名（前端拼默认文件名用）。
    pub fn extension(self) -> &'static str {
        match self {
            DictFormat::MtoolJson | DictFormat::WtoolJson => "json",
            DictFormat::WtoolTsv => "tsv",
        }
    }

    /// 猜一个文件是什么格式。认不出来就报错，并列出支持哪些。
    ///
    /// 判据只有两条，都不看文件名（用户可能随便起名字）：
    ///
    /// 1. 第一个有意义的字符是 `{` → JSON。再看有没有 `wtool` 这个顶层标记：
    ///    有就是我们自己的，没有就按 MTool 的扁平表认。
    /// 2. 否则看有没有「一行里带 Tab 的正文」（注释和空行不算）→ 行式文本。
    ///
    /// 加密的 MTool 文件在进 UTF-8 之前先拦下来：它开头是 `99 99 99 9X`，
    /// 直接报「只能读明文」，比让 UTF-8 解码失败说「不是文本」有用得多。
    pub fn sniff(path: &Path) -> Result<Self> {
        let bytes = read_bytes(path)?;

        if bytes.first() == Some(&0x99) {
            return Err(Error::new(format!(
                "{} 像是加密过的 MTool 词典（开头是 99 99 99），wtool 只能读明文。\n\
                 请在 MTool 里导出明文的 TrsData.json 再试。",
                path.display()
            )));
        }

        let text = decode(path, strip_bom(bytes))?;

        if text.trim_start().starts_with('{') {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                Error::new(format!(
                    "{} 看着像 JSON，但解析不了：{e}\nwtool 支持：\n{SUPPORTED}",
                    path.display()
                ))
            })?;
            return Ok(if v.get("wtool").is_some() {
                DictFormat::WtoolJson
            } else {
                DictFormat::MtoolJson
            });
        }

        if has_entry_line(&text) {
            return Ok(DictFormat::WtoolTsv);
        }

        Err(Error::new(format!(
            "{} 认不出是什么词典格式。\nwtool 支持：\n{SUPPORTED}",
            path.display()
        )))
    }

    /// 按这个格式读一个词典文件。
    pub fn load(self, path: &Path) -> Result<BTreeMap<String, String>> {
        match self {
            DictFormat::MtoolJson => mtool::load_index(path),
            DictFormat::WtoolTsv => wtool::read_tsv(path),
            DictFormat::WtoolJson => wtool::read_json(path),
        }
    }

    /// 按这个格式写一个词典文件，返回写出去几条。
    pub fn write(self, path: &Path, map: &BTreeMap<String, String>) -> Result<usize> {
        match self {
            // MTool 的文件是全社区共用的外部契约，我们只读。
            // 真写出去一份「长得像 MTool」的文件，别人拿去用会踩坑。
            DictFormat::MtoolJson => Err(Error::new(format!(
                "MTool 词典是只读格式，wtool 不会往里写。换成 {} 或 {} 再试。",
                DictFormat::WtoolTsv.name(),
                DictFormat::WtoolJson.name()
            ))),
            DictFormat::WtoolTsv => wtool::write_tsv(path, map),
            DictFormat::WtoolJson => wtool::write_json(path, map),
        }
    }
}

/// 正文里有没有「一条正经词条」的样子：非注释、非空、且带 Tab。
fn has_entry_line(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_end_matches('\r');
        !l.is_empty() && !l.starts_with('#') && l.contains('\t')
    })
}

/// 读整个文件。读不到时把路径带上 —— 用户填错路径是常事。
pub(crate) fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::new(format!("读不了 {}：{e}", path.display())))
}

/// 去掉 UTF-8 BOM。Windows 上的编辑器爱加这东西，留着会让首字符判成 `\u{feff}`。
pub(crate) fn strip_bom(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes[3..].to_vec()
    } else {
        bytes
    }
}

/// 按 UTF-8 解码，失败就说是哪个文件 —— 这一层只处理「不是文本」，不猜格式。
pub(crate) fn decode(path: &Path, bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| {
        Error::new(format!(
            "{} 不是 UTF-8 文本，认不出是什么词典格式。\nwtool 支持：\n{SUPPORTED}",
            path.display()
        ))
    })
}

/// 读一个文本词典文件（BOM 已去、已确认是 UTF-8）。
pub(crate) fn read_text(path: &Path) -> Result<String> {
    decode(path, strip_bom(read_bytes(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一次性的临时文件。项目一贯不新增 crate（连 `link.rs` 都只用 std），
    /// 测试也不为这点事去引 `tempfile` —— 和 `bridge.rs` 里的 `scratch` 一个路子。
    fn scratch(tag: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("wtool_fmt_{tag}_{}.dict", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn sniff_recognizes_each_format() {
        let mtool = scratch("mtool", "{\"アイテム\": \"道具\"}");
        assert_eq!(DictFormat::sniff(&mtool).unwrap(), DictFormat::MtoolJson);

        let wjson = scratch("wjson", "{\"wtool\": 1, \"entries\": {\"アイテム\": \"道具\"}}");
        assert_eq!(DictFormat::sniff(&wjson).unwrap(), DictFormat::WtoolJson);

        let tsv = scratch("tsv", "#wtool-tsv 1\nアイテム\t道具\n");
        assert_eq!(DictFormat::sniff(&tsv).unwrap(), DictFormat::WtoolTsv);
    }

    /// BOM 不该把首字符判成 `\u{feff}`，进而认不出 JSON。
    #[test]
    fn sniff_skips_utf8_bom() {
        let path = scratch("bom", "\u{feff}{\"アイテム\": \"道具\"}");
        assert_eq!(DictFormat::sniff(&path).unwrap(), DictFormat::MtoolJson);
    }

    /// 加密的 MTool 文件要报「只能读明文」，不能报「不是文本」。
    #[test]
    fn sniff_reports_encrypted_mtool_clearly() {
        let path = std::env::temp_dir().join(format!("wtool_fmt_enc_{}.dict", std::process::id()));
        std::fs::write(&path, [0x99, 0x99, 0x99, 0x91, 0x00, 0x01]).unwrap();

        let msg = DictFormat::sniff(&path).unwrap_err().message;
        assert!(msg.contains("加密"), "得说清楚是加密文件：{msg}");
    }

    /// 认不出来的东西要报「不认识」并列出支持的格式。
    #[test]
    fn sniff_rejects_unknown_with_format_list() {
        let path = scratch("junk", "这不是词典，只是一段普通文字。\n");
        let msg = DictFormat::sniff(&path).unwrap_err().message;
        assert!(msg.contains("认不出"), "{msg}");
        assert!(msg.contains("MTool 词典"), "错误信息里要列支持哪些格式：{msg}");
    }

    /// MTool 格式只读不写 —— 别让 wtool 产出一份冒充 MTool 的文件。
    #[test]
    fn mtool_format_is_read_only() {
        let path = std::env::temp_dir().join("wtool_fmt_never_written.json");
        let map = BTreeMap::new();
        assert!(DictFormat::MtoolJson.write(&path, &map).is_err());
        assert!(!path.exists(), "报错了就不该真的写出去");
    }
}