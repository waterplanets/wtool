//! RGSS3（VX Ace）这一支：**注入方式**走 NWPatch（见 [`patch`]），不涉及 DLL 注入。
//!
//! 注意这里只有「怎么把钩子架进去」，没有 `Engine` 实现 —— wtool 的实时翻译是
//! 主机侧查词的架构：游戏侧（`Patch/wtool_hook.rb`）自己读词典、自己换字，
//! 不需要 wtool 起监听、也不需要 `Engine::poll` 那套事件流。

pub mod patch;