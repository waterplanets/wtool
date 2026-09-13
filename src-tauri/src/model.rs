//! 内部数据契约的唯一真源。
//!
//! 规则：外部格式（MTool / 其它引擎的词典）一律先转成这里的类型，
//! 才允许在工程里传递。前端拿到的也是这些结构的 JSON 形态。

use serde::{Deserialize, Serialize};

/// 一条待翻译/已翻译词条。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsSentence {
    /// 原文（词典 key）。实测可能含 `\n`，也可能是 `"20"` 这类脏 key。
    pub source: String,
    /// 译文（词典 value）。实测存在空字符串，表示"未翻译"而非"翻译为空"。
    pub target: String,
}
