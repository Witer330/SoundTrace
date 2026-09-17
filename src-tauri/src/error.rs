use serde::Serialize;
use std::fmt;

/// 前端可读的错误：所有 Tauri command 统一返回 Result<T, AppError>，
/// 序列化为 { msg: string }，前端 catch 后直接展示 msg。
#[derive(Debug, Serialize)]
pub struct AppError {
    pub msg: String,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;

macro_rules! from_err {
    ($ty:ty) => {
        impl From<$ty> for AppError {
            fn from(e: $ty) -> Self {
                AppError { msg: e.to_string() }
            }
        }
    };
}

from_err!(std::io::Error);
from_err!(rusqlite::Error);
from_err!(serde_json::Error);
from_err!(tauri::Error);

pub fn err(msg: impl Into<String>) -> AppError {
    AppError { msg: msg.into() }
}
