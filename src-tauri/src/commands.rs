//! 前端（Vue）与 Rust 之间的唯一通道。
//!
//! 约定：这里只做「参数搬运 + 错误转换」，不放业务逻辑。
//! 每个 `#[tauri::command]` 都是一个可以被前端 `invoke(...)` 调用的函数。

use std::path::Path;

use crate::dict::mtool;
use crate::error::Result;
use crate::model::TsSentence;

/// 读取一份 MTool 词典文件，把词条列表交给前端。
#[tauri::command]
pub fn load_dictionary(path: &str) -> Result<Vec<TsSentence>> {
    mtool::load_ts_file(Path::new(path))
}
