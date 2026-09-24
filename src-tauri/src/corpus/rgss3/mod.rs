//! RGSS3（RPG Maker VX Ace）的数据格式知识。
//!
//! 这里装的是**格式**：`Game.rgss3a` 怎么解、Ruby Marshal 怎么走、哪些文件
//! 里有给玩家看的字。引擎类型的判断**不在这** —— 那是 `engine` 的事，
//! 「哪个游戏走哪条抽取路线」的分派写在调用方 `task::collect_offline`。
//! 放在 corpus 下是为了和 MV/MZ 的 JSON 抽取（[`crate::corpus::offline`]）对称：
//! 两边产物都是 [`crate::corpus::Corpus`]，共用粒度、过滤和落盘格式。
//!
//! ```text
//!   archive.rs   Game.rgss3a 读取器 + 松散 Data/ 目录，统一成「按名字读文件」
//!   rvdata2.rs   Ruby Marshal 遍历器：把对象树里的字符串连同路径挖出来
//!   scripts.rs   Scripts.rvdata2：解 zlib、再从 Ruby 源码里抠字符串字面量
//!   scan.rs      抽取入口：扫哪些文件、每条按什么粒度收
//! ```

pub mod archive;
pub mod rvdata2;
pub mod scan;
pub mod scripts;

pub use scan::scan;