//! 前端（Vue）与 Rust 之间的唯一通道。
//!
//! 约定：这里只做「参数搬运 + 错误转换」，不放业务逻辑。
//! 每个 `#[tauri::command]` 都是一个可以被前端 `invoke(...)` 调用的函数。
//!
//! 配置只有一份（[`crate::config::AppConfig`]）：前端进来先 `load_config` 填表单，
//! 改完 `set_config` 存回磁盘并推给任务层，之后所有「开始……」命令都不再带参数
//! —— 这一个约定，把「三个任务各要一份配置」收敛成一处。

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

use crate::config::AppConfig;
use crate::corpus::{self, CorpusFormat};
use crate::dict::{self, DictFormat};
use crate::engine::inject::{self, MethodStatus};
use crate::engine::{get_engine_type, resolve_game_dir, EngineType};
use crate::error::{Error, Result};
use crate::model::TsSentence;
use crate::sysfont;
use crate::task;

/// 当前活着的后台任务。None = 什么都没跑。
///
/// `Mutex::new` 是 const fn，所以能直接当 static 用，不用 OnceLock。
static CURRENT: Mutex<Option<task::Handle>> = Mutex::new(None);

/// 这一次运行里用户碰过的游戏目录。
///
/// **临时注入**的东西（MV/MZ 的插件桥）在 wtool 退出时要还原，靠的就是这个。
/// 持久注入（RGSS3）不在此列：它装完就是终态，`cleanup_on_exit` 会跳过。
///
/// 这里不做持久化：强杀 wtool 会留下文件，那种情况交给界面上的「取消注入」手动收尾。
static LAST_GAME_DIR: Mutex<String> = Mutex::new(String::new());

/// 记下这次运行碰过的游戏目录，退出时靠它收尾。
fn remember_game_dir(game_dir: &str) {
    *LAST_GAME_DIR.lock().unwrap() = game_dir.to_string();
}

/// 退出前把注入的东西撤掉，让游戏目录恢复原样。
///
/// 只撤「非持久、且真的装着」的那些（`Installed` / `Stale`）—— 没装过的去调
/// `uninstall` 会被它拒掉（它会报「没装」），那种报错在这儿没有任何意义。
/// **持久注入（RGSS3）跳过**：它装完就是终态，游戏照常翻译，不该被 wtool 退出带走。
/// 撤销失败也不上报：这是退出路径，出错没地方给人看；用户下次进来还能手动
/// 「取消注入」兜底。
pub fn cleanup_on_exit() {
    let game_dir = LAST_GAME_DIR.lock().unwrap().clone();
    if game_dir.trim().is_empty() {
        return;
    }
    let dir = resolve_game_dir(&game_dir);
    for method in inject::mechanisms(get_engine_type(&dir)) {
        if method.persistent() {
            continue;
        }
        let state = method.probe(&dir).state;
        if matches!(state, inject::State::Installed | inject::State::Stale) {
            let _ = method.uninstall(&dir);
        }
    }
}

//-------------------------------------------------------------------------
// 配置：读 / 写
//-------------------------------------------------------------------------

/// 读配置（`%APPDATA%\wtool\config.json`）。前端开界面先调它填表单。
///
/// 文件不在、读不动、JSON 坏了都退回默认值而不报错 —— 配置只是「上次填过什么」
/// 的记忆，坏了不该挡住用户干活。
#[tauri::command]
pub fn load_config() -> AppConfig {
    AppConfig::load()
}

/// 存配置：收拾干净 → 落盘 → 推给任务层。返回收拾后的那份，前端据此刷新表单
/// （用户填的引号之类会被去掉，界面得跟着变）。
///
/// 一个命令管三件事，是故意的：这三件事必须一起发生，否则「界面显示的配置」
/// 「磁盘上的配置」「任务真在用的配置」就会各说各话。
#[tauri::command]
pub fn set_config(mut cfg: AppConfig) -> Result<AppConfig> {
    cfg.normalize();
    cfg.save()?;
    if !cfg.game_dir.is_empty() {
        remember_game_dir(&cfg.game_dir);
    }
    task::set_config(task::Config::from_app(&cfg, false));
    Ok(cfg)
}

//-------------------------------------------------------------------------
// 任务：开始 / 停止
//-------------------------------------------------------------------------

/// 一次「开始翻译」的结果，告诉前端接下来该怎么操作。
///
/// 这就是「翻译操作统一接口」的那句话：前端不问引擎，只问这几个能力位。
#[derive(Debug, Serialize)]
pub struct StartOutcome {
    /// 要不要保持 wtool 开着。true = 界面得停在「正在翻译」状态别关；
    /// false = 装完就完了，用户可以直接关掉 wtool 去玩游戏（RGSS3）。
    pub server_running: bool,
    /// 这次安装会不会留在游戏目录（RGSS3 会，退出也不还原）。
    pub persistent: bool,
    /// 用上的注入方式名，日志/提示里带上。
    pub method: String,
}

/// 统一翻译入口：按引擎挑注入方式 → 装上 → 需要服务的起服务。
///
/// 全靠 [`inject::pick`] 先预判一次，把「这次要不要保持 wtool 开着」提前告诉
/// 前端；真正的安装与服务由后台任务自己走一遍（同一套优先级，不会打架）。
///
/// MV/MZ：桥走内存通道，`server_running = true`，界面须停在「正在翻译」。
/// RGSS3：钩子自带词典、进程内查表，任务装完即返回，`server_running = false`。
#[tauri::command]
pub fn start_translation(app: tauri::AppHandle) -> Result<StartOutcome> {
    let outcome = pick_outcome()?;
    if !outcome.server_running {
        // 零服务的机制（RGSS3）由任务自己装完就收工，不需要预先把词典路径校验
        // 到「能读」那么细 —— 但游戏目录总得填。
        let cfg = task::config();
        if cfg.game_dir.trim().is_empty() {
            return Err(Error::new("还没填游戏目录"));
        }
    }
    *CURRENT.lock().unwrap() = Some(task::trainning(&app));
    Ok(outcome)
}

/// 在线采集：起后台线程，让玩家正常玩一遍攒语料。带着 `server_running` 一起回，
/// 因为采集**必须**有服务（文本是游戏侧推上来的）。
#[tauri::command]
pub fn start_collect(app: tauri::AppHandle) -> Result<StartOutcome> {
    let outcome = pick_outcome()?;
    if !outcome.server_running {
        return Err(Error::new(
            "这个引擎的钩子不向 wtool 回报文本，在线采集不适用。请改用「离线扫描」收语料。",
        ));
    }
    *CURRENT.lock().unwrap() = Some(task::trainning_collect(&app)?);
    Ok(outcome)
}

/// 离线翻译：把语料翻成词典。起后台线程，进度经 `trans-status` 推给前端。
#[tauri::command]
pub fn start_translate_file(app: tauri::AppHandle) -> Result<()> {
    *CURRENT.lock().unwrap() = Some(task::trainning_translate(&app)?);
    Ok(())
}

/// 停掉后台任务。三个任务一套停止语义：把句柄置空，Sender 随之 drop，
/// 线程那边 recv 返回 Disconnected，任务自己退出。
#[tauri::command]
pub fn stop_trans() -> bool {
    *CURRENT.lock().unwrap() = None;
    true
}

/// 预判这次「开始」会不会起常驻服务。挑不出注入方式就当场报错，
/// 免得用户对着一个必然失败的任务干等。
fn pick_outcome() -> Result<StartOutcome> {
    let cfg = task::config();
    let dir = resolve_game_dir(&cfg.game_dir);
    if cfg.game_dir.trim().is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }
    let kind = get_engine_type(&dir);
    if kind == EngineType::Unknown {
        return Err(Error::new(
            "这个目录既不像 MZ 也不像 MV，游戏目录是不是填错了？",
        ));
    }
    let method = inject::pick(kind, &dir).ok_or_else(|| {
        Error::new(format!(
            "{}（{}）上还没有可用的注入方式",
            kind.name(),
            kind.runtime().name()
        ))
    })?;
    Ok(StartOutcome {
        server_running: method.needs_server(),
        persistent: method.persistent(),
        method: method.name().to_string(),
    })
}

