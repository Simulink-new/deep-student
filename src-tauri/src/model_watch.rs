//! 模型新版本每日巡检 (task-046)
//!
//! 每日后台任务：对每个配置了 API Key 的供应商拉取远端模型列表，
//! 与本地模型条目（user + builtin 合并视图）diff，把「供应商已上架但本地
//! 尚未配置」的新模型持久化到 settings，并通过 `model-watch:discovered`
//! 事件通知前端设置页。
//!
//! 归属与去重（2026-09-19 修复「全部归到英伟达」）：已知模型判断为
//! **全局**双索引（完整 id + 去 `厂商/` 前缀的短名，跨所有供应商），同一
//! 模型被多个供应商发现时合并为一条 pending，带全部来源；聚合平台
//! （NVIDIA NIM）的来源排在末尾，前端默认选中的首位来源即原生供应商。
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

/// 一个新模型在某个供应商下的来源（聚合平台如 NVIDIA NIM 的模型 id 可能
/// 带厂商前缀，如 `zai/glm-4.6`，与原生供应商的 `glm-4.6` 同模型不同 id）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VendorSource {
    pub vendor_id: String,
    pub vendor_name: String,
    /// 该供应商下的实际模型 id（adopt 时按它建条目）
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    /// 主模型 id（第一个发现来源的 id，仅作展示与稳定 key）
    pub model_id: String,
    /// 展示名（Gemini displayName / Anthropic display_name），无则等同 model_id
    pub label: String,
    pub discovered_at: String,
    /// 上架该模型的全部供应商；顺序为发现顺序，聚合平台（nvidia 等）排最后，
    /// 前端默认选中首位即「原生供应商」
    pub sources: Vec<VendorSource>,
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

/// 模型短名：去掉聚合平台的厂商前缀（`zai/glm-4.6` → `glm-4.6`），小写。
/// 跨供应商去重（known/pending/dismissed）统一按短名比较，避免同一模型
/// 因 NVIDIA NIM 等聚合平台带前缀的 id 而绕过去重。
fn short_model_name(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_lowercase()
}

/// 聚合型供应商（上架全厂商模型的平台）排在来源列表末尾，
/// 让原生供应商成为前端默认选中的「使用」目标。
fn order_sources(sources: &mut [VendorSource]) {
    sources.sort_by_key(|s| is_aggregator(&s.vendor_id, &s.vendor_name));
}

/// 仅按现有证据识别 NVIDIA NIM；后续发现其他聚合平台（OpenRouter 等）再扩
fn is_aggregator(_vendor_id: &str, vendor_name: &str) -> bool {
    vendor_name.to_lowercase().contains("nvidia")
        || vendor_name.to_lowercase().contains("nim")
}

/// 旧版单来源 pending 条目（vendorId/vendorName 顶层字段），仅用于兼容读盘
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyDiscoveredModel {
    vendor_id: String,
    vendor_name: String,
    model_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    discovered_at: String,
}

fn read_pending(db: &Database) -> Vec<DiscoveredModel> {
    let raw = db
        .get_setting(KEY_PENDING)
        .ok()
        .flatten()
        .unwrap_or_default();
    let values: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
    let mut list = Vec::with_capacity(values.len());
    for v in values {
        if v.get("sources").is_some() {
            if let Ok(item) = serde_json::from_value::<DiscoveredModel>(v) {
                list.push(item);
            }
        } else if let Ok(old) = serde_json::from_value::<LegacyDiscoveredModel>(v) {
            // 旧格式迁移：单来源转为 sources 数组，label 缺失回退 model_id
            list.push(DiscoveredModel {
                model_id: old.model_id.clone(),
                label: if old.label.is_empty() {
                    old.model_id.clone()
                } else {
                    old.label
                },
                discovered_at: old.discovered_at,
                sources: vec![VendorSource {
                    vendor_id: old.vendor_id,
                    vendor_name: old.vendor_name,
                    model_id: old.model_id,
                }],
            });
        }
    }
    list
}

fn write_pending(db: &Database, pending: &[DiscoveredModel]) -> Result<()> {
    let json = serde_json::to_string(pending)
        .map_err(|e| AppError::configuration(format!("序列化模型巡检结果失败: {}", e)))?;
    db.save_setting(KEY_PENDING, &json)
        .map_err(|e| AppError::database(format!("保存模型巡检结果失败: {}", e)))
}

