//! 全工程统一错误类型。
//!
//! 前端只会收到 `{ "message": "..." }`，不暴露 Rust 侧的细节。

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Error {
    pub message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(format!("IO 失败: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(format!("JSON 解析失败: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
