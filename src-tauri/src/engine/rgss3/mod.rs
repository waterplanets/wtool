//! RGSS3（VX Ace）这一支：**注入方式**走 NWPatch（见 [`patch`] 与 [`live`]），
//! 不涉及 DLL 注入。
//!
//! - [`patch`]（一次性）：把词典与字体烤进游戏目录，装完即终态；
//! - [`live`]（实时）：同一份钩子源码 + `LIVE=true`，wtool 改文件、钩子轮询热重载。
//!
//! 注意这里只有「怎么把钩子架进去」，没有 `Engine` 实现 —— wtool 的实时翻译是
//! 主机侧查词的架构：游戏侧（`Patch/wtool_hook.rb`）自己读词典、自己换字，
//! 不需要 wtool 起监听、也不需要 `Engine::poll` 那套事件流。

pub mod live;
pub mod patch;