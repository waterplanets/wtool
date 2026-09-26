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
//! 现在三条路，按「运行时 × 模式」摆开：
//!
//! - RPG Maker 的 JS 插件桥（`mz::bridge`，实时）—— 写 `js/plugins`，跑在 NW.js 上；
//! - RPG Maker 的烤入插件（`mz::baked`，一次性）—— 词典与字体烤进同一份插件；
//! - RGSS3 的文件轮询（`rgss3::live`，实时）与 NWPatch（`rgss3::patch`，一次性）。
//!
//! 「实时 / 一次性」这层是 [`Mode`]，靠什么维持通道是 [`Service`]。

use std::path::Path;

use serde::Serialize;

use crate::engine::mz::baked::JsPluginBaked;
use crate::engine::mz::bridge::JsPluginBridge;
use crate::engine::rgss3::live::Rgss3Live;
use crate::engine::rgss3::patch::Rgss3Patch;
use crate::engine::{EngineType, Runtime};
use crate::error::Result;

/// 翻译通道的两种形态。
///
/// 这不是「哪条路更好」，而是同一套注入的两种用法：
///
/// - [`Mode::Live`]：游戏运行时连着 wtool。词典和字体都在 wtool 手里，
///   改一下就能重下发生效 —— **调字体、试词典时用这条**。代价是 wtool 得开着。
/// - [`Mode::OneTime`]：装的时候把词典和字体烤进游戏侧文件，**装完即终态**，
///   游戏自己查表，wtool 关掉也照翻。给正式玩的时候用。
///
/// 两种形态在三个引擎上都有：MV/MZ 是「JS 桥（TCP）↔ 烤入插件」，
/// RGSS3 是「文件轮询热重载 ↔ 烤入 Patch.rb」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 实时：游戏连着 wtool。
    Live,
    /// 一次性：词典与字体烤进游戏侧，装完即终态。
    OneTime,
}

impl Mode {
    /// 给人看的名字。
    pub fn name(self) -> &'static str {
        match self {
            Mode::Live => "实时连接",
            Mode::OneTime => "一次性注入",
        }
    }

    /// 存进配置、传给前端的稳定字符串。`auto` 不在这里 —— 那是「让引擎自己定」。
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::OneTime => "onetime",
        }
    }

    /// 认 `live` / `onetime`；其余（含 `auto`、空串、拼错）返回 `None`，
    /// 交给 [`resolve_mode`] 按引擎兜底。
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() {
            "live" => Some(Mode::Live),
            "onetime" => Some(Mode::OneTime),
            _ => None,
        }
    }
}

/// 「实时」模式下，通道靠什么维持。
///
/// 单独抽出来是因为 `needs_server` 只是它的一个推论：不靠任何外部通道的方式，
/// 装完就是终态。前端也按它区分「实时」是哪种实时（TCP 还是文件轮询）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    /// 不需要任何服务：装完即终态（所有一次性方式）。
    None,
    /// wtool 起 TCP 服务，游戏连上来（MV/MZ 实时）。
    Tcp,
    /// wtool 往游戏目录写文件，游戏侧轮询热重载（RGSS3 实时）。
    FileSync,
}

