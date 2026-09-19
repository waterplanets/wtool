//! 注入方式：怎么把翻译通道架进游戏进程。
//!
//! 和 `Engine` 是两个维度：
//!
//! - `Engine` 决定「文本存在哪个对象里、怎么写回」，看的是**引擎**；
//! - `Mechanism` 决定「怎么把我们的代码塞进游戏」，看的是**运行时**
//!   （见 `Runtime`）——比如写插件目录这条路，要求引擎自己有插件机制、
//!   而且跑在能执行 JS 的运行时上，缺一不可。
//!
//! 一台运行时上能跑多个引擎（NW.js 上既有 MV、MZ，也有 TyranoBuilder），
//! 所以将来接第二个共用运行时的引擎时，机制代码可以原样复用，只需要写新的
//! `Engine` 接线 —— 这就是把这一层单拎出来的收益。
//!
//! 现在只有一条路：RPG Maker 的 JS 插件桥（`mz::bridge`）。

use std::path::Path;

use serde::Serialize;

use crate::engine::mz::bridge::JsPluginBridge;
use crate::engine::{EngineType, Runtime};
use crate::error::Result;

/// 一条注入方式「现在处在哪一步」。
///
/// 刻意没有「已经连上、正在工作」这一档：那是运行时连接状态，只有
/// `TcpLink` 知道，磁盘上看不出来，不归这一层管。
/// 它由 `Report` 的调用方（看着 `is_attached()`）自己合成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// 这条路在这个游戏上不成立（比如根本没有 `js/plugins` 目录）。
    NotApplicable,
    /// 能做，还没做。
    Ready,
    /// 装好了，等游戏连上来。
    Installed,
    /// 装过，但和我们现在的版本对不上：文件被改过，或者注册表里被关掉了。
    Stale,
}

/// 撤销能做到什么程度 —— 决定「取消注入」这个按钮对用户到底意味着什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    /// 运行时断开即恢复（CDP 那种，游戏盘上一根汗毛都没动）。
    RuntimeFull,
    /// 运行时禁用 + 离线恢复备份（插件桥，现在这条）。
    RuntimeAndOffline,
    /// 只能离线恢复备份（把译文烤进游戏数据文件那种）。
    OfflineOnly,
    /// 只能重启游戏（内存 hook 那种）。
    None,
}

/// 一次探测的结果。
pub struct Report {
    pub state: State,
    /// 一句给人看的话，直接显示在界面上和日志里。
    pub detail: String,
}

/// 一种注入方式。
pub trait Mechanism {
    /// 稳定的机器名，前端按它调「取消注入」。
    fn id(&self) -> &'static str;
    /// 给人看的名字。
    fn name(&self) -> &'static str;
    /// 只看磁盘就能判断的状态。**失败不许 panic**：探测是要展示给用户看的，
    /// 目录填错、文件没权限都属于「探测结果」，不是程序的异常。
    fn probe(&self, game_dir: &Path) -> Report;
    fn install(&self, game_dir: &Path) -> Result<Vec<String>>;
    fn uninstall(&self, game_dir: &Path) -> Result<Vec<String>>;
    fn reversibility(&self) -> Reversibility;
    /// 装完要不要重启游戏才生效。
    fn restart_required(&self) -> bool;
}

/// 给前端的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct MethodStatus {
    pub id: String,
    pub name: String,
    pub state: State,
    pub detail: String,
    pub reversible: Reversibility,
    pub restart_required: bool,
}

/// 按引擎给出候选注入方式，顺序即优先级。
///
/// 分叉走 `kind.runtime()` 而不是 `matches!(kind, EngineType::Mz | EngineType::Mv)`：
/// 「能不能执行 JS」是运行时的属性，不是引擎的。写成引擎分支的话，
/// 下次接一个同样跑在 NW.js 上的引擎就会漏掉。
///
/// 光有「能执行 JS」还不够 —— 写插件目录这条路的另一个前提是引擎自己得有
/// 插件机制。所以这里将来会长成「运行时 × 引擎」的一张稀疏矩阵：有些格子
/// 永远只有一个顺位（比如 Unity 的内存 hook 只能配 Unity），那很正常。
pub fn mechanisms(kind: EngineType) -> Vec<Box<dyn Mechanism>> {
    match kind.runtime() {
        Runtime::NwJs => vec![Box::new(JsPluginBridge)],
        Runtime::Unknown => Vec::new(),
    }
}

/// 探测所有候选方式，组装成给前端的状态快照。
pub fn status(kind: EngineType, game_dir: &Path) -> Vec<MethodStatus> {
    mechanisms(kind)
        .into_iter()
        .map(|m| {
            let report = m.probe(game_dir);
            MethodStatus {
                id: m.id().to_string(),
                name: m.name().to_string(),
                state: report.state,
                detail: report.detail,
                reversible: m.reversibility(),
                restart_required: m.restart_required(),
            }
        })
        .collect()
}

/// 按 id 找回一条方式，用于「取消注入」。
pub fn find(kind: EngineType, id: &str) -> Option<Box<dyn Mechanism>> {
    mechanisms(kind).into_iter().find(|m| m.id() == id)
}