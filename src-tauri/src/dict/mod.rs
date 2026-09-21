//! 词典层：把外部词典文件读成一张「原文 → 译文」的表，再交给 [`mtool::Dict`] 建索引。
//!
//! 分三层，各管各的：
//!
//! ```text
//!   format   认格式：这个文件是什么？（MTool / wtool 行式 / wtool JSON）
//!     │
//!     ├── mtool   读 MTool 的 TrsData.json（只读，外部契约）
//!     └── wtool   读 / 写 wtool 自己的两种格式
//!          │
//!          ▼
//!        Dict     只认「已经解析好的表」，不关心它从哪个格式来
//! ```
//!
//! 于是「加一种新格式」只需要动 `format` + 一个新适配器，`Dict` 一个字不用改。

pub mod format;
pub mod mtool;
pub mod wtool;

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::model::TsSentence;

pub use format::DictFormat;

/// 读一个词典文件：先嗅探格式，再交给对应适配器。认不出格式会报错。
///
/// 业务层都走这个口子，不需要自己先判格式 —— 用户填的是路径，不是格式。
pub fn load_map(path: &Path) -> Result<BTreeMap<String, String>> {
    DictFormat::sniff(path)?.load(path)
}

/// 读成一个词条列表，给前端那个「词典预览」用。
///
/// 之所以不直接把 `BTreeMap` 交出去：MTool 的格式是「不由我们定义的外部契约」，
/// 一旦泄漏到上层，将来换格式就要动全工程。这里多做一步转换，把脏东西挡在外面。
pub fn load_ts_file(path: &Path) -> Result<Vec<TsSentence>> {
    Ok(load_map(path)?
        .into_iter()
        .map(|(source, target)| TsSentence { source, target })
        .collect())
}