impl Service {
    pub fn as_str(self) -> &'static str {
        match self {
            Service::None => "none",
            Service::Tcp => "tcp",
            Service::FileSync => "file_sync",
        }
    }

    /// 需不需要 wtool 常驻。`None` 之外的两种都要。
    pub fn needs_server(self) -> bool {
        self != Service::None
    }
}

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
    /// 这条方式属于实时还是一次性。
    fn mode(&self) -> Mode;
    /// 这条方式靠什么维持通道（决定要不要起常驻服务）。
    fn service(&self) -> Service;
    /// 只看磁盘就能判断的状态。**失败不许 panic**：探测是要展示给用户看的，
    /// 目录填错、文件没权限都属于「探测结果」，不是程序的异常。
    fn probe(&self, game_dir: &Path) -> Report;
    /// 装上这条路。两个参数都是 GUI 上配的：
    ///
    /// - `dict_path`：词典文件。有些方式需要把它转成游戏侧能直接读的形态一并
    ///   投进去（RGSS3 的两条路、MV/MZ 的一次性路都走这条），用不上的方式
    ///   （JS 插件桥的实时模式，词典走内存通道）忽略它即可。
    /// - `font`：目标字体名。同样是「要在装的时候烤进游戏侧文件里」的方式才用
    ///   得上（RGSS3 的钩子读的是磁盘上的 Ruby 源码）；空串 = 不指定，用默认。
    fn install(&self, game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>>;
    fn uninstall(&self, game_dir: &Path) -> Result<Vec<String>>;
    /// 任务跑着的时候热更新字体。
    ///
    /// 只有「实时 + 读文件」的方式用得上（RGSS3 的实时模式：wtool 改写游戏目录
    /// 里那个字体文件，游戏侧轮询到就换字体）—— 这正是实时模式存在的理由：
    /// 调字体不用重装。其余方式不需要，默认什么都不做。
    fn update_font(&self, _game_dir: &Path, _font: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn reversibility(&self) -> Reversibility;
    /// 装完要不要重启游戏才生效。
    fn restart_required(&self) -> bool;
    /// 翻译时需不需要 wtool 常驻的运行时服务。
    ///
    /// 由 [`Mechanism::service`] 推出：一次性方式不需要，实时方式（TCP / 文件
    /// 轮询）都需要。前端按这个字段决定主按钮是「开始翻译（起服务）」还是
    /// 「安装并完成」。
    fn needs_server(&self) -> bool {
        self.service().needs_server()
    }
    /// 安装是否在 wtool 退出后保留。
    ///
    /// 非持久的方式在 wtool 退出时会被 `commands::cleanup_on_exit` 还原，
    /// 免得游戏目录里常驻着用不上的文件、每次开游戏白载一遍。
    fn persistent(&self) -> bool {
        false
    }
}

/// 给前端的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct MethodStatus {
    pub id: String,
    pub name: String,
    /// 实时 / 一次性。
    pub mode: Mode,
    /// 靠什么维持通道（`none` = 装完即终态）。
    pub service: Service,
    pub state: State,
    pub detail: String,
    pub reversible: Reversibility,
    pub restart_required: bool,
    /// 翻译是否需要 wtool 常驻服务。
    pub needs_server: bool,
    /// 安装是否在 wtool 退出后保留。
    pub persistent: bool,
}

/// 按引擎给出候选注入方式，顺序即优先级。
///
/// 分叉走 `kind.runtime()` 而不是 `matches!(kind, EngineType::Mz | EngineType::Mv)`：
/// 「能不能执行 JS」是运行时的属性，不是引擎的。写成引擎分支的话，
/// 下次接一个同样跑在 NW.js 上的引擎就会漏掉。
///
/// 每个引擎给两条：实时在前、一次性在后（同一模式内各只有一条，所以顺序只影响
/// 前端列表的排布）。挑哪条由 [`resolve_mode`] 定的模式说了算，见 [`pick`]。
pub fn mechanisms(kind: EngineType) -> Vec<Box<dyn Mechanism>> {
    match kind.runtime() {
        Runtime::NwJs => vec![Box::new(JsPluginBridge), Box::new(JsPluginBaked)],
        // RGSS3 没有 JS 可执行，「写插件」不成立；但它有 NWPatch —— 游戏启动时
        // 会 `File.read` + `eval` 一份松散的 `Patch/Patch.rb`。这条路是**零注入**
        // 的：不动进程内存、不落 DLL、杀软管不着，删文件即撤销。
        //
        // 实时那条走**文件轮询**：wtool 把词典和字体写进 `Patch/` 下的文件，
        // 钩子每秒比一次 mtime，改了就地重载 —— 调字体不用重装，代价是 wtool
        // 得开着（退出即卸）。一次性那条（`Rgss3Patch`）把词典烤进钩子，装完即终态。
        //
        // 顺带记下另一条路的现状，省得重新摸：MTool 的 `RGSSHook.dll` 那条
        // （`inject.exe Game.exe RGSSHook.dll`，CreateRemoteThread）方向是反的
        // ——DLL 是服务端、监听 42800，wtool 得当客户端连它，线格式还是帧式的
        // `[len][type]` 而非「一行一条 JSON」，和 `link::TcpLink` 的契约不兼容；
        // 而且会被卡巴斯基拦。所以现在走 NWPatch 这条。
        Runtime::Native => vec![Box::new(Rgss3Live), Box::new(Rgss3Patch)],
        Runtime::Unknown => Vec::new(),
    }
}

