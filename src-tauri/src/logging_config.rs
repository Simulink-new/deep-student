//! 运行时日志配置（task-047）
//!
//! 持久化在 `{config_dir}/com.lanxia.deepstudent/logging.json`，在 Builder
//! 装配日志插件**之前**同步读取（此时 DB 未就绪，不能用 settings 表）。
//!
//! - `level`：全局日志级别（默认 info），**重启后生效**——tauri-plugin-log
//!   的级别在构建期固化，无运行时热改 API（对标 JetBrains 需换底座，见
//!   docs/diagnostics-logging-2026-09-16.md）。
//! - `webview_mirror`：是否把后端日志镜像到 Webview 控制台（默认关；
//!   生产环境每条日志过 IPC 是持续开销）。dev build 始终开启。
//!
//! 另提供统一的「日志根目录」解析：所有运行日志（主日志/崩溃/前端/后端
//! 结构化）归于 `%LOCALAPPDATA%\<identifier>\logs\`（即 Tauri 的
//! app_log_dir）。Windows 惯例日志本就走 Local 而非 Roaming。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 与 tauri.conf.json 的 identifier 保持一致（此处无法读配置，硬编码单一来源语义）
const APP_IDENTIFIER: &str = "com.lanxia.deepstudent";
const PREFS_FILENAME: &str = "logging.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingPrefs {
    /// error | warn | info | debug | trace
    pub level: String,
    pub webview_mirror: bool,
}

impl Default for LoggingPrefs {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            webview_mirror: false,
        }
    }
}

impl LoggingPrefs {
    pub fn level_filter(&self) -> log::LevelFilter {
        match self.level.trim().to_ascii_lowercase().as_str() {
            "error" => log::LevelFilter::Error,
            "warn" => log::LevelFilter::Warn,
            "debug" => log::LevelFilter::Debug,
            "trace" => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        }
    }
}

/// logging.json 的完整路径；配置目录不可解析时返回 None（调用方回退默认配置）
fn prefs_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_IDENTIFIER).join(PREFS_FILENAME))
}

/// 读取配置；文件缺失/损坏时回退默认（日志系统必须永远可用）
pub fn load_prefs() -> LoggingPrefs {
    let Some(path) = prefs_path() else {
        return LoggingPrefs::default();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return LoggingPrefs::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_prefs(prefs: &LoggingPrefs) -> std::io::Result<()> {
    let path = prefs_path()
        .ok_or_else(|| std::io::Error::other("无法解析应用配置目录".to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| std::io::Error::other(format!("序列化日志配置失败: {}", e)))?;
    // 原子写：先写临时文件再改名，避免中途崩溃留下半个 JSON
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// 统一的日志根目录（Tauri app_log_dir；解析失败时回退 app_data_dir/logs）
pub fn resolve_log_root(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    app.path().app_log_dir().unwrap_or_else(|_| {
        app.path()
            .app_data_dir()
            .map(|d| d.join("logs"))
            .unwrap_or_else(|_| std::env::temp_dir().join("deep-student").join("logs"))
    })
}

// ==================== Tauri 命令 ====================

#[tauri::command]
pub fn logging_get_prefs() -> LoggingPrefs {
    load_prefs()
}

/// 保存日志级别/镜像开关。**重启后生效**（tauri-plugin-log 级别构建期固化）。
#[tauri::command]
pub fn logging_set_prefs(level: String, webview_mirror: bool) -> Result<(), String> {
    let normalized = level.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "error" | "warn" | "info" | "debug" | "trace" => {}
        other => return Err(format!("非法日志级别: {}", other)),
    }
    let prefs = LoggingPrefs {
        level: normalized,
        webview_mirror,
    };
    save_prefs(&prefs).map_err(|e| e.to_string())
}
