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

/// 游戏里「一段对话」，由引擎适配层从游戏进程里读出来。
///
/// 粒度刻意取成「整段」而不是「一行」：词典的 key 就是整段
/// （多条 401 指令用 `\n` 拼起来的那一团），按行查表会全部落空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dialogue {
    /// 对话框序号，游戏侧单调递增。回写译文时用它确认「写的还是那一段」。
    pub seq: u64,
    /// 摊平后的显示行。`$gameMessage._texts` 的每条自身还可能含 `\n`。
    pub lines: Vec<String>,
    /// `lines` 用 `\n` 拼回去，也就是查词典用的那个 key。
    pub text: String,
}
