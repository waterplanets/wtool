//! 前端（Vue）与 Rust 之间的唯一通道。
//!
//! 约定：这里只做「参数搬运 + 错误转换」，不放业务逻辑。
//! 每个 `#[tauri::command]` 都是一个可以被前端 `invoke(...)` 调用的函数。

use std::path::Path;
use std::sync::Mutex;

use crate::dict;
use crate::engine::inject::{self, MethodStatus};
use crate::engine::{get_engine_type, resolve_game_dir};
use crate::error::{Error, Result};
use crate::model::TsSentence;
use crate::task::{self, Config};

/// 当前活着的后台任务。None = 什么都没跑。
///
/// `Mutex::new` 是 const fn，所以能直接当 static 用，不用 OnceLock。
static CURRENT: Mutex<Option<task::Handle>> = Mutex::new(None);

/// 读取一份词典文件，把词条列表交给前端。
///
/// 格式不用前端先选：MTool 的 JSON 和 wtool 自己的两种格式都认，认不出会报错。
#[tauri::command]
pub fn load_dictionary(path: &str) -> Result<Vec<TsSentence>> {
    dict::load_ts_file(Path::new(path))
}

/// 记下 GUI 上填的那几个变量（游戏目录、词典路径、目标字体）。
///
/// 全局就一份，存在 `task` 里；点「开始翻译」之前调一次，
/// `start_trans` 起的线程会自己读。
#[tauri::command]
pub fn set_config(
    game_dir: String,
    dict_path: String,
    font: String,
    translate_backend: String,
    corpus_path: String,
    out_path: String,
    llm_base_url: String,
    llm_model: String,
    llm_api_key: String,
    llm_batch: usize,
) -> bool {
    // gtx 是默认后端（免费零配置）；空串或别的值按 gtx 处理。
    let backend = if translate_backend.trim() == "llm" {
        "llm"
    } else {
        "gtx"
    };
    task::set_config(Config {
        game_dir: resolve_game_dir(&game_dir).to_string_lossy().into_owned(),
        dict_path,
        font,
        translate_backend: backend.to_string(),
        collect: false,
        corpus_path,
        out_path,
        llm_base_url,
        llm_model,
        llm_api_key,
        llm_batch,
    });
    true
}

/// 离线翻译：后台线程把语料翻成词典（`--translate` 的 GUI 版）。语料/输出/后端/
/// LLM 配置都已由 `set_config` 收好，这里只负责起线程、把进度经 `trans-status`
/// 推给前端。
#[tauri::command]
pub fn start_translate_file(app: tauri::AppHandle) -> Result<()> {
    task::trainning_translate(&app)
}

/// 在线采集：起后台线程，让玩家正常玩一遍攒语料。游戏目录/语料路径在 `set_config`
/// 已收好；这个任务放进 `CURRENT`，前端「停止」按钮能停（和直播翻译同一套语义）。
#[tauri::command]
pub fn start_collect(app: tauri::AppHandle) -> Result<()> {
    *CURRENT.lock().unwrap() = Some(task::trainning_collect(&app)?);
    Ok(())
}

/// 离线扫描：不开游戏，直接扫游戏目录收语料，返回条数给人看。同步执行，很快。
#[tauri::command]
pub fn collect_offline(game_dir: String, out: String) -> Result<String> {
    // collect_offline 内部自己 resolve 游戏目录，这里原样透传即可。
    task::collect_offline(&game_dir, Path::new(&out))
}

/// 猜这个目录是什么引擎、跑在什么运行时上，给界面显示。
///
/// 单开一个口子是为了让「认不出目录」这个症状自证：界面上写「未知引擎」，
/// 就说明填的路径不是游戏根目录（或者路径不对），而不是这个游戏真没有
/// 可用的注入方式 —— 否则两种情况的界面表现一模一样，没法查。
#[tauri::command]
pub fn detect_engine(game_dir: &str) -> String {
    let kind = get_engine_type(&resolve_game_dir(game_dir));
    format!("{}（{}）", kind.name(), kind.runtime().name())
}

/// 起一个后台任务。把新句柄放进 CURRENT，旧句柄被覆盖 = 自动 drop = 旧任务退出。
#[tauri::command]
pub fn start_trans(app: tauri::AppHandle) -> bool {
    let mut current = CURRENT.lock().unwrap();
    *current = Some(task::trainning(&app));
    true
}

/// 停掉后台任务。把句柄置空，Sender 随之 drop，线程那边 recv 返回 Disconnected，任务自己退出。
#[tauri::command]
pub fn stop_trans() -> bool {
    *CURRENT.lock().unwrap() = None;
    true
}

/// 这个游戏支持哪些注入方式、各自现在什么状态。
///
/// 引擎是现猜的，不用用户选 —— 和「开始翻译」走的是同一个 `get_engine_type`，
/// 所以界面上看到的和真跑起来的一定一致。认不出引擎就返回空列表，
/// 由界面去说「这个引擎还没有可用的注入方式」。
#[tauri::command]
pub fn list_injections(game_dir: &str) -> Vec<MethodStatus> {
    let dir = resolve_game_dir(game_dir);
    inject::status(get_engine_type(&dir), &dir)
}

/// 装上某个注入方式，把游戏目录改成能连上 wtool 的样子。
///
/// 和 `uninstall_injection` 一样只搬运参数 —— 这里不启动翻译任务，
/// 用户可以只注入、稍后自己开任务。
#[tauri::command]
pub fn install_injection(game_dir: &str, id: &str) -> Result<Vec<String>> {
    let dir = resolve_game_dir(game_dir);
    let method = inject::find(get_engine_type(&dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.install(&dir)
}

/// 撤销某个注入方式，把游戏目录恢复原样。
///
/// 不动后台任务：停任务是 `stop_trans` 的事，前端按 `stop_trans` → `uninstall_injection`
/// 的顺序调两次就行。这里保持「只搬运参数 + 转换错误」。
#[tauri::command]
pub fn uninstall_injection(game_dir: &str, id: &str) -> Result<Vec<String>> {
    let dir = resolve_game_dir(game_dir);
    let method = inject::find(get_engine_type(&dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.uninstall(&dir)
}





