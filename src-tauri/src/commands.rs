//! 前端（Vue）与 Rust 之间的唯一通道。
//!
//! 约定：这里只做「参数搬运 + 错误转换」，不放业务逻辑。
//! 每个 `#[tauri::command]` 都是一个可以被前端 `invoke(...)` 调用的函数。

use std::path::Path;
use std::sync::Mutex;

use crate::dict::mtool;
use crate::engine::get_engine_type;
use crate::engine::inject::{self, MethodStatus};
use crate::error::{Error, Result};
use crate::model::TsSentence;
use crate::task::{self, Config};

/// 当前活着的后台任务。None = 什么都没跑。
///
/// `Mutex::new` 是 const fn，所以能直接当 static 用，不用 OnceLock。
static CURRENT: Mutex<Option<task::Handle>> = Mutex::new(None);

/// 读取一份 MTool 词典文件，把词条列表交给前端。
#[tauri::command]
pub fn load_dictionary(path: &str) -> Result<Vec<TsSentence>> {
    mtool::load_ts_file(Path::new(path))
}

/// 记下 GUI 上填的那几个变量（游戏目录、词典路径）。
///
/// 全局就一份，存在 `task` 里；点「开始翻译」之前调一次，
/// `start_trans` 起的线程会自己读。
#[tauri::command]
pub fn set_config(game_dir: String, dict_path: String) -> bool {
    task::set_config(Config {
        game_dir,
        dict_path,
    });
    true
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
    let dir = Path::new(game_dir);
    inject::status(get_engine_type(dir), dir)
}

/// 装上某个注入方式，把游戏目录改成能连上 wtool 的样子。
///
/// 和 `uninstall_injection` 一样只搬运参数 —— 这里不启动翻译任务，
/// 用户可以只注入、稍后自己开任务。
#[tauri::command]
pub fn install_injection(game_dir: &str, id: &str) -> Result<Vec<String>> {
    let dir = Path::new(game_dir);
    let method = inject::find(get_engine_type(dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.install(dir)
}

/// 撤销某个注入方式，把游戏目录恢复原样。
///
/// 不动后台任务：停任务是 `stop_trans` 的事，前端按 `stop_trans` → `uninstall_injection`
/// 的顺序调两次就行。这里保持「只搬运参数 + 转换错误」。
#[tauri::command]
pub fn uninstall_injection(game_dir: &str, id: &str) -> Result<Vec<String>> {
    let dir = Path::new(game_dir);
    let method = inject::find(get_engine_type(dir), id)
        .ok_or_else(|| Error::new(format!("这个游戏上没有叫「{id}」的注入方式")))?;
    method.uninstall(dir)
}