fn read_dismissed(db: &Database) -> HashSet<String> {
    let raw = db
        .get_setting(KEY_DISMISSED)
        .ok()
        .flatten()
        .unwrap_or_default();
    serde_json::from_str::<Vec<String>>(&raw)
        .unwrap_or_default()
        .into_iter()
        // 兼容旧键格式 `{vendor_id}::{model}`：统一归一为模型短名
        .map(|k| short_model_name(k.split("::").last().unwrap_or(&k)))
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

    // 已知模型全局双索引（大小写不敏感）：完整 id + 短名。
    // 跨供应商全局比对——聚合平台（NVIDIA NIM 等）上架的第三方模型若已
    // 在其原生供应商下配置（含带前缀变体），不再重复报告。
    let mut known_full: HashSet<String> = HashSet::new();
    let mut known_short: HashSet<String> = HashSet::new();
    for p in &profiles {
        known_full.insert(p.model.to_lowercase());
        known_short.insert(short_model_name(&p.model));
    }

    let mut pending = read_pending(db);
    let dismissed = read_dismissed(db);
    // 现存 pending 已覆盖的模型短名
    let pending_keys: HashSet<String> = pending
        .iter()
        .flat_map(|d| {
            std::iter::once(short_model_name(&d.model_id))
                .chain(d.sources.iter().map(|s| short_model_name(&s.model_id)))
        })
        .collect();

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::internal(format!("创建 HTTP 客户端失败: {}", e)))?;

    let mut vendors_checked = 0usize;
    let mut vendors_failed = 0usize;
    // 本轮新发现的模型（短名 → 下标），同模型多供应商来源合并进同一条
    let mut newly_found: Vec<DiscoveredModel> = Vec::new();
    let mut found_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut pending_dirty = false;
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
                for m in remote {
                    let full = m.id.to_lowercase();
                    let short = short_model_name(&m.id);
                    // 任何供应商（含带前缀变体）已配置 → 已知，不报
                    if known_full.contains(&full) || known_short.contains(&short) {
                        continue;
                    }
                    if dismissed.contains(&short) {
                        continue;
                    }

                    let source = VendorSource {
                        vendor_id: vendor.id.clone(),
                        vendor_name: vendor.name.clone(),
                        model_id: m.id.clone(),
                    };

                    // 本轮已发现同模型（短名相同）→ 追加来源
                    if let Some(&i) = found_index.get(&short) {
                        if !newly_found[i].sources.iter().any(|s| s.vendor_id == vendor.id) {
                            newly_found[i].sources.push(source);
                        }
                        continue;
                    }
                    // 早前轮次已入 pending 的同模型 → 把新来源补进旧条目
                    if pending_keys.contains(&short) {
                        if let Some(entry) = pending
                            .iter_mut()
                            .find(|d| short_model_name(&d.model_id) == short)
                        {
                            if !entry.sources.iter().any(|s| s.vendor_id == vendor.id) {
                                entry.sources.push(source);
                                pending_dirty = true;
                            }
                        }
                        continue;
                    }

                    found_index.insert(short, newly_found.len());
                    newly_found.push(DiscoveredModel {
                        model_id: m.id.clone(),
                        label: m.label,
                        discovered_at: now.clone(),
                        sources: vec![source],
                    });
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
                .map(|d| {
                    format!(
                        "{} [{}]",
                        d.model_id,
                        d.sources
                            .iter()
                            .map(|s| s.vendor_name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
        );
    }

    // 聚合平台来源排末尾（原生供应商成为默认「使用」目标），有变更才落盘
    for d in &mut newly_found {
        order_sources(&mut d.sources);
    }
    if !newly_found.is_empty() || pending_dirty {
        for d in &mut pending {
            order_sources(&mut d.sources);
        }
        pending.extend(newly_found.iter().cloned());
        write_pending(db, &pending)?;
        if !newly_found.is_empty() {
            if let Some(app) = app {
                if let Err(e) = app.emit(MODEL_WATCH_DISCOVERED_EVENT, &newly_found) {
                    log::warn!("[ModelWatch] 事件发送失败: {}", e);
                }
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
///
/// ⚠️ 运行时上下文红线（2026-09-16 启动闪退事故）：
/// 本函数在 setup() 里被**主线程同步**调用，此处没有 Tokio 运行时上下文。
/// `BACKGROUND_TASKS.spawn` 内部是 `tokio::spawn`，在无线程上下文处调用会
/// panic「there is no reactor running」→ 进程启动即崩溃。
/// 因此这里必须用 `tauri::async_runtime::spawn`（自带运行时句柄）。
/// 另外本任务是无限循环，若注册进 TaskTracker，`shutdown()` 的 wait() 每次
/// 退出都会等满 5s 超时——无限任务本就不该被追踪，随进程退出丢弃即可
/// （巡检 best-effort：退出时若在运行中，最坏只是少写一次 settings）。
pub fn start_daily_scheduler(app: AppHandle, llm: Arc<LLMManager>, db: Arc<Database>) {
    tauri::async_runtime::spawn(async move {
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

/// 忽略某个新模型（按模型短名，对该模型的所有供应商来源生效）
#[tauri::command]
pub async fn model_watch_dismiss(
    vendor_id: String,
    model_id: String,
    state: State<'_, AppState>,
) -> Result<()> {
    let _ = vendor_id; // 保留参数以维持前端调用兼容；归属判断已改为模型级
    let db = &state.database;
    let short = short_model_name(&model_id);

    let mut dismissed = read_dismissed(db);
    dismissed.insert(short.clone());
    write_dismissed(db, &dismissed)?;

    let pending: Vec<DiscoveredModel> = read_pending(db)
        .into_iter()
        .filter(|d| {
            short_model_name(&d.model_id) != short
                && !d.sources.iter().any(|s| short_model_name(&s.model_id) == short)
        })
        .collect();
    write_pending(db, &pending)
}

/// 「使用」发现的新模型：直接在对应供应商下创建**启用状态**的模型条目。
///
/// 条目参数取默认值（max_output_tokens=8192 / temperature=0.7 / general adapter），
/// api_protocol 继承供应商配置（save 时仍会按 vendor 规范化），label 优先用巡检
/// 时拿到的展示名（Gemini displayName / Anthropic display_name），能力位按
/// model_id 启发式粗猜——猜错只影响 UI 标签，可在模型编辑器修正。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelWatchAdoptResult {
    /// false = 已存在同 vendor+model 条目（幂等命中），本次未新建
    pub created: bool,
    /// 供应商是否已配置 API Key——无 key 时条目虽启用但运行时不可用，前端据此提醒
    pub vendor_has_key: bool,
}

/// 按 model_id 粗猜能力位。只认子串证据强的模式，宁缺勿滥：
/// vision/-vl → 多模态；thinking/reason → 推理；embed/bge → 向量；rerank → 重排。
fn guess_capabilities(model_id: &str) -> (bool, bool, bool, bool) {
    let id = model_id.to_lowercase();
    let multimodal = id.contains("vision") || id.contains("-vl");
    let reasoning = id.contains("thinking") || id.contains("reason");
    let embedding = id.contains("embed") || id.contains("bge");
    let reranker = id.contains("rerank");
    (multimodal, reasoning, embedding, reranker)
}

#[tauri::command]
pub async fn model_watch_adopt_model(
    vendor_id: String,
    model_id: String,
    state: State<'_, AppState>,
) -> Result<ModelWatchAdoptResult> {
    let llm = &state.llm_manager;

    // 供应商必须仍存在（pending 里可能残留已被删除的供应商）
    let vendors = llm.read_user_vendor_configs().await?;
    let vendor = vendors
        .iter()
        .find(|v| v.id == vendor_id)
        .ok_or_else(|| {
            AppError::configuration(format!("供应商不存在或已删除: {}", vendor_id))
        })?;
    let vendor_has_key = !vendor.api_key.is_empty();

    let db = &state.database;

    // label 优先取巡检时的展示名，缺失回退 model_id
    let label = read_pending(db)
        .into_iter()
        .find(|d| {
            d.sources
                .iter()
                .any(|s| s.vendor_id == vendor_id && s.model_id.eq_ignore_ascii_case(&model_id))
        })
        .map(|d| d.label)
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| model_id.clone());

    // 幂等：已存在同 vendor+model 的条目时只清理 pending，不重复建
    let mut profiles = llm.read_user_model_profiles().await?;
    let exists = profiles
        .iter()
        .any(|p| p.vendor_id == vendor_id && p.model.eq_ignore_ascii_case(&model_id));
    let created = if !exists {
        let (is_multimodal, is_reasoning, is_embedding, is_reranker) =
            guess_capabilities(&model_id);
        let adopted = ModelProfile {
            vendor_id: vendor_id.clone(),
            label,
            model: model_id.clone(),
            enabled: true,
            is_multimodal,
            is_reasoning,
            is_embedding,
            is_reranker,
            api_protocol: vendor.api_protocol.clone(),
            ..Default::default()
        };
        profiles.push(adopted);
        llm.save_model_profiles(&profiles).await?;
        true
    } else {
        false
    };

    // 从待处理列表移除整条（同模型的其他来源也不再提示——全局 known 双索引
    // 会把它的短名/完整 id 变体一并视为已配置）
    let short = short_model_name(&model_id);
    let pending: Vec<DiscoveredModel> = read_pending(db)
        .into_iter()
        .filter(|d| {
            short_model_name(&d.model_id) != short
                && !d.sources.iter().any(|s| short_model_name(&s.model_id) == short)
        })
        .collect();
    write_pending(db, &pending)?;

    Ok(ModelWatchAdoptResult {
        created,
        vendor_has_key,
    })
}
