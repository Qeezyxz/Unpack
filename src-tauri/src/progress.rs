use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Serialize)]
pub struct ProgressEvent {
    pub channel: String,
    pub message: String,
    pub current: u64,
    pub total: u64,
}

pub fn emit(app: &AppHandle, channel: &str, message: impl Into<String>, current: u64, total: u64) {
    let _ = app.emit(
        "unpack-progress",
        ProgressEvent {
            channel: channel.to_string(),
            message: message.into(),
            current,
            total,
        },
    );
}