//-------------------------------------------------------------------------
// 语料 / 词典
//-------------------------------------------------------------------------

/// 离线扫描：不开游戏，直接扫游戏目录收语料，返回一句给人看的话。
/// 同步执行，不过 Tauri 命令跑在 worker 线程上，界面不会冻。
#[tauri::command]
pub fn collect_offline(game_dir: String, out: String, format: String) -> Result<String> {
    let out = crate::engine::clean_path(&out);
    task::collect_offline(
        &game_dir,
        Path::new(&out),
        CorpusFormat::parse(&format),
    )
}

/// 语料「另存为 / 换格式」：读进来按目标格式再写一份。
///
/// 和翻译无关，纯搬运 —— 但它是「读到的语料想换个格式」这件事的唯一出口，
/// 免得用户为了转个格式去重扫一遍游戏（RGSS3 那个要读 975MB 归档）。
#[tauri::command]
pub fn convert_corpus(src: String, dst: String, format: String) -> Result<String> {
    let src = crate::engine::clean_path(&src);
    let dst = crate::engine::clean_path(&dst);
    let corpus = corpus::load(Path::new(&src))?;
    let fmt = CorpusFormat::parse(&format);
    let n = corpus::save(Path::new(&dst), &corpus, fmt)?;
    Ok(format!(
        "{} 条已转存到 {}（{}）",
        n,
        dst,
        fmt.name()
    ))
}

/// 词典「另存为 / 换格式」：读进来按目标格式再写一份。
///
/// 输入可以是 MTool 词典（只读）—— 「把它转成 wtool 能续写的格式」正是
/// 用户最常需要的一步。输出只可能是能写的两种，[`DictFormat::parse_out`] 保证。
#[tauri::command]
pub fn convert_dict(src: String, dst: String, format: String) -> Result<String> {
    let src = crate::engine::clean_path(&src);
    let dst = crate::engine::clean_path(&dst);
    let src = Path::new(&src);
    let map = DictFormat::sniff(src)?.load(src)?;
    let out_fmt = DictFormat::parse_out(&format);
    let n = out_fmt.write(Path::new(&dst), &map)?;
    Ok(format!(
        "{n} 条已转存到 {}（{}）",
        dst,
        out_fmt.name()
    ))
}

/// 读一份词典文件，把词条列表交给前端。
///
/// 格式不用前端先选：MTool 的 JSON 和 wtool 自己的两种格式都认，认不出会报错。
#[tauri::command]
pub fn load_dictionary(path: &str) -> Result<Vec<TsSentence>> {
    dict::load_ts_file(Path::new(&crate::engine::clean_path(path)))
}

//-------------------------------------------------------------------------
// 注入
//-------------------------------------------------------------------------

/// 猜这个目录是什么引擎、跑在什么运行时上，给界面显示。
///
/// 单开一个口子是为了让「认不出目录」这个症状自证：界面上写「未知引擎」，
/// 就说明填的路径不是游戏根目录（或者路径不对），而不是这个游戏真没有
/// 可用的注入方式 —— 否则两种情况的界面表现一模一样，没法查。
#[derive(Debug, Serialize)]
pub struct EngineInfo {
    pub name: String,
    pub runtime: String,
    /// 认出来了没有。`false` 时前端该提示「检查游戏目录」。
    pub known: bool,
}

/// 引擎探测结果，给界面显示。
#[tauri::command]
pub fn detect_engine(game_dir: &str) -> EngineInfo {
    let kind = get_engine_type(&resolve_game_dir(game_dir));
    EngineInfo {
        name: kind.name().to_string(),
        runtime: kind.runtime().name().to_string(),
        known: kind != EngineType::Unknown,
    }
}

/// 这个游戏支持哪些注入方式、各自现在什么状态。
///
/// 引擎是现猜的，不用用户选 —— 和「开始翻译」走的是同一个 `get_engine_type`，
/// 所以界面上看到的和真跑起来的一定一致。认不出引擎就返回空列表，
/// 由界面去说「这个引擎还没有可用的注入方式」。
#[tauri::command]
pub fn list_injections(game_dir: &str) -> Vec<MethodStatus> {
    let dir = resolve_game_dir(game_dir);
    if !dir.as_os_str().is_empty() {
        remember_game_dir(&dir.to_string_lossy());
    }
    inject::status(get_engine_type(&dir), &dir)
}

