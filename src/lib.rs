//! Persistent core. The local CLI represents the workspace's human member.
//! Agent execution is deliberately not exposed through this management entry.
pub mod acceptance;
mod api_driver;
pub mod artifact;
mod assignment;
pub mod blocker;
mod candidate;
pub mod channel;
mod checker;
pub mod cli_resources;
pub mod connection;
pub mod connection_probe;
pub mod content;
pub mod credential;
pub mod database;
mod decisions;
mod dispatch;
pub mod disposition;
pub mod execution_context;
pub mod handoff;
pub mod member;
mod member_tools;
pub mod model;
pub mod profile;
pub mod rework;
mod runs;
pub mod runtime;
pub mod sample;
pub mod store;
pub mod verification;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("数据库操作失败：{0}")]
    Database(#[from] rusqlite::Error),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("数据格式错误：{0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Forbidden(_) => "forbidden",
            Self::Unavailable(_) => "unavailable",
            Self::Database(_) => "storage_error",
            Self::Io(_) => "io_error",
            Self::Json(_) => "invalid_data",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Serialize)]
pub struct Envelope<T> {
    pub version: u8,
    pub ok: bool,
    pub data: T,
}

pub mod export;

pub mod retry;

pub mod recovery;
pub mod skill;
