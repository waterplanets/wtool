//! 引擎适配层：把「某一个游戏引擎」抽象成「能读原文、能写译文」的一个东西。
//!
//! 分工：
//!
//! ```text
//!   业务层（翻译循环）
//!        │  只认 Engine，换引擎不动这里
//!        ▼
//!   engine::Engine            ← 本文件，引擎无关的语义
//!        │
//!        ├── link            传输：TCP（现在）/  CDP、DLL（以后）
//!        └── mz              词表：RPG Maker MZ（MZ 与 MV 的消息系统同名，一起覆盖）
//!
//!   engine::inject           注入方式：怎么把通道架进游戏进程
//!        └── mz::bridge      JS 插件桥 —— 现在唯一的一条路
//! ```
//!
//! 换传输（TCP → CDP）不动引擎代码，换引擎（MZ → 别的）不动传输代码，
//! 这是把 `Engine` 和 `Link` 拆成两个 trait 的全部理由。
//!
//! 注入方式再单拎出来，是因为它和引擎是两个维度：引擎决定「文本存在哪个
//! 对象里」，注入方式决定「怎么把代码塞进去」，而后者取决于**运行时**
//! （见 [`Runtime`]），不取决于引擎的名字。

pub mod inject;
pub mod link;
pub mod mz;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::Dialogue;

/// 游戏跑在什么运行时上 —— 也就是这个引擎被封装成了什么样子。
///
/// 和 [`EngineType`] 是两个维度：引擎是「这套游戏怎么组织文本」，
/// 运行时是「代码在什么环境里执行、我们能不能把代码塞进去」。
///
/// 一台运行时上能跑多个引擎（NW.js 上既有 MV、MZ，也有 TyranoBuilder），
/// 同一个引擎也能换封装（MV 官方是 NW.js，也有被包成 Electron 的）。
/// 所以「注入方式能不能用」要问运行时，而不是问引擎 —— 见 [`inject::mechanisms`]。
///
/// 在 wtool 现在支持的范围内，这两个维度恰好一一对应，所以单独列出这个枚举
/// 还换不来行为差异。但适用性判断统一走 [`EngineType::runtime`]，将来接
/// 第二个共用运行时的引擎时，把这里从函数升成真正的字段就行，
/// 不用回头去拆散落各处的 `matches!(kind, ...)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    /// NW.js：RPG Maker MV/MZ 的官方封装，带 Node 集成（游戏侧的桥能 `require('net')`）。
    NwJs,
    /// 没认出来。
    Unknown,
}

impl Runtime {
    /// 给人看的名字，用于日志和前端。
    pub fn name(self) -> &'static str {
        match self {
            Runtime::NwJs => "NW.js",
            Runtime::Unknown => "未知运行时",
        }
    }
}

/// 认出来的游戏引擎。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineType {
    /// RPG Maker MZ：`js/rmmz_core.js`
    Mz,
    /// RPG Maker MV：`www/js/rpg_core.js`
    Mv,
    /// 没认出来。
    Unknown,
}

impl EngineType {
    /// 给人看的名字，用于日志和前端。
    pub fn name(self) -> &'static str {
        match self {
            EngineType::Mz => "RPG Maker MZ",
            EngineType::Mv => "RPG Maker MV",
            EngineType::Unknown => "未知引擎",
        }
    }

    /// 这个引擎跑在什么运行时上。
    ///
    /// **机制的适用性判断一律走这里**，别写 `matches!(kind, EngineType::Mz | EngineType::Mv)`
    /// —— 那是把运行时的知识塞进引擎分支里，下次加引擎就会漏。
    pub fn runtime(self) -> Runtime {
        match self {
            EngineType::Mz | EngineType::Mv => Runtime::NwJs,
            EngineType::Unknown => Runtime::Unknown,
        }
    }
}

/// 把用户填的那串路径收拾成一个游戏目录。
///
/// 用户是从文件管理器或别处复制粘贴过来的，粘进来什么样都有可能：Windows
/// 复制路径默认带一对引号，前后可能还夹着空格，也可能干脆指到了
/// `Game.exe` 而不是它所在的目录。这些都不该让下游每个 `is_file()` 去猜，
/// 在入口处一次性收拾干净。
///
/// 之所以能容忍「指到 exe」：判据只看目录结构（见 [`get_engine_type`]），
/// 多退回一层父目录没有任何歧义 —— 游戏的可执行文件就躺在游戏根目录里。
pub fn resolve_game_dir(raw: &str) -> PathBuf {
    let trimmed = raw.trim().trim_matches('"').trim();
    let p = Path::new(trimmed);
    // 指到文件（多半是 Game.exe）时，改用它所在的目录。
    if p.is_file() {
        if let Some(parent) = p.parent() {
            return parent.to_path_buf();
        }
    }
    p.to_path_buf()
}