/// 单独装某个注入方式（不含翻译任务）。给「先装好、稍后自己开游戏 / 开任务」
/// 这种用法留的口子；平常走 [`start_translation`] 就够了。
#[tauri::command]
pub fn install_injection(
    game_dir: &str,
    id: &str,
    dict_path: &str,
    font: &str,
) -> Result<Vec<String>> {
    remember_game_dir(game_dir);
    let dir = resolve_game_dir(game_dir);
    let method = inject::find(get_engine_type(&dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.install(&dir, Path::new(&crate::engine::clean_path(dict_path)), font)
}

/// 撤销某个注入方式，把游戏目录恢复原样。
///
/// 不动后台任务：停任务是 `stop_trans` 的事，前端按 `stop_trans` → `uninstall_injection`
/// 的顺序调两次就行。这里保持「只搬运参数 + 转换错误」。
#[tauri::command]
pub fn uninstall_injection(game_dir: &str, id: &str) -> Result<Vec<String>> {
    remember_game_dir(game_dir);
    let dir = resolve_game_dir(game_dir);
    let method = inject::find(get_engine_type(&dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.uninstall(&dir)
}

//-------------------------------------------------------------------------
// 杂项
//-------------------------------------------------------------------------

/// 写一个「常驻主机」启动脚本到游戏目录（`wtool-host.cmd`）。
///
/// 给必须靠服务才能翻译的引擎（MV/MZ）用：用户不想一直开着 wtool 界面时，
/// 双击这个脚本就能把主机跑起来（无窗口模式，日志落 `%APPDATA%\wtool\host.log`）。
///
/// **脚本里不写 API Key**：key 在 `%APPDATA%\wtool\config.json` 里，脚本引用不到
/// 也不需要引用 —— 所以脚本可以随便放、随便拷。
#[tauri::command]
pub fn generate_launcher(game_dir: &str) -> Result<String> {
    let dir = resolve_game_dir(game_dir);
    if dir.as_os_str().is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }

    let exe = std::env::current_exe()
        .map_err(|e| Error::new(format!("拿不到 wtool 自己的路径：{e}")))?;

    // 一行注释说清楚这脚本是什么、从哪来、怎么停。
    let script = format!(
        "@echo off\r\n\
         rem wtool 常驻翻译主机（由 wtool 生成）。\r\n\
         rem 需要 wtool 保持运行：游戏里的桥会把文本推给它。关掉这个窗口即停。\r\n\
         rem 配置与 API Key 读 %APPDATA%\\wtool\\config.json，改设置请回 wtool 界面。\r\n\
         rem 日志：%APPDATA%\\wtool\\host.log\r\n\
         \"{}\" --host --game \"%~dp0.\"\r\n",
        exe.display()
    );

    let path = dir.join("wtool-host.cmd");
    std::fs::write(&path, script)?;
    Ok(format!("启动脚本已写到 {}", path.display()))
}

/// 删掉游戏目录里的启动脚本（`wtool-host.cmd`）。
///
/// 只删我们自己写的那一个文件，别的一概不碰。本来就不在时报出来而不是当成功：
/// 用户点了删除却没反应，比一句明确的「本来就没有」更难排查。
#[tauri::command]
pub fn remove_launcher(game_dir: &str) -> Result<String> {
    let dir = resolve_game_dir(game_dir);
    if dir.as_os_str().is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }

    let path = dir.join("wtool-host.cmd");
    if !path.is_file() {
        return Err(Error::new(format!(
            "{} 不在 —— 可能已经删过了",
            path.display()
        )));
    }

    std::fs::remove_file(&path)?;
    Ok(format!("启动脚本已删除：{}", path.display()))
}

/// 系统里装了哪些字体，给界面上的字体菜单用。
///
/// 前端拿不到系统字体列表（WebView 里没有这个能力），只能这边枚举了递过去。
#[tauri::command]
pub fn list_system_fonts() -> Vec<String> {
    sysfont::list()
}