//! 模型新版本每日巡检 (task-046)
//!
//! 每日后台任务：对每个配置了 API Key 的供应商拉取远端模型列表，
//! 与本地模型条目（user + builtin 合并视图）diff，把「供应商已上架但本地
//! 尚未配置」的新模型持久化到 settings，并通过 `model-watch:discovered`
//! 事件通知前端设置页。
//!
//! 端点约定与前端 VendorModelFetcher 保持一致（已 curl 验证）：
//! - OpenAI 兼容: `GET {base_url}/models` + `Authorization: Bearer`
//! - Gemini:      `GET {base_url}/v1beta/models?key=...&pageSize=100`
//! - Anthropic:   `GET {base_url}/models` + `x-api-key` + `anthropic-version`
//!
//! 安全属性：api_key 全程不出后端；settings 中只存模型 id/供应商名/时间戳。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::commands::AppState;
use crate::database::Database;
use crate::llm_manager::{LLMManager, ModelProfile, VendorConfig};
use crate::models::AppError;

type Result<T> = std::result::Result<T, AppError>;

/// 前端监听此事件接收「发现新模型」通知（payload: Vec<DiscoveredModel>，仅本次新增）
pub const MODEL_WATCH_DISCOVERED_EVENT: &str = "model-watch:discovered";

const KEY_LAST_RUN_AT: &str = "model_watch.last_run_at";
const KEY_PENDING: &str = "model_watch.pending";
const KEY_DISMISSED: &str = "model_watch.dismissed";

/// 距上次成功检查超过该间隔才再次运行
const RUN_INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// 主循环醒来复查「是否到期」的周期
const LOOP_TICK: Duration = Duration::from_secs(3600);
/// 启动后让路首屏渲染的延迟
const STARTUP_DELAY: Duration = Duration::from_secs(120);
/// 单供应商请求超时
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 供应商间串行间隔（礼貌限速）
const VENDOR_GAP: Duration = Duration::from_millis(500);

/// 运行互斥：防止「立即检查」按钮与每日任务并发重复拉取
static RUNNING: AtomicBool = AtomicBool::new(false);

// ==================== 对外类型 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    pub vendor_id: String,
    pub vendor_name: String,
    pub model_id: String,
    /// 展示名（Gemini displayName / Anthropic display_name），无则等同 model_id
    pub label: String,
    pub discovered_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelWatchState {
    pub last_run_at: Option<String>,
    pub pending: Vec<DiscoveredModel>,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelWatchRunSummary {
    pub vendors_checked: usize,
    pub vendors_failed: usize,
    pub new_found: usize,
}

// ==================== 远端响应解析 ====================

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Option<Vec<OpenAiModelItem>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelItem {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GeminiModelsResponse {
    models: Option<Vec<GeminiModelItem>>,
}

#[derive(Debug, Deserialize)]
struct GeminiModelItem {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    supported_generation_methods: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelsResponse {
    data: Option<Vec<AnthropicModelItem>>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelItem {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// 拉取到的远端模型（id + 展示名）
struct RemoteModel {
    id: String,
    label: String,
}

// ==================== 供应商分类与拉取 ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchKind {
    OpenAiCompat,
    Gemini,
    Anthropic,
    /// ollama / paddleocr 等无 OpenAI 兼容 /models 端点的供应商
    Unsupported,
}

fn classify_vendor(vendor: &VendorConfig) -> FetchKind {
    let pt = vendor.provider_type.to_lowercase();
    match pt.as_str() {
        "gemini" => FetchKind::Gemini,
        "anthropic" | "claude" => FetchKind::Anthropic,
        "ollama" | "paddleocr" | "local" => FetchKind::Unsupported,
        _ => FetchKind::OpenAiCompat,
    }
}

/// 与前端 VendorModelFetcher 的过滤规则保持一致：
/// 排除音频/视频/图像生成类模型（这些不走文本对话配置）
fn is_text_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    !(id.contains("tts")
        || id.contains("whisper")
        || id.contains("video")
        || id.contains("kolors")
        || id.contains("flux")
        || id.contains("dall-e")
        || id.contains("audio"))
}

async fn fetch_remote_models(
    client: &reqwest::Client,
    vendor: &VendorConfig,
) -> std::result::Result<Vec<RemoteModel>, String> {
    let base = vendor.base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("base_url 为空".to_string());
    }

    match classify_vendor(vendor) {
        FetchKind::Unsupported => Err("该供应商类型不支持模型列表巡检".to_string()),
        FetchKind::OpenAiCompat => {
            let mut req = client.get(format!("{}/models", base));
            // NVIDIA NIM 无需认证；其余供应商带 Bearer
            if vendor.provider_type.to_lowercase() != "nvidia" && !vendor.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", vendor.api_key));
            }
            for (k, v) in &vendor.headers {
                req = req.header(k, v);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("HTTP {}", status.as_u16()));
            }
            let body: OpenAiModelsResponse = resp.json().await.map_err(|e| e.to_string())?;
            let mut list: Vec<RemoteModel> = body
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|m| m.id)
                .filter(|id| is_text_model(id))
                .map(|id| RemoteModel {
                    label: id.clone(),
                    id,
                })
                .collect();
            list.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(list)
        }
        FetchKind::Gemini => {
            if vendor.api_key.is_empty() {
                return Err("缺少 API Key".to_string());
            }
            let url = format!(
                "{}/v1beta/models?key={}&pageSize=100",
                base, vendor.api_key
            );
            let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("HTTP {}", status.as_u16()));
            }
            let body: GeminiModelsResponse = resp.json().await.map_err(|e| e.to_string())?;
            let mut list: Vec<RemoteModel> = body
                .models
                .unwrap_or_default()
                .into_iter()
                .filter(|m| {
                    m.supported_generation_methods
                        .as_deref()
                        .map(|ms| ms.iter().any(|g| g == "generateContent"))
                        .unwrap_or(false)
                })
                .map(|m| {
                    // "models/gemini-2.5-pro" → "gemini-2.5-pro"
                    let id = m
                        .name
                        .strip_prefix("models/")
                        .unwrap_or(&m.name)
                        .to_string();
                    RemoteModel {
                        label: m.display_name.unwrap_or_else(|| id.clone()),
                        id,
                    }
                })
                .collect();
            list.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(list)
        }
        FetchKind::Anthropic => {
            if vendor.api_key.is_empty() {
                return Err("缺少 API Key".to_string());
            }
            // 仅取首页：Anthropic 模型目录很小，分页收益为零
            let mut req = client
                .get(format!("{}/models", base))
                .header("x-api-key", &vendor.api_key)
                .header("anthropic-version", "2023-06-01");
            for (k, v) in &vendor.headers {
                req = req.header(k, v);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("HTTP {}", status.as_u16()));
            }
            let body: AnthropicModelsResponse = resp.json().await.map_err(|e| e.to_string())?;
            let mut list: Vec<RemoteModel> = body
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|m| RemoteModel {
                    label: m.display_name.unwrap_or_else(|| m.id.clone()),
                    id: m.id,
                })
                .collect();
            list.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(list)
        }
    }
}

// ==================== 持久化 ====================

fn dismissed_key(vendor_id: &str, model_id: &str) -> String {
    format!("{}::{}", vendor_id, model_id.to_lowercase())
}

fn read_pending(db: &Database) -> Vec<DiscoveredModel> {
    db.get_setting(KEY_PENDING)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_pending(db: &Database, pending: &[DiscoveredModel]) -> Result<()> {
    let json = serde_json::to_string(pending)
        .map_err(|e| AppError::configuration(format!("序列化模型巡检结果失败: {}", e)))?;
    db.save_setting(KEY_PENDING, &json)
        .map_err(|e| AppError::database(format!("保存模型巡检结果失败: {}", e)))
}

fn read_dismissed(db: &Database) -> HashSet<String> {
    db.get_setting(KEY_DISMISSED)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn write_dismissed(db: &Database, dismissed: &HashSet<String>) -> Result<()> {
    let list: Vec<&String> = dismissed.iter().collect();
    let json = serde_json::to_string(&list)
        .map_err(|e| AppError::configuration(format!("序列化忽略清单失败: {}", e)))?;
    db.save_setting(KEY_DISMISSED, &json)
        .map_err(|e| AppError::database(format!("保存忽略清单失败: {}", e)))
}

fn read_last_run_at(db: &Database) -> Option<String> {
    db.get_setting(KEY_LAST_RUN_AT).ok().flatten()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ==================== 巡检主流程 ====================

/// 是否需要运行：从未运行，或距上次成功运行超过 24h
fn due_for_run(db: &Database) -> bool {
    match read_last_run_at(db).and_then(|s| DateTime::parse_from_rfc3339(&s).ok()) {
        Some(last) => {
            let elapsed = Utc::now()
                .signed_duration_since(last.with_timezone(&Utc))
                .to_std()
                .unwrap_or(Duration::ZERO);
            elapsed >= RUN_INTERVAL
        }
        None => true,
    }
}

/// 执行一轮巡检。返回运行摘要；`run` 内任何单点失败都只记日志不中断整轮。
/// 仅当至少一个供应商拉取成功时才推进 last_run_at（网络整体故障时下一小时重试）。
async fn run_check(
    app: Option<&AppHandle>,
    llm: &LLMManager,
    db: &Database,
) -> Result<ModelWatchRunSummary> {
    // 互斥：已有运行则直接报告 0
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(ModelWatchRunSummary {
            vendors_checked: 0,
            vendors_failed: 0,
            new_found: 0,
        });
    }
    let result = run_check_inner(app, llm, db).await;
    RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_check_inner(
    app: Option<&AppHandle>,
    llm: &LLMManager,
    db: &Database,
) -> Result<ModelWatchRunSummary> {
    let vendors = llm.read_user_vendor_configs().await.unwrap_or_else(|e| {
        log::warn!("[ModelWatch] 读取供应商配置失败: {}", e);
        Vec::new()
    });
    let profiles = llm.get_model_profiles().await.unwrap_or_else(|e| {
        log::warn!("[ModelWatch] 读取模型条目失败: {}", e);
        Vec::new()
    });

    // 已知模型集合（大小写不敏感）：vendor_id -> {model_id_lower}
    let mut known: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    for p in &profiles {
        known
            .entry(p.vendor_id.clone())
            .or_default()
            .insert(p.model.to_lowercase());
    }

    let mut pending = read_pending(db);
    let dismissed = read_dismissed(db);
    // pending 中已有的不再重复加入
    let pending_keys: HashSet<String> = pending
        .iter()
        .map(|d| dismissed_key(&d.vendor_id, &d.model_id))
        .collect();

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::internal(format!("创建 HTTP 客户端失败: {}", e)))?;

    let mut vendors_checked = 0usize;
    let mut vendors_failed = 0usize;
    let mut newly_found: Vec<DiscoveredModel> = Vec::new();
    let now = now_rfc3339();

    for vendor in &vendors {
        // 无 key 的供应商跳过（NVIDIA 除外，其模型列表无需认证）
        let is_nvidia = vendor.provider_type.to_lowercase() == "nvidia";
        if vendor.api_key.is_empty() && !is_nvidia {
            continue;
        }
        if vendor.base_url.trim().is_empty() {
            continue;
        }

        match fetch_remote_models(&client, vendor).await {
            Ok(remote) => {
                vendors_checked += 1;
                let known_for_vendor = known.get(&vendor.id);
                for m in remote {
                    let key_lower = m.id.to_lowercase();
                    let already_known = known_for_vendor
                        .map(|s| s.contains(&key_lower))
                        .unwrap_or(false);
                    let dkey = dismissed_key(&vendor.id, &m.id);
                    if already_known || dismissed.contains(&dkey) || pending_keys.contains(&dkey)
                    {
                        continue;
                    }
                    let item = DiscoveredModel {
                        vendor_id: vendor.id.clone(),
                        vendor_name: vendor.name.clone(),
                        model_id: m.id,
                        label: m.label,
                        discovered_at: now.clone(),
                    };
                    newly_found.push(item);
                }
            }
            Err(e) => {
                vendors_failed += 1;
                log::debug!(
                    "[ModelWatch] 供应商 {}({}) 模型列表拉取失败: {}",
                    vendor.name,
                    vendor.id,
                    e
                );
            }
        }
        tokio::time::sleep(VENDOR_GAP).await;
    }

    if !newly_found.is_empty() {
        log::info!(
            "[ModelWatch] 发现 {} 个新模型: {:?}",
            newly_found.len(),
            newly_found
                .iter()
                .map(|d| format!("{}/{}", d.vendor_name, d.model_id))
                .collect::<Vec<_>>()
        );
        pending.extend(newly_found.iter().cloned());
        write_pending(db, &pending)?;
        if let Some(app) = app {
            if let Err(e) = app.emit(MODEL_WATCH_DISCOVERED_EVENT, &newly_found) {
                log::warn!("[ModelWatch] 事件发送失败: {}", e);
            }
        }
    }

    // 至少一个供应商成功才推进时间戳；全军覆没视为网络故障，下小时重试
    if vendors_checked > 0 {
        if let Err(e) = db.save_setting(KEY_LAST_RUN_AT, &now) {
            log::warn!("[ModelWatch] 保存巡检时间失败: {}", e);
        }
    }

    Ok(ModelWatchRunSummary {
        vendors_checked,
        vendors_failed,
        new_found: newly_found.len(),
    })
}

/// 每日调度：启动 2 分钟后首次检查（若到期），之后每小时复查一次。
/// 任务注册到全局 TaskTracker，应用退出时随 background_tasks::shutdown 优雅收尾。
pub fn start_daily_scheduler(app: AppHandle, llm: Arc<LLMManager>, db: Arc<Database>) {
    crate::background_tasks::BACKGROUND_TASKS.spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if due_for_run(&db) {
                if let Err(e) = run_check(Some(&app), &llm, &db).await {
                    log::warn!("[ModelWatch] 每日巡检失败: {}", e);
                }
            }
            tokio::time::sleep(LOOP_TICK).await;
        }
    });
    log::info!("[ModelWatch] 模型新版本每日巡检已启动");
}

// ==================== Tauri 命令 ====================

/// 获取巡检状态：上次运行时间 + 待处理新模型列表
#[tauri::command]
pub async fn model_watch_get_state(state: State<'_, AppState>) -> Result<ModelWatchState> {
    let db = &state.database;
    Ok(ModelWatchState {
        last_run_at: read_last_run_at(db),
        pending: read_pending(db),
        running: RUNNING.load(Ordering::SeqCst),
    })
}

/// 立即执行一轮巡检（设置页「立即检查」按钮）
#[tauri::command]
pub async fn model_watch_run_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelWatchRunSummary> {
    run_check(Some(&app), &state.llm_manager, &state.database).await
}

/// 忽略某个新模型（之后巡检不再报告它）
#[tauri::command]
pub async fn model_watch_dismiss(
    vendor_id: String,
    model_id: String,
    state: State<'_, AppState>,
) -> Result<()> {
    let db = &state.database;
    let dkey = dismissed_key(&vendor_id, &model_id);

    let mut dismissed = read_dismissed(db);
    dismissed.insert(dkey.clone());
    write_dismissed(db, &dismissed)?;

    let pending: Vec<DiscoveredModel> = read_pending(db)
        .into_iter()
        .filter(|d| dismissed_key(&d.vendor_id, &d.model_id) != dkey)
        .collect();
    write_pending(db, &pending)
}

/// 把发现的新模型添加为「停用状态的草稿」模型条目，供用户在模型列表中完善后启用
#[tauri::command]
pub async fn model_watch_add_as_draft(
    vendor_id: String,
    model_id: String,
    state: State<'_, AppState>,
) -> Result<()> {
    let llm = &state.llm_manager;

    // 幂等：已存在同 vendor+model 的条目时只清理 pending，不重复建
    let mut profiles = llm.read_user_model_profiles().await?;
    let exists = profiles
        .iter()
        .any(|p| p.vendor_id == vendor_id && p.model.eq_ignore_ascii_case(&model_id));
    if !exists {
        let draft = ModelProfile {
            vendor_id: vendor_id.clone(),
            label: model_id.clone(),
            model: model_id.clone(),
            enabled: false, // 草稿：先不启用，等用户在模型列表中完善参数
            ..Default::default()
        };
        profiles.push(draft);
        llm.save_model_profiles(&profiles).await?;
    }

    // 从待处理列表移除（不加入 dismissed：它已在 profiles 中，巡检自然不会再见）
    let db = &state.database;
    let dkey = dismissed_key(&vendor_id, &model_id);
    let pending: Vec<DiscoveredModel> = read_pending(db)
        .into_iter()
        .filter(|d| dismissed_key(&d.vendor_id, &d.model_id) != dkey)
        .collect();
    write_pending(db, &pending)
}