/// 从一个目录猜游戏引擎——点「开始翻译」后自动走这一步，用户不用选。
///
/// 判据只看引擎自带的脚本文件名，不看 `Game.exe`、不查注册表：
/// 这两个文件名是引擎自己写死的，最不容易认错，代价也只是两个 `is_file()`。
/// MV 把整个游戏塞在 `www/` 下，MZ 直接铺在根目录，所以两个根都试一遍。
pub fn get_engine_type(game_dir: &Path) -> EngineType {
    for root in [game_dir.to_path_buf(), game_dir.join("www")] {
        if root.join("js").join("rmmz_core.js").is_file() {
            return EngineType::Mz;
        }
        if root.join("js").join("rpg_core.js").is_file() {
            return EngineType::Mv;
        }
    }
    EngineType::Unknown
}

/// 游戏推上来的一条事件。
#[derive(Debug, Clone)]
pub enum Event {
    /// 游戏内的 JS 连上来了（或者断线后重新连上）。
    Attached,
    /// 出现了一段新对话，`seq` 是它的编号。
    Text(Dialogue),
    /// 那段对话结束了。
    Closed { seq: u64 },
    /// 游戏送来一批要整批翻译的原文：菜单词条（`System.json` 的 `terms`），
    /// 或者数据库里的名字与说明（`$dataItems` / `$dataSkills` / …）。
    ///
    /// 这类文字游戏侧都等不了「上报 → 查表 → 回写」这一趟往返（菜单是渲染时
    /// 同步取字的），所以开机就把整批问过来，拿到译文直接在内存里换掉。
    /// 一次性事件，见过一次就不再来了（除非游戏重开或者重连）。
    ///
    /// `loose` 说的是 wtool 这边该用哪种查法：
    ///
    /// - `false`：菜单词条，走精确查。词条本身就是词典的 key 粒度，整条查不中
    ///   就是没有；退到片段反而会把 `ーーーー` 这类分隔符翻出鬼来。
    /// - `true`：物品/技能的名字与说明，走 `translate` 的三级退级。说明里混着
    ///   `\n` 和控制码，词典里既有整段的 key 也有只盖住半句的片段 key，
    ///   不退级就翻不全。
    ///
    /// `label` 只是日志里那批文本的名字。开机要问四批（菜单词条、数据库文本、
    /// 事件文本、独特数据），不带个名字，日志上就只剩一串数不清的条数。
    Terms {
        loose: bool,
        label: String,
        texts: Vec<String>,
    },
}

/// 一个可读可写的游戏。
///
/// 刻意做成「拉取式」而不是「回调式」：`poll` 非阻塞，由业务层的循环
/// 按自己的节奏调用。好处是 TCP 这种「游戏主动推」的传输，
/// 和 CDP 那种「wtool 主动问」的传输，都能塞进同一个形状里。
pub trait Engine: Send {
    /// 引擎名，给日志和前端看。
    fn name(&self) -> &'static str;

    /// 非阻塞地取一条事件。`Ok(None)` 只表示「此刻没有」，不是出错。
    fn poll(&mut self) -> Result<Option<Event>>;

    /// 写译文回去，作用于最近一条 `Text` 事件对应的那一段对话。
    ///
    /// 这是「写访问接口」的入口：往游戏进程里改文本，全工程只走这一个口子。
    fn apply(&mut self, target: &str) -> Result<()>;

    /// 写一批整表译文回去，对应最近一条 `Terms` 事件。
    ///
    /// `map` 是「原文 → 译文」，**只放查得到的**：查不到的词条游戏侧原样留着，
    /// 既不用发空串，也不用发原文，省一趟还少一处出错的地方。
    ///
    /// 和 `apply` 分开是因为语义不同：`apply` 写的是「屏幕上正显示的这段话」，
    /// 靠 `seq` 认门；这里写的是「长期有效的一张小表」，和 `seq` 无关。
    fn apply_terms(&mut self, map: &[(String, String)]) -> Result<()>;

    /// 设置游戏主字体。空串 = 恢复游戏原字体（不替换）。
    ///
    /// 独立于词表翻译：它改的是 `Game_System.prototype.mainFontFace`，
    /// 让游戏把主字体整体换成用户选的系统中文字体，避免自带的日文字体
    /// 在简体字的码位上空有占位、画出来一片空白。业务层在 `Attached`
    /// 事件连上游戏时调用一次，字体跟任务走。
    fn set_font(&mut self, font: &str) -> Result<()>;
}