/// 这次该用哪种模式。
///
/// 配置里 `run_mode` 可能是 `auto` / `live` / `onetime`（见 `config::AppConfig`）。
/// `auto`（以及任何认不出来的值）走「各引擎自己成熟的那条」：MV/MZ 用实时（词典
/// 整份留在 wtool，不往插件里塞几十万条）、RGSS3 用一次性（烤进 Patch，不必让
/// 用户为了翻译一直开着 wtool）。这样默认行为和我们加双模式之前一模一样。
pub fn resolve_mode(run_mode: &str, kind: EngineType) -> Mode {
    if let Some(m) = Mode::parse(run_mode) {
        return m;
    }
    match kind.runtime() {
        Runtime::Native => Mode::OneTime,
        _ => Mode::Live,
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
                mode: m.mode(),
                service: m.service(),
                state: report.state,
                detail: report.detail,
                reversible: m.reversibility(),
                restart_required: m.restart_required(),
                needs_server: m.needs_server(),
                persistent: m.persistent(),
            }
        })
        .collect()
}

/// 按 id 找回一条方式，用于「取消注入」。
pub fn find(kind: EngineType, id: &str) -> Option<Box<dyn Mechanism>> {
    mechanisms(kind).into_iter().find(|m| m.id() == id)
}

/// 这个游戏上指定模式下**第一条可用的**注入方式。
///
/// `task::run` 挑机制时走的就是这个（探一遍、跳过不适用的、用那条能用的）。
/// 单独抽出来是给「开始翻译」那个命令**预判**用：它要在起任务之前就知道这次
/// 会不会起常驻服务，好把结果告诉前端。
pub fn pick(kind: EngineType, game_dir: &Path, mode: Mode) -> Option<Box<dyn Mechanism>> {
    mechanisms(kind)
        .into_iter()
        .filter(|m| m.mode() == mode)
        .find(|m| m.probe(game_dir).state != State::NotApplicable)
}

/// 装一条方式之前，先把**同引擎另一条**卸掉。
///
/// 同一个引擎的两条路会在游戏里重叠挂钩（同一次对话上报两遍、同一段文本查两次
/// 表），必须互斥。这里只卸「真的装着」的（`Installed` / `Stale`），没装过的
/// 去调 `uninstall` 会被它拒掉，那报错在这儿没意义。
///
/// 返回给用户看的几句话（卸了什么、或者卸失败的原因）。**失败不阻断**：卸不掉
/// 就继续装新的，最坏结果是重叠挂钩 —— 那也比「什么都装不上」强，用户还能从
/// 日志里看到原因。
pub fn uninstall_siblings(kind: EngineType, game_dir: &Path, keep_id: &str) -> Vec<String> {
    let mut log = Vec::new();
    for m in mechanisms(kind) {
        if m.id() == keep_id {
            continue;
        }
        if !matches!(m.probe(game_dir).state, State::Installed | State::Stale) {
            continue;
        }
        log.push(format!("先清理另一种方式：{}", m.name()));
        match m.uninstall(game_dir) {
            Ok(lines) => log.extend(lines),
            Err(e) => log.push(format!("清理 {} 没成功（{}），继续安装", m.name(), e.message)),
        }
    }
    log
}