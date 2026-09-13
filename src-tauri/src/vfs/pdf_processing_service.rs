//! 媒体预处理流水线服务（PDF + 图片）
//!
//! 负责在媒体文件上传后自动执行预处理流水线：
//!
//! ## PDF 处理流程
//! - Stage 1: 文本提取 (text_extraction) - 已在上传时完成
//! - Stage 2: 页面渲染 (page_rendering) - 已在上传时完成
//! - Stage 3: OCR 处理 (ocr_processing) - 复用 VFS Blob 中的预渲染图片
//! - Stage 4: 向量索引 (vector_indexing)
//!
//! ## 图片处理流程（v2.0 新增）
//! - Stage 1: 图片压缩 (image_compression) - 可选，大于阈值才压缩
//! - Stage 2: OCR 识别 (ocr_processing) - 单张图片 OCR
//! - Stage 3: 向量索引 (vector_indexing)
//!
//! ## 设计原则
//! - 异步执行：上传立即返回，后台处理
//! - 状态可见：前端实时显示处理进度
//! - 可取消：支持取消正在进行的处理
//! - 可重试：失败后可重试
//! - 图片复用：PDF Stage 3 复用 Stage 2 预渲染的图片，无需重新渲染
//! - 统一架构：PDF 和图片共享相同的事件系统和状态管理
//!
//! ## 参考文档
//! - docs/design/pdf-preprocessing-pipeline.md

use base64::Engine;
use dashmap::{DashMap, DashSet};
use futures::stream::{self, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::database::Database;

/// 记录并跳过迭代中的错误，避免静默丢弃
fn log_and_skip_err<T, E: std::fmt::Display>(result: Result<T, E>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("[MediaProcessingService] Row parse error (skipped): {}", e);
            None
        }
    }
}
use crate::file_manager::FileManager;
use crate::llm_manager::LLMManager;
use crate::models::PdfOcrTextBlock;
use crate::vfs::database::VfsDatabase;
use crate::vfs::error::{VfsError, VfsResult};
use crate::vfs::index_service::VfsIndexService;
use crate::vfs::indexing::types::{OcrPageResult, OcrPagesJson};
use crate::vfs::repos::pdf_preview::{render_pdf_preview_with_progress, PdfPreviewConfig};
use crate::vfs::repos::{VfsBlobRepo, VfsFileRepo, VfsFolderRepo, VfsNoteRepo};
use crate::vfs::types::{PdfPreviewJson, VfsCreateNoteParams, VfsFolder, VfsFolderItem};
use crate::vfs::unit_builder::UnitBuildInput;

// ============================================================================
// 处理状态枚举
// ============================================================================

/// 媒体类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    /// PDF 文档
    Pdf,
    /// 图片
    Image,
}

impl MediaType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaType::Pdf => "pdf",
            MediaType::Image => "image",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pdf" => Some(MediaType::Pdf),
            "image" => Some(MediaType::Image),
            _ => None,
        }
    }

    /// 从 MIME 类型推断媒体类型
    pub fn from_mime(mime: &str) -> Option<Self> {
        if mime == "application/pdf" {
            Some(MediaType::Pdf)
        } else if mime.starts_with("image/") {
            Some(MediaType::Image)
        } else {
            None
        }
    }
}

/// 处理阶段枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    /// 等待处理
    Pending,
    /// 文本提取中（PDF 专用）
    TextExtraction,
    /// 页面渲染中（PDF 专用）
    PageRendering,
    /// 页面压缩中（PDF 专用）
    PageCompression,
    /// 图片压缩中（图片专用）
    ImageCompression,
    /// OCR 处理中（PDF 多页 / 图片单张）
    OcrProcessing,
    /// 向量索引中
    VectorIndexing,
    /// 处理完成
    Completed,
    /// 处理完成但存在可重试问题（如 OCR/向量索引部分失败）
    CompletedWithIssues,
    /// 处理失败
    Error,
}

impl ProcessingStage {
    /// 转换为数据库存储的字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingStage::Pending => "pending",
            ProcessingStage::TextExtraction => "text_extraction",
            ProcessingStage::PageRendering => "page_rendering",
            ProcessingStage::PageCompression => "page_compression",
            ProcessingStage::ImageCompression => "image_compression",
            ProcessingStage::OcrProcessing => "ocr_processing",
            ProcessingStage::VectorIndexing => "vector_indexing",
            ProcessingStage::Completed => "completed",
            ProcessingStage::CompletedWithIssues => "completed_with_issues",
            ProcessingStage::Error => "error",
        }
    }

    /// 从字符串解析
    pub fn from_str(s: &str) -> Self {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "pending" => ProcessingStage::Pending,
            "text_extraction" => ProcessingStage::TextExtraction,
            "page_rendering" => ProcessingStage::PageRendering,
            "page_compression" => ProcessingStage::PageCompression,
            "image_compression" => ProcessingStage::ImageCompression,
            "ocr_processing" => ProcessingStage::OcrProcessing,
            "vector_indexing" => ProcessingStage::VectorIndexing,
            "completed" => ProcessingStage::Completed,
            "completed_with_issues" => ProcessingStage::CompletedWithIssues,
            "error" => ProcessingStage::Error,
            _ => ProcessingStage::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingIssue {
    pub stage: String,
    pub message: String,
    #[serde(default)]
    pub retriable: bool,
}

/// 处理进度
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingProgress {
    /// 当前阶段
    pub stage: String,
    /// 当前处理的页码（PDF 渲染/OCR 时使用，图片始终为 1）
    #[serde(skip_serializing_if = "Option::is_none", alias = "current_page")]
    pub current_page: Option<usize>,
    /// 总页数（PDF 专用，图片始终为 1）
    #[serde(skip_serializing_if = "Option::is_none", alias = "total_pages")]
    pub total_pages: Option<usize>,
    /// 总进度百分比 (0-100)
    pub percent: f32,
    /// 已就绪的注入模式
    /// - PDF: ["text", "image", "ocr"]
    /// - 图片: ["image", "ocr"]
    #[serde(alias = "ready_modes")]
    pub ready_modes: Vec<String>,
    /// 媒体类型（v2.0 新增）
    #[serde(skip_serializing_if = "Option::is_none", alias = "media_type")]
    pub media_type: Option<String>,
    /// 已记录的问题阶段（用于 completed_with_issues/重试定位）
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "failed_stages",
        default
    )]
    pub failed_stages: Option<Vec<ProcessingIssue>>,
}

impl Default for ProcessingProgress {
    fn default() -> Self {
        Self {
            stage: "pending".to_string(),
            current_page: None,
            total_pages: None,
            percent: 0.0,
            ready_modes: vec![],
            media_type: None,
            failed_stages: None,
        }
    }
}

/// 处理状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingStatus {
    /// 文件 ID
    pub file_id: String,
    /// 当前阶段
    pub stage: String,
    /// 进度
    pub progress: ProcessingProgress,
    /// 错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 开始时间戳（毫秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// 完成时间戳（毫秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

// ============================================================================
// OCR 策略配置
// ============================================================================

/// PDF 文本提取阈值默认值（字符数）
/// 如果提取的文本少于此阈值，则认为是扫描版 PDF，需要触发 OCR
const DEFAULT_PDF_TEXT_THRESHOLD: usize = 100;

/// OCR 策略配置（与设置面板保持一致）
#[derive(Debug, Clone)]
struct OcrStrategyConfig {
    /// 是否启用自动 OCR
    pub enabled: bool,
    /// 多模态模型跳过 OCR（当前统一跳过）
    pub skip_for_multimodal: bool,
    /// PDF 文本阈值（字符数，低于此值触发 OCR）
    pub pdf_text_threshold: usize,
    /// 是否对图片启用 OCR
    pub ocr_images: bool,
    /// 是否对扫描版 PDF 启用 OCR
    pub ocr_scanned_pdf: bool,
}

impl Default for OcrStrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // 默认不跳过 OCR，确保文本索引有内容
            skip_for_multimodal: false,
            pdf_text_threshold: DEFAULT_PDF_TEXT_THRESHOLD,
            ocr_images: true,
            ocr_scanned_pdf: true,
        }
    }
}

impl OcrStrategyConfig {
    /// 从数据库设置加载配置
    fn load_from_db(db: &Database) -> Self {
        let mut config = Self::default();

        if let Ok(Some(v)) = db.get_setting("ocr.enabled") {
            config.enabled = v.to_lowercase() == "true";
        }
        if let Ok(Some(v)) = db.get_setting("ocr.skip_for_multimodal") {
            config.skip_for_multimodal = v.to_lowercase() == "true";
        }
        if let Ok(Some(v)) = db.get_setting("ocr.pdf_text_threshold") {
            if let Ok(n) = v.parse::<usize>() {
                config.pdf_text_threshold = n;
            }
        }
        if let Ok(Some(v)) = db.get_setting("ocr.images") {
            config.ocr_images = v.to_lowercase() == "true";
        }
        if let Ok(Some(v)) = db.get_setting("ocr.scanned_pdf") {
            config.ocr_scanned_pdf = v.to_lowercase() == "true";
        }

        config
    }
}

// ============================================================================
// 事件类型
// ============================================================================

/// 进度事件（统一媒体处理事件）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProcessingProgressEvent {
    pub file_id: String,
    pub status: ProcessingProgress,
    pub media_type: String,
}

/// 完成事件（统一媒体处理事件）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProcessingCompletedEvent {
    pub file_id: String,
    pub ready_modes: Vec<String>,
    pub stage: String,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
}

/// 错误事件（统一媒体处理事件）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProcessingErrorEvent {
    pub file_id: String,
    pub error: String,
    pub stage: String,
    pub media_type: String,
}

// 兼容旧事件类型别名
pub type PdfProcessingProgressEvent = MediaProcessingProgressEvent;
pub type PdfProcessingCompletedEvent = MediaProcessingCompletedEvent;
pub type PdfProcessingErrorEvent = MediaProcessingErrorEvent;

// ============================================================================
// OCR 处理常量
// ============================================================================

/// OCR 最大并发数
const MAX_OCR_CONCURRENCY: usize = 4;
/// OCR 最大重试次数
const MAX_OCR_RETRY_ATTEMPTS: usize = 3;
/// 初始退避时间（毫秒）
const INITIAL_BACKOFF_MS: u64 = 1000;
/// 最大退避时间（毫秒）
const MAX_BACKOFF_MS: u64 = 20_000;

// ============================================================================
// OCR 结果类型（已移至 vfs/indexing/types.rs）
// ============================================================================

// ============================================================================
// 服务实现
// ============================================================================

// ============================================================================
// 图片压缩配置
// ============================================================================

/// 图片压缩配置
pub struct ImageCompressionConfig {
    /// 是否启用压缩（默认 true）
    pub enabled: bool,
    /// 压缩阈值：超过此大小才压缩（默认 1MB）
    pub size_threshold: usize,
    /// 像素阈值：超过此像素才压缩（默认 2 百万像素 = 2MP）
    pub pixel_threshold: usize,
    /// 压缩质量（默认 "medium"）
    pub quality: String, // "low" | "medium" | "high" | "auto"
}

impl Default for ImageCompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            size_threshold: 1 * 1024 * 1024, // 1MB
            pixel_threshold: 2_000_000,      // 2MP
            quality: "medium".to_string(),
        }
    }
}

// ============================================================================
// 服务实现
// ============================================================================

/// 向量索引回调类型
///
/// 由 VfsIndexCoordinator 注入，避免 PdfProcessingService 直接依赖
/// VfsFullIndexingService，从而打破 indexing ↔ pdf_processing_service 循环。
pub type VectorIndexCallback =
    Arc<dyn Fn(String /* resource_id */) -> Pin<Box<dyn Future<Output = VfsResult<()>> + Send>>
        + Send
        + Sync>;

/// 媒体预处理服务（PDF + 图片）
///
/// ## 依赖
/// - `db`: VFS 数据库（用于获取 blob 路径和更新 OCR 结果）
/// - `llm_manager`: LLM 管理器（用于调用 OCR API）
/// - `file_manager`: 文件管理器（用于解析图片路径和图片压缩）
pub struct PdfProcessingService {
    /// VFS 数据库
    db: Arc<VfsDatabase>,
    /// 设置数据库（用于 OCR 策略）
    settings_db: Arc<Database>,
    /// LLM 管理器（用于 OCR API 调用）
    llm_manager: Arc<LLMManager>,
    /// 文件管理器（用于图片路径解析）
    file_manager: Arc<FileManager>,
    /// 运行中的任务追踪：file_id -> (CancellationToken, generation)
    /// ★ P0 修复：增加 generation 标识，避免 cancel+restart 竞态条件
    running_tasks: DashMap<String, (CancellationToken, u64)>,
    /// ★ perf-audit A4#3: 进度持久化节流状态 file_id -> (stage, percent桶, ready_modes签名, 上次写入)
    /// 旧实现每 tick 全量 UPDATE——300 页 ≈600 次 DB 写;现仅在阶段/模式变化或 ≥2s 时落盘
    progress_persist_state: DashMap<String, (String, u32, String, std::time::Instant)>,
    /// 任务 generation 计数器（单调递增，用于区分同 file_id 的不同任务）
    generation_counter: AtomicU64,
    /// App Handle（用于发送事件）
    app_handle: RwLock<Option<AppHandle>>,
    /// 向量索引回调（由协调器注入，打破循环依赖）
    vector_index_callback: Option<VectorIndexCallback>,
    /// ★ Fix 2: 强制 OCR 的文件集合（前端手动触发时标记，绕过配置检查）
    force_ocr_set: DashSet<String>,
}

impl PdfProcessingService {
    /// 创建新的 PDF 预处理服务
    ///
    /// ## 参数
    /// - `db`: VFS 数据库
    /// - `llm_manager`: LLM 管理器（用于 OCR API）
    /// - `file_manager`: 文件管理器
    pub fn new(
        db: Arc<VfsDatabase>,
        settings_db: Arc<Database>,
        llm_manager: Arc<LLMManager>,
        file_manager: Arc<FileManager>,
    ) -> Self {
        Self {
            db,
            settings_db,
            llm_manager,
            file_manager,
            running_tasks: DashMap::new(),
            progress_persist_state: DashMap::new(),
            generation_counter: AtomicU64::new(0),
            app_handle: RwLock::new(None),
            vector_index_callback: None,
            force_ocr_set: DashSet::new(),
        }
    }

    /// 设置向量索引回调（由 VfsIndexCoordinator 调用）
    pub fn set_vector_index_callback(&mut self, callback: VectorIndexCallback) {
        self.vector_index_callback = Some(callback);
    }

    fn load_ocr_config(&self) -> OcrStrategyConfig {
        OcrStrategyConfig::load_from_db(&self.settings_db)
    }

    /// ★ Fix 2: 标记文件为强制 OCR（前端手动触发时调用，绕过配置检查）
    pub fn mark_force_ocr(&self, file_id: &str) {
        self.force_ocr_set.insert(file_id.to_string());
        info!(
            "[PdfProcessingService] Marked file {} for force OCR (manual trigger)",
            file_id
        );
    }

    /// ★ Fix 2: 清除强制 OCR 标记
    pub fn unmark_force_ocr(&self, file_id: &str) {
        self.force_ocr_set.remove(file_id);
    }

    /// 设置 App Handle
    pub async fn set_app_handle(&self, app_handle: AppHandle) {
        let mut handle = self.app_handle.write().await;
        *handle = Some(app_handle);
    }

    /// 获取 App Handle
    async fn get_app_handle(&self) -> Option<AppHandle> {
        let handle = self.app_handle.read().await;
        handle.clone()
    }

    fn is_task_generation_current(&self, file_id: &str, generation: u64) -> bool {
        self.running_tasks
            .get(file_id)
            .map(|entry| entry.value().1 == generation)
            .unwrap_or(false)
    }

    fn skip_stale_task_side_effects(
        &self,
        file_id: &str,
        generation: Option<u64>,
        op: &str,
    ) -> bool {
        let Some(gen) = generation else {
            return false;
        };

        let current = self.is_task_generation_current(file_id, gen);
        if !current {
            debug!(
                "[MediaProcessingService] Skip stale side effect: file={}, gen={}, op={}",
                file_id, gen, op
            );
        }
        !current
    }

    /// 启动媒体预处理流水线
    ///
    /// 异步执行，立即返回。处理在后台进行。
    /// 自动检测媒体类型（PDF/图片）并选择正确的处理流程。
    ///
    /// ## 参数
    /// - `file_id`: 文件 ID
    /// - `start_from_stage`: 从哪个阶段开始
    ///   - PDF: 默认从 OCR 阶段开始（文本提取和页面渲染已在上传时完成）
    ///   - 图片: 默认从压缩阶段开始
    pub async fn start_pipeline(
        self: &Arc<Self>,
        file_id: &str,
        start_from_stage: Option<ProcessingStage>,
    ) -> VfsResult<()> {
        let file_id = file_id.to_string();

        // 检测媒体类型
        let media_type = self.detect_media_type(&file_id)?;
        info!(
            "[MediaProcessingService] Detected media type: {:?} for file: {}",
            media_type, file_id
        );

        // 根据媒体类型选择默认起始阶段
        let start_stage = start_from_stage.unwrap_or_else(|| match media_type {
            MediaType::Pdf => ProcessingStage::OcrProcessing,
            MediaType::Image => ProcessingStage::ImageCompression,
        });

        // ★ P0 修复：使用原子操作检查并插入，避免 TOCTOU 竞态条件
        // ★ P0-2 修复：使用 generation counter 避免 cancel+restart 竞态
        let cancel_token = CancellationToken::new();
        let generation = self.generation_counter.fetch_add(1, Ordering::SeqCst);
        {
            use dashmap::mapref::entry::Entry;
            match self.running_tasks.entry(file_id.clone()) {
                Entry::Occupied(_) => {
                    warn!(
                        "[MediaProcessingService] Pipeline already running for file: {}",
                        file_id
                    );
                    return Ok(());
                }
                Entry::Vacant(entry) => {
                    entry.insert((cancel_token.clone(), generation));
                }
            }
        }

        // 更新数据库状态
        // ★ P0-3 修复：初始状态写失败（如 SQLITE_BUSY 重试耗尽）时必须移除刚插入的
        // running_tasks 条目 —— 清理 guard 在未 spawn 的 worker 内，条目否则会永久残留，
        // 后续所有 start_pipeline 命中 Occupied 分支静默 no-op，任务永远卡死。
        if let Err(e) = self
            .update_processing_status(&file_id, start_stage, None, None, Some(generation))
            .await
        {
            self.running_tasks.remove(&file_id);
            return Err(e);
        }

        // 克隆 self 用于异步任务
        let service = Arc::clone(self);
        let file_id_clone = file_id.clone();
        let initial_stage = start_stage; // 保存初始阶段用于错误报告
        let mt = media_type; // 捕获媒体类型

        // 启动后台任务
        tokio::spawn(async move {
            // ★ P0 修复：使用 defer 模式确保 running_tasks 在任何情况下都被清理
            // 包括 panic、早期 return、正常完成
            let file_id_for_cleanup = file_id_clone.clone();
            let service_for_cleanup = Arc::clone(&service);

            // 使用 scopeguard 模式：创建一个在 drop 时清理的闭包
            struct CleanupOnDrop<F: FnOnce()>(Option<F>);
            impl<F: FnOnce()> Drop for CleanupOnDrop<F> {
                fn drop(&mut self) {
                    if let Some(f) = self.0.take() {
                        f();
                    }
                }
            }

            // ★ P0-2 修复：cleanup guard 仅删除同 generation 的条目
            // 防止 cancel+start 场景中旧任务的 guard 误删新任务条目
            let task_generation = generation;
            let _cleanup_guard = CleanupOnDrop(Some(move || {
                service_for_cleanup
                    .running_tasks
                    .remove_if(&file_id_for_cleanup, |_, (_, gen)| *gen == task_generation);
                debug!(
                    "[MediaProcessingService] Task cleanup guard triggered for file: {} (gen={})",
                    file_id_for_cleanup, task_generation
                );
            }));

            let result = match mt {
                MediaType::Pdf => {
                    service
                        .run_pdf_pipeline_internal(
                            &file_id_clone,
                            start_stage,
                            cancel_token.clone(),
                            generation,
                        )
                        .await
                }
                MediaType::Image => {
                    service
                        .run_image_pipeline_internal(
                            &file_id_clone,
                            start_stage,
                            cancel_token.clone(),
                            generation,
                        )
                        .await
                }
            };

            // 注意：running_tasks.remove 现在由 _cleanup_guard 在 drop 时自动执行

            match result {
                Ok(()) => {
                    info!(
                        "[MediaProcessingService] Pipeline completed for file: {} (type: {:?})",
                        file_id_clone, mt
                    );
                }
                Err(e) => {
                    error!(
                        "[MediaProcessingService] Pipeline failed for file {} (type: {:?}): {}",
                        file_id_clone, mt, e
                    );
                    // 更新数据库状态为 error（使用初始阶段作为失败阶段）
                    if let Err(db_err) = service.set_error(
                        &file_id_clone,
                        &e.to_string(),
                        initial_stage,
                        Some(generation),
                    ) {
                        error!(
                            "[MediaProcessingService] Failed to set error status: {}",
                            db_err
                        );
                    }
                    // 发送错误事件（统一事件）
                    if let Some(app_handle) = service.get_app_handle().await {
                        let _ = app_handle.emit(
                            "media-processing-error",
                            MediaProcessingErrorEvent {
                                file_id: file_id_clone.clone(),
                                error: e.to_string(),
                                stage: initial_stage.as_str().to_string(),
                                media_type: mt.as_str().to_string(),
                            },
                        );
                    }
                }
            }
        });

        info!(
            "[MediaProcessingService] Pipeline started for file: {} (type: {:?}, from stage: {:?})",
            file_id, media_type, start_stage
        );

        Ok(())
    }

    /// 检测媒体类型
    fn detect_media_type(&self, file_id: &str) -> VfsResult<MediaType> {
        let conn = self.db.get_conn_safe()?;
        // ★ 使用 Option<String> 处理 NULL mime_type
        let mime_type: Option<String> = conn
            .query_row(
                "SELECT mime_type FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(|e| VfsError::Database(format!("Failed to get mime_type: {}", e)))?;

        // 如果 mime_type 存在且可识别，直接使用
        if let Some(ref mime) = mime_type {
            if let Some(mt) = MediaType::from_mime(mime) {
                return Ok(mt);
            }
        }

        // ★ 回退：从文件名推断媒体类型
        let file_name: String = conn
            .query_row(
                "SELECT file_name FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(|e| VfsError::Database(format!("Failed to get file_name: {}", e)))?;

        let lower = file_name.to_lowercase();
        if lower.ends_with(".pdf") {
            info!(
                "[MediaProcessingService] Inferred PDF type from extension for file {} (mime_type was {:?})",
                file_id, mime_type
            );
            return Ok(MediaType::Pdf);
        }
        if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
            || lower.ends_with(".gif") || lower.ends_with(".bmp") || lower.ends_with(".webp")
        {
            return Ok(MediaType::Image);
        }

        // ★ 最后回退：如果 file_type 列标记为 document，默认视为 PDF
        let file_type: Option<String> = conn
            .query_row(
                "SELECT file_type FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .ok();
        if let Some(ref ft) = file_type {
            if ft == "document" {
                info!(
                    "[MediaProcessingService] Defaulting to PDF for file {} (file_type=document, mime_type={:?})",
                    file_id, mime_type
                );
                return Ok(MediaType::Pdf);
            }
        }

        Err(VfsError::InvalidArgument {
            param: "mime_type".to_string(),
            reason: format!(
                "Unsupported media type: {:?} for file: {} (name: {})",
                mime_type, file_id, file_name
            ),
        })
    }

    /// PDF 流水线内部执行
    async fn run_pdf_pipeline_internal(
        &self,
        file_id: &str,
        start_stage: ProcessingStage,
        cancel_token: CancellationToken,
        generation: u64,
    ) -> VfsResult<()> {
        // ★ Fix 3: 保证 force_ocr 标记在任何退出路径上被清理
        // （正常完成、错误、取消、panic）。使用本地守卫，在 drop 时自动清理。
        struct ForceOcrGuard<'a> {
            service: &'a PdfProcessingService,
            file_id: String,
        }
        impl<'a> Drop for ForceOcrGuard<'a> {
            fn drop(&mut self) {
                self.service.force_ocr_set.remove(&self.file_id);
            }
        }
        let _force_ocr_guard = ForceOcrGuard {
            service: self,
            file_id: file_id.to_string(),
        };

        // 获取文件信息
        let conn = self.db.get_conn_safe()?;
        let (page_count, has_extracted_text, extracted_text_len, has_preview, has_ocr): (
            Option<i32>,
            bool,
            i64,
            bool,
            bool,
        ) = conn
            .query_row(
                r#"
                SELECT page_count,
                       extracted_text IS NOT NULL,
                       COALESCE(LENGTH(extracted_text), 0),
                       preview_json IS NOT NULL,
                       ocr_pages_json IS NOT NULL
                FROM files WHERE id = ?1
                "#,
                params![file_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i32>(1)? != 0,
                        row.get(2)?,
                        row.get::<_, i32>(3)? != 0,
                        row.get::<_, i32>(4)? != 0,
                    ))
                },
            )
            .map_err(|e| VfsError::Database(format!("Failed to get file info: {}", e)))?;

        let mut total_pages = page_count.unwrap_or(0) as usize;
        let extracted_text_len = extracted_text_len.max(0) as usize;
        let ocr_config = self.load_ocr_config();

        // 确定初始就绪模式
        // ★ P0 架构改造：image 模式必须等到页面压缩完成后才就绪
        let mut ready_modes: Vec<String> = vec![];
        let mut issues: Vec<ProcessingIssue> = Vec::new();
        if has_extracted_text {
            ready_modes.push("text".to_string());
        }
        // 注意：不再根据 has_preview 直接添加 image，需要检查压缩状态
        if has_ocr {
            ready_modes.push("ocr".to_string());
        }

        // ★ P0 架构改造：Stage 2.5 页面压缩（在 OCR 之前）
        // 为每个 PDF 页面生成压缩版本，发送时直接使用压缩版本
        if has_preview {
            if cancel_token.is_cancelled() {
                info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                let _ = self
                    .update_processing_status(
                        file_id,
                        ProcessingStage::Pending,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;
                return Ok(());
            }

            // 获取 preview_json
            let preview_json: Option<String> = conn
                .query_row(
                    "SELECT preview_json FROM files WHERE id = ?1",
                    params![file_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| VfsError::Database(format!("Failed to get preview_json: {}", e)))?;

            if let Some(ref pj) = preview_json {
                // 检查是否已经有压缩版本
                let needs_compression = self.check_pdf_pages_need_compression(pj)?;

                if needs_compression {
                    info!(
                        "[PdfProcessingService] Starting page compression for file: {} ({} pages)",
                        file_id, total_pages
                    );

                    // 更新状态为 page_compression（★ P0-3：通知性写库，失败不中断流水线）
                    self.try_update_processing_status(
                        file_id,
                        ProcessingStage::PageCompression,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;

                    // 更新状态并发送进度事件（此时 image 还未就绪）
                    // ★ P1-1 修复：压缩范围 5%-20%
                    let progress = ProcessingProgress {
                        stage: "page_compression".to_string(),
                        current_page: Some(0),
                        total_pages: Some(total_pages),
                        percent: 5.0,
                        ready_modes: ready_modes.clone(),
                        media_type: Some("pdf".to_string()),
                        failed_stages: None,
                    };
                    self.try_update_processing_status(
                        file_id,
                        ProcessingStage::PageCompression,
                        Some(&progress),
                        None,
                        Some(generation),
                    )
                    .await;
                    self.emit_progress(file_id, progress, MediaType::Pdf, Some(generation))
                        .await;

                    // 执行页面压缩
                    match self
                        .stage_pdf_page_compression(
                            file_id,
                            pj,
                            total_pages,
                            &cancel_token,
                            generation,
                        )
                        .await
                    {
                        Ok(()) => {
                            // ★ P0 改造：压缩完成后，image 模式才就绪
                            if total_pages > 0 && !ready_modes.contains(&"image".to_string()) {
                                ready_modes.push("image".to_string());
                            } else if total_pages == 0 {
                                warn!(
                                    "[PdfProcessingService] Page compression completed but no pages for file: {}",
                                    file_id
                                );
                            }
                            info!(
                                "[PdfProcessingService] Page compression completed for file: {}, ready_modes: {:?}",
                                file_id, ready_modes
                            );
                        }
                        Err(e) => {
                            warn!(
                                "[PdfProcessingService] Page compression failed for file {}: {}",
                                file_id, e
                            );
                            // ★ P0 修复：压缩失败时仍然标记 image 就绪（使用原图回退）
                            if total_pages > 0 && !ready_modes.contains(&"image".to_string()) {
                                ready_modes.push("image".to_string());
                            } else if total_pages == 0 {
                                warn!(
                                    "[PdfProcessingService] Page compression failed and no pages for file: {}",
                                    file_id
                                );
                            }
                            warn!(
                                "[PdfProcessingService] Using original pages as fallback for file: {}",
                                file_id
                            );
                        }
                    }
                } else {
                    // 已经有压缩版本，image 模式就绪
                    if total_pages > 0 && !ready_modes.contains(&"image".to_string()) {
                        ready_modes.push("image".to_string());
                    } else if total_pages == 0 {
                        warn!(
                            "[PdfProcessingService] Preview has no pages for file: {}",
                            file_id
                        );
                    }
                    info!(
                        "[PdfProcessingService] Pages already compressed for file: {}, ready_modes: {:?}",
                        file_id, ready_modes
                    );
                }
            }
        }

        // Stage 3: OCR 处理（如果需要）
        // ★ FIX 1: Track note_id from OCR for completed event
        let mut ocr_note_id: Option<String> = None;
        // ★ Fix 2: 检查是否被标记为强制 OCR（前端手动触发），绕过配置检查
        let is_force_ocr = self.force_ocr_set.contains(file_id);
        let should_run_pdf_ocr = is_force_ocr
            || (ocr_config.enabled
                && ocr_config.ocr_scanned_pdf
                && !ocr_config.skip_for_multimodal
                && extracted_text_len < ocr_config.pdf_text_threshold);

        if start_stage <= ProcessingStage::OcrProcessing && !has_ocr {
            if !should_run_pdf_ocr {
                info!(
                    "[PdfProcessingService] OCR skipped for file {}: force_ocr={}, enabled={}, ocr_scanned_pdf={}, skip_for_multimodal={}, text_len={}, threshold={}",
                    file_id,
                    is_force_ocr,
                    ocr_config.enabled,
                    ocr_config.ocr_scanned_pdf,
                    ocr_config.skip_for_multimodal,
                    extracted_text_len,
                    ocr_config.pdf_text_threshold
                );
            } else {
                if cancel_token.is_cancelled() {
                    info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                    let _ = self
                        .update_processing_status(
                            file_id,
                            ProcessingStage::Pending,
                            None,
                            None,
                            Some(generation),
                        )
                        .await;
                    return Ok(());
                }

                // 更新状态（★ P0-3：通知性写库，失败不中断流水线）
                self.try_update_processing_status(
                    file_id,
                    ProcessingStage::OcrProcessing,
                    None,
                    None,
                    Some(generation),
                )
                .await;

                // 发送进度事件
                // ★ P1-1 修复：进度单调递增（压缩 0-20% → OCR 20-75% → 向量 75-95%）
                self.emit_progress(
                    file_id,
                    ProcessingProgress {
                        stage: "ocr_processing".to_string(),
                        current_page: Some(0),
                        total_pages: Some(total_pages),
                        percent: 20.0,
                        ready_modes: ready_modes.clone(),
                        media_type: Some("pdf".to_string()),
                        failed_stages: None,
                    },
                    MediaType::Pdf,
                    Some(generation),
                )
                .await;

                // 获取 preview_json 用于 OCR 处理（可能已更新了压缩版本）
                let preview_json: Option<String> = conn
                    .query_row(
                        "SELECT preview_json FROM files WHERE id = ?1",
                        params![file_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| {
                        VfsError::Database(format!("Failed to get preview_json: {}", e))
                    })?;

                // 如果没有缓存 preview_json（历史文件），动态生成
                let preview_json = if let Some(pj) = preview_json {
                    Some(pj)
                } else {
                    info!(
                        "[PdfProcessingService] No cached preview for file {}, attempting dynamic generation...",
                        file_id
                    );

                    // 1. 获取 PDF 文件字节：优先 blob_hash，其次 original_path
                    let pdf_bytes: Option<Vec<u8>> = {
                        let (blob_hash, original_path): (Option<String>, Option<String>) = conn
                            .query_row(
                                "SELECT blob_hash, original_path FROM files WHERE id = ?1",
                                params![file_id],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            )
                            .optional()
                            .map_err(|e| {
                                VfsError::Database(format!("Failed to get file source info: {}", e))
                            })?
                            .unwrap_or((None, None));

                        if let Some(ref hash) = blob_hash {
                            let blobs_dir = self.db.blobs_dir().to_path_buf();
                            match VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, hash)?
                                .and_then(|path| std::fs::read(&path).ok())
                            {
                                Some(bytes) => Some(bytes),
                                None => {
                                    warn!(
                                        "[PdfProcessingService] Blob not found for hash: {}",
                                        hash
                                    );
                                    None
                                }
                            }
                        } else if let Some(ref path_str) = original_path {
                            match std::fs::read(path_str) {
                                Ok(bytes) => Some(bytes),
                                Err(e) => {
                                    warn!(
                                        "[PdfProcessingService] Failed to read original path {}: {}",
                                        path_str, e
                                    );
                                    None
                                }
                            }
                        } else {
                            warn!(
                                "[PdfProcessingService] Neither blob_hash nor original_path available for file: {}",
                                file_id
                            );
                            None
                        }
                    };

                    match pdf_bytes {
                        Some(bytes) => {
                            let bytes_len = bytes.len(); // snapshot before bytes is moved into spawn_blocking
                            let config = PdfPreviewConfig::default();
                            let blobs_dir = self.db.blobs_dir().to_path_buf();
                            let db_clone = self.db.clone();

                            // render_pdf_preview_with_progress 是 CPU 密集型操作，在阻塞线程中运行
                            let gen_result =
                                tokio::task::spawn_blocking(move || {
                                    let conn = db_clone.get_conn_safe()?;
                                    render_pdf_preview_with_progress(
                                        &conn,
                                        &blobs_dir,
                                        &bytes,
                                        &config,
                                        |_, _| {},
                                    )
                                })
                                .await
                                .map_err(|e| {
                                    VfsError::Other(format!(
                                        "Preview generation task panicked: {}",
                                        e
                                    ))
                                })
                                .and_then(|inner| inner);

                            match gen_result {
                                Ok(result) => {
                                    if let Some(pj_ref) = &result.preview_json {
                                        let pj_str = serde_json::to_string(pj_ref)
                                            .map_err(|e| {
                                                VfsError::Serialization(format!(
                                                    "Failed to serialize preview_json: {}",
                                                    e
                                                ))
                                            })?;

                                        // 缓存到 files 表
                                        conn.execute(
                                            "UPDATE files SET preview_json = ?1, extracted_text = ?2, page_count = ?3 WHERE id = ?4",
                                            params![
                                                pj_str,
                                                result.extracted_text.as_deref(),
                                                result.page_count as i32,
                                                file_id,
                                            ],
                                        ).map_err(|e| {
                                            VfsError::Database(format!(
                                                "Failed to cache preview_json: {}",
                                                e
                                            ))
                                        })?;

                                        // 更新 total_pages，使后续进度显示正确
                                        total_pages = result.page_count;

                                        info!(
                                            "[PdfProcessingService] Dynamic preview generated and cached for file: {} ({} pages, {} bytes PDF)",
                                            file_id, result.page_count, bytes_len
                                        );

                                        Some(pj_str)
                                    } else {
                                        warn!(
                                            "[PdfProcessingService] Dynamic preview generation returned no pages for file: {}",
                                            file_id
                                        );
                                        None
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "[PdfProcessingService] Dynamic preview generation failed for file {}: {}",
                                        file_id, e
                                    );
                                    None
                                }
                            }
                        }
                        None => {
                            warn!(
                                "[PdfProcessingService] Cannot generate preview: unable to read PDF content for file: {}",
                                file_id
                            );
                            None
                        }
                    }
                };

                if let Some(ref pj) = preview_json {
                    // 执行 OCR 处理（复用预渲染图片）
                    match self
                        .stage_ocr_processing(
                            file_id,
                            pj,
                            total_pages,
                            &mut ready_modes,
                            &cancel_token,
                            generation,
                        )
                        .await
                    {
                        Ok((_ocr_json, note_id)) => {
                            ocr_note_id = note_id;
                            info!(
                                "[PdfProcessingService] OCR processing completed for file: {}",
                                file_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                "[PdfProcessingService] OCR processing failed for file {}: {}",
                                file_id, e
                            );
                            issues.push(ProcessingIssue {
                                stage: ProcessingStage::OcrProcessing.as_str().to_string(),
                                message: e.to_string(),
                                retriable: true,
                            });
                        }
                    }
                } else {
                    warn!(
                        "[PdfProcessingService] No preview_json available for OCR, skipping file: {}",
                        file_id
                    );
                }
            }
        } else if !has_ocr && !has_preview {
            info!(
                "[PdfProcessingService] OCR skipped: no preview available for file: {}",
                file_id
            );
        }

        // ★ Fix 3: force_ocr 标记由 Fix 3 守卫在函数底部自动清理
        // 如果已有 OCR，添加到就绪模式
        if has_ocr && !ready_modes.contains(&"ocr".to_string()) {
            ready_modes.push("ocr".to_string());
        }

        // Stage 4: 向量索引（如果需要）
        if start_stage <= ProcessingStage::VectorIndexing {
            if cancel_token.is_cancelled() {
                info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                let _ = self
                    .update_processing_status(
                        file_id,
                        ProcessingStage::Pending,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;
                return Ok(());
            }

            // 更新状态（★ P0-3：通知性写库，失败不中断流水线）
            self.try_update_processing_status(
                file_id,
                ProcessingStage::VectorIndexing,
                None,
                None,
                Some(generation),
            )
            .await;

            // 执行向量索引
            // 注意：索引失败不会中断流水线，错误会被记录
            if let Err(e) = self
                .stage_vector_indexing(file_id, &mut ready_modes, MediaType::Pdf, generation)
                .await
            {
                warn!(
                    "[PdfProcessingService] Vector indexing stage failed for file {}: {}",
                    file_id, e
                );
                issues.push(ProcessingIssue {
                    stage: ProcessingStage::VectorIndexing.as_str().to_string(),
                    message: e.to_string(),
                    retriable: true,
                });
            }
        }

        // 标记完成
        let now_ms = chrono::Utc::now().timestamp_millis();
        let completed_with_issues = !issues.is_empty();
        let progress = ProcessingProgress {
            stage: if completed_with_issues {
                ProcessingStage::CompletedWithIssues.as_str().to_string()
            } else {
                ProcessingStage::Completed.as_str().to_string()
            },
            current_page: None,
            total_pages: Some(total_pages),
            percent: 100.0,
            ready_modes: ready_modes.clone(),
            media_type: Some("pdf".to_string()),
            failed_stages: if completed_with_issues {
                Some(issues)
            } else {
                None
            },
        };

        // ★ P0-3 修复：终态写库失败不再误判整条流水线失败。
        // OCR 数据（ocr_pages_json / 笔记）已在 stage_ocr_processing 内落库，
        // 此处仅为状态通知 —— 写失败时照常发送完成事件；
        // 即使 DB 停留在中间态，下次启动 recover_stuck_tasks + ensure_ocr_pipeline 会自愈。
        self.try_update_processing_status(
            file_id,
            if completed_with_issues {
                ProcessingStage::CompletedWithIssues
            } else {
                ProcessingStage::Completed
            },
            Some(&progress),
            Some(now_ms),
            Some(generation),
        )
        .await;

        // 发送完成事件
        self.emit_completed(
            file_id,
            ready_modes,
            if completed_with_issues {
                ProcessingStage::CompletedWithIssues
            } else {
                ProcessingStage::Completed
            },
            MediaType::Pdf,
            Some(generation),
            ocr_note_id,
        )
        .await;

        Ok(())
    }

    // ========================================================================
    // 图片处理流水线（v2.0 新增）
    // ========================================================================

    /// 图片流水线内部执行
    ///
    /// ## 处理流程
    /// - Stage 1: 图片压缩 (image_compression) - 可选，大图才压缩
    /// - Stage 2: OCR 识别 (ocr_processing) - 单张图片 OCR
    /// - Stage 3: 向量索引 (vector_indexing)
    async fn run_image_pipeline_internal(
        &self,
        file_id: &str,
        start_stage: ProcessingStage,
        cancel_token: CancellationToken,
        generation: u64,
    ) -> VfsResult<()> {
        info!(
            "[MediaProcessingService] Starting image pipeline for file: {} (from stage: {:?})",
            file_id, start_stage
        );

        // 获取文件信息
        let conn = self.db.get_conn_safe()?;
        let (blob_hash, resource_id, file_size, has_ocr, mime_type): (
            Option<String>,
            Option<String>,
            Option<i64>,
            bool,
            Option<String>,
        ) = conn
            .query_row(
                r#"
                SELECT f.blob_hash, f.resource_id, f.size, r.ocr_text IS NOT NULL, f.mime_type
                FROM files f
                LEFT JOIN resources r ON r.id = f.resource_id
                WHERE f.id = ?1
                "#,
                params![file_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i32>(3)? != 0,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|e| VfsError::Database(format!("Failed to get file info: {}", e)))?;

        let blobs_dir = self.db.blobs_dir();

        // 初始化就绪模式
        // ★ P0 架构改造：image 模式必须等到压缩完成后才就绪
        // 原因：发送时不再压缩，必须使用预处理的压缩结果
        let mut ready_modes: Vec<String> = vec![];
        let mut issues: Vec<ProcessingIssue> = Vec::new();

        // 图片上传后原图已存在 resources 表，image 模式立即就绪
        // 压缩是优化（减小 base64 体积），不应阻塞用户发送
        ready_modes.push("image".to_string());
        if has_ocr {
            ready_modes.push("ocr".to_string());
        }

        let ocr_config = self.load_ocr_config();
        let should_run_image_ocr =
            ocr_config.enabled && ocr_config.ocr_images && !ocr_config.skip_for_multimodal;

        info!(
            "[OCR_DIAG] Image pipeline OCR decision: file_id={}, has_ocr={}, should_run_image_ocr={}, ocr_config=(enabled={}, ocr_images={}, skip_for_multimodal={}), blob_hash={:?}, resource_id={:?}",
            file_id, has_ocr, should_run_image_ocr,
            ocr_config.enabled, ocr_config.ocr_images, ocr_config.skip_for_multimodal,
            blob_hash, resource_id
        );

        // Stage 1: 图片压缩
        if start_stage <= ProcessingStage::ImageCompression {
            if cancel_token.is_cancelled() {
                info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                let _ = self
                    .update_processing_status(
                        file_id,
                        ProcessingStage::Pending,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;
                return Ok(());
            }

            // 更新状态（★ P0-3：通知性写库，失败不中断流水线）
            self.try_update_processing_status(
                file_id,
                ProcessingStage::ImageCompression,
                None,
                None,
                Some(generation),
            )
            .await;

            // 发送进度事件
            self.emit_progress(
                file_id,
                ProcessingProgress {
                    stage: "image_compression".to_string(),
                    current_page: Some(1),
                    total_pages: Some(1),
                    percent: 10.0,
                    ready_modes: ready_modes.clone(),
                    media_type: Some("image".to_string()),
                    failed_stages: None,
                },
                MediaType::Image,
                Some(generation),
            )
            .await;

            // 执行压缩（如果需要）
            let compression_config = ImageCompressionConfig::default();
            if compression_config.enabled {
                if let Some(ref bh) = blob_hash {
                    match self
                        .stage_image_compression(file_id, bh, file_size, &compression_config)
                        .await
                    {
                        Ok(_compressed) => {
                            // ★ P0 架构改造：压缩完成后，image 模式才就绪
                            if !ready_modes.contains(&"image".to_string()) {
                                ready_modes.push("image".to_string());
                            }
                            info!(
                                "[MediaProcessingService] Image compression completed for file: {}, ready_modes: {:?}",
                                file_id, ready_modes
                            );
                        }
                        Err(e) => {
                            warn!(
                                "[MediaProcessingService] Image compression failed for file {}: {}",
                                file_id, e
                            );
                            // ★ P0 修复：压缩失败时仍然标记 image 就绪（使用原图回退）
                            // 否则用户将无法发送这个附件
                            if !ready_modes.contains(&"image".to_string()) {
                                ready_modes.push("image".to_string());
                            }
                            warn!(
                                "[MediaProcessingService] Using original image as fallback for file: {}",
                                file_id
                            );
                        }
                    }
                } else {
                    // 没有 blob_hash，检查是否有 inline 内容
                    let base64_content =
                        VfsFileRepo::get_content_with_conn(&conn, &blobs_dir, file_id)?;
                    if base64_content.is_some() {
                        if !ready_modes.contains(&"image".to_string()) {
                            ready_modes.push("image".to_string());
                        }
                    } else {
                        warn!(
                            "[MediaProcessingService] Image compression skipped: no content for file {}",
                            file_id
                        );
                    }
                }
            } else {
                // 压缩功能禁用，确保内容存在后标记 image 就绪
                let has_content = if blob_hash.is_some() {
                    true
                } else {
                    VfsFileRepo::get_content_with_conn(&conn, &blobs_dir, file_id)?.is_some()
                };
                if has_content && !ready_modes.contains(&"image".to_string()) {
                    ready_modes.push("image".to_string());
                } else if !has_content {
                    warn!(
                        "[MediaProcessingService] Image compression disabled but content missing for file {}",
                        file_id
                    );
                }
            }

            // 发送进度更新（现在 ready_modes 包含 image）
            self.emit_progress(
                file_id,
                ProcessingProgress {
                    stage: "image_compression".to_string(),
                    current_page: Some(1),
                    total_pages: Some(1),
                    percent: 25.0,
                    ready_modes: ready_modes.clone(),
                    media_type: Some("image".to_string()),
                    failed_stages: None,
                },
                MediaType::Image,
                Some(generation),
            )
            .await;
        }

        // Stage 2: OCR 处理（如果需要）
        if start_stage <= ProcessingStage::OcrProcessing && !has_ocr && should_run_image_ocr {
            info!(
                "[OCR_DIAG] Image OCR Stage 2 ENTERED: file_id={}, start_stage={:?}",
                file_id, start_stage
            );
            if cancel_token.is_cancelled() {
                info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                let _ = self
                    .update_processing_status(
                        file_id,
                        ProcessingStage::Pending,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;
                return Ok(());
            }

            // 更新状态（★ P0-3：通知性写库，失败不中断流水线）
            self.try_update_processing_status(
                file_id,
                ProcessingStage::OcrProcessing,
                None,
                None,
                Some(generation),
            )
            .await;

            // 发送进度事件
            self.emit_progress(
                file_id,
                ProcessingProgress {
                    stage: "ocr_processing".to_string(),
                    current_page: Some(1),
                    total_pages: Some(1),
                    percent: 40.0,
                    ready_modes: ready_modes.clone(),
                    media_type: Some("image".to_string()),
                    failed_stages: None,
                },
                MediaType::Image,
                Some(generation),
            )
            .await;

            // 执行 OCR
            let mt = mime_type.as_deref().unwrap_or("image/png");
            if let Some(ref bh) = blob_hash {
                match self
                    .stage_image_ocr(file_id, bh, mt, &cancel_token, generation)
                    .await
                {
                    Ok(ocr_text) => {
                        // ★ 2026-02-13 修复：仅当 OCR 文本非空时才标记 'ocr' 就绪
                        // 原问题：空文本时 ready_modes 虚标 'ocr'，前端认为 OCR 就绪放行发送，
                        // 但后端 DB 中 ocr_text=NULL，导致模型只收到占位符
                        if !ocr_text.trim().is_empty() {
                            info!(
                                "[MediaProcessingService] Image OCR completed for file: {} ({} chars)",
                                file_id,
                                ocr_text.len()
                            );
                            ready_modes.push("ocr".to_string());
                        } else {
                            warn!(
                                "[MediaProcessingService] Image OCR returned empty text for file: {}, NOT marking 'ocr' as ready",
                                file_id
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[MediaProcessingService] Image OCR failed for file {}: {}",
                            file_id, e
                        );
                        issues.push(ProcessingIssue {
                            stage: ProcessingStage::OcrProcessing.as_str().to_string(),
                            message: e.to_string(),
                            retriable: true,
                        });
                    }
                }
            } else {
                let blobs_dir = self.db.blobs_dir();
                let base64_content =
                    VfsFileRepo::get_content_with_conn(&conn, &blobs_dir, file_id)?;
                if let Some(data) = base64_content {
                    match self
                        .stage_image_ocr_with_base64(file_id, data, mt, &cancel_token, generation)
                        .await
                    {
                        Ok(ocr_text) => {
                            // ★ 2026-02-13 修复：同上，仅非空时标记就绪
                            if !ocr_text.trim().is_empty() {
                                info!(
                                    "[MediaProcessingService] Image OCR completed for file: {} ({} chars)",
                                    file_id,
                                    ocr_text.len()
                                );
                                ready_modes.push("ocr".to_string());
                            } else {
                                warn!(
                                    "[MediaProcessingService] Image OCR returned empty text for file: {}, NOT marking 'ocr' as ready",
                                    file_id
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "[MediaProcessingService] Image OCR failed for file {}: {}",
                                file_id, e
                            );
                            issues.push(ProcessingIssue {
                                stage: ProcessingStage::OcrProcessing.as_str().to_string(),
                                message: e.to_string(),
                                retriable: true,
                            });
                        }
                    }
                } else {
                    warn!(
                        "[MediaProcessingService] Image OCR skipped: no content for file {}",
                        file_id
                    );
                }
            }

            // 发送进度更新
            self.emit_progress(
                file_id,
                ProcessingProgress {
                    stage: "ocr_processing".to_string(),
                    current_page: Some(1),
                    total_pages: Some(1),
                    percent: 60.0,
                    ready_modes: ready_modes.clone(),
                    media_type: Some("image".to_string()),
                    failed_stages: None,
                },
                MediaType::Image,
                Some(generation),
            )
            .await;
        } else if !has_ocr && !should_run_image_ocr {
            info!(
                "[MediaProcessingService] Image OCR skipped for file {}: enabled={}, skip_for_multimodal={}",
                file_id,
                ocr_config.enabled,
                ocr_config.skip_for_multimodal
            );
        }

        // Stage 3: 向量索引
        if start_stage <= ProcessingStage::VectorIndexing && resource_id.is_some() {
            if cancel_token.is_cancelled() {
                info!("[PdfProcessingService] Pipeline cancelled for {}", file_id);
                let _ = self
                    .update_processing_status(
                        file_id,
                        ProcessingStage::Pending,
                        None,
                        None,
                        Some(generation),
                    )
                    .await;
                return Ok(());
            }

            // 更新状态（★ P0-3：通知性写库，失败不中断流水线）
            self.try_update_processing_status(
                file_id,
                ProcessingStage::VectorIndexing,
                None,
                None,
                Some(generation),
            )
            .await;

            // 执行向量索引
            if let Err(e) = self
                .stage_vector_indexing(file_id, &mut ready_modes, MediaType::Image, generation)
                .await
            {
                warn!(
                    "[MediaProcessingService] Vector indexing stage failed for file {}: {}",
                    file_id, e
                );
                issues.push(ProcessingIssue {
                    stage: ProcessingStage::VectorIndexing.as_str().to_string(),
                    message: e.to_string(),
                    retriable: true,
                });
            }
        }

        // 标记完成
        let now_ms = chrono::Utc::now().timestamp_millis();
        let completed_with_issues = !issues.is_empty();
        let progress = ProcessingProgress {
            stage: if completed_with_issues {
                ProcessingStage::CompletedWithIssues.as_str().to_string()
            } else {
                ProcessingStage::Completed.as_str().to_string()
            },
            current_page: Some(1),
            total_pages: Some(1),
            percent: 100.0,
            ready_modes: ready_modes.clone(),
            media_type: Some("image".to_string()),
            failed_stages: if completed_with_issues {
                Some(issues)
            } else {
                None
            },
        };

        // ★ P0-3 修复：终态写库失败不再误判整条流水线失败（同 PDF 流水线）
        self.try_update_processing_status(
            file_id,
            if completed_with_issues {
                ProcessingStage::CompletedWithIssues
            } else {
                ProcessingStage::Completed
            },
            Some(&progress),
            Some(now_ms),
            Some(generation),
        )
        .await;

        // 发送完成事件
        self.emit_completed(
            file_id,
            ready_modes,
            if completed_with_issues {
                ProcessingStage::CompletedWithIssues
            } else {
                ProcessingStage::Completed
            },
            MediaType::Image,
            Some(generation),
            None,
        )
        .await;

        Ok(())
    }

    /// Stage 1: 图片压缩
    ///
    /// ★ P0 架构改造：强制预处理压缩
    /// 对所有图片执行压缩（使用 `low` 质量），存储结果供发送时使用。
    /// 发送时不再进行实时压缩，完全依赖预处理结果。
    ///
    /// ## 返回
    /// - `Ok(true)`: 压缩完成
    /// - `Ok(false)`: 压缩跳过（图片已经很小或压缩效果不明显）
    async fn stage_image_compression(
        &self,
        file_id: &str,
        blob_hash: &str,
        _file_size: Option<i64>,
        _config: &ImageCompressionConfig,
    ) -> VfsResult<bool> {
        // ★ P0 改造：移除大小阈值检查，对所有图片都进行压缩
        // 原因：发送时不再压缩，必须在预处理阶段完成

        // 获取原始图片数据
        let conn = self.db.get_conn_safe()?;
        let blobs_dir = self.db.blobs_dir();
        let has_text: bool = conn
            .query_row(
                "SELECT extracted_text IS NOT NULL FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_or(0)
            != 0;
        let mut ready_modes = Vec::new();
        if has_text {
            ready_modes.push("text".to_string());
        }
        let blob_path = VfsBlobRepo::get_blob_path_with_conn(&conn, blobs_dir, blob_hash)?
            .ok_or_else(|| VfsError::NotFound {
                resource_type: "Blob".to_string(),
                id: blob_hash.to_string(),
            })?;

        // 读取原始图片
        let image_data = tokio::fs::read(&blob_path).await?;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

        // ★ P0 改造：强制使用 `low` 质量进行压缩
        // 与发送时的默认策略保持一致（多图/PDF 场景）
        let compressed_base64 = self
            .file_manager
            .adjust_image_quality_base64(&base64_data, "low");

        let original_size = base64_data.len();
        let compressed_size = compressed_base64.len();

        // 如果压缩后没有显著减少（<10%），使用原始图片但标记为"已压缩"
        // 这样发送时就知道不需要再压缩
        let (_final_data, final_hash) = if compressed_size >= original_size * 9 / 10 {
            info!(
                "[MediaProcessingService] Compression not effective for file {}: {} -> {} bytes, using original",
                file_id, original_size, compressed_size
            );
            // 使用原始数据，但仍然标记 compressed_blob_hash 为原始 hash
            // 这样 VFS 解析时知道已经处理过了
            (image_data.clone(), blob_hash.to_string())
        } else {
            // 解码压缩后的数据
            let compressed_data = base64::engine::general_purpose::STANDARD
                .decode(&compressed_base64)
                .map_err(|e| {
                    VfsError::Other(format!("Failed to decode compressed image: {}", e))
                })?;

            // 计算压缩后的哈希
            let mut hasher = Sha256::new();
            hasher.update(&compressed_data);
            let compressed_hash = format!("{:x}", hasher.finalize());

            // 存储压缩后的 blob（如果还不存在）
            if VfsBlobRepo::get_blob_path_with_conn(&conn, blobs_dir, &compressed_hash)?.is_none() {
                VfsBlobRepo::store_blob_with_conn(
                    &conn,
                    blobs_dir,
                    &compressed_data,
                    Some("image/jpeg"),
                    Some("jpg"),
                )?;
            }

            info!(
                "[MediaProcessingService] Image compressed for file {}: {} -> {} bytes ({:.1}% reduction, hash: {})",
                file_id, original_size, compressed_size,
                (1.0 - compressed_size as f64 / original_size as f64) * 100.0,
                compressed_hash
            );

            (compressed_data, compressed_hash)
        };

        // 更新文件记录，标记压缩已完成
        conn.execute(
            "UPDATE files SET compressed_blob_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![final_hash, file_id],
        )?;

        // 返回 true 表示压缩阶段已完成（无论是否真正压缩）
        Ok(true)
    }

    // ========================================================================
    // ★ P0 架构改造：PDF 页面压缩
    // ========================================================================

    /// 检查 PDF 页面是否需要压缩
    ///
    /// 通过检查 preview_json 中是否已存在 compressed_blob_hash 字段来判断
    fn check_pdf_pages_need_compression(&self, preview_json: &str) -> VfsResult<bool> {
        let preview: PdfPreviewJson = serde_json::from_str(preview_json)
            .map_err(|e| VfsError::Serialization(format!("Failed to parse preview_json: {}", e)))?;

        if preview.pages.is_empty() {
            return Ok(false);
        }

        let conn = self.db.get_conn_safe()?;
        let blobs_dir = self.db.blobs_dir();

        // 只要有任意页面缺少压缩版本或压缩 blob 不存在，就需要压缩
        for page in preview.pages.iter() {
            let Some(ref ch) = page.compressed_blob_hash else {
                return Ok(true);
            };
            if ch.trim().is_empty() {
                return Ok(true);
            }
            if VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, ch)?.is_none() {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Stage 2.5: PDF 页面压缩
    ///
    /// 为每个 PDF 页面生成压缩版本，更新 preview_json 中的 compressed_blob_hash 字段。
    /// 发送时直接使用压缩版本，不再实时压缩。
    async fn stage_pdf_page_compression(
        &self,
        file_id: &str,
        preview_json: &str,
        total_pages: usize,
        cancel_token: &CancellationToken,
        generation: u64,
    ) -> VfsResult<()> {
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let mut preview: PdfPreviewJson = serde_json::from_str(preview_json)
            .map_err(|e| VfsError::Serialization(format!("Failed to parse preview_json: {}", e)))?;

        let conn = self.db.get_conn_safe()?;
        let blobs_dir = self.db.blobs_dir().to_path_buf();
        let app_handle = self.get_app_handle().await;
        let has_text: bool = conn
            .query_row(
                "SELECT extracted_text IS NOT NULL FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_or(0)
            != 0;
        let mut ready_modes = Vec::new();
        if has_text {
            ready_modes.push("text".to_string());
        }
        drop(conn); // Release connection before concurrent processing

        // Collect pages that need compression
        let pages_to_compress: Vec<(usize, crate::vfs::types::PdfPagePreview)> = preview
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.compressed_blob_hash.is_none())
            .map(|(i, p)| (i, p.clone()))
            .collect();

        let already_compressed = preview.pages.len() - pages_to_compress.len();
        if pages_to_compress.is_empty() {
            info!(
                "[PdfProcessingService] All {} pages already have compressed versions for file: {}",
                preview.pages.len(), file_id
            );
            return Ok(());
        }

        info!(
            "[PdfProcessingService] Starting concurrent page compression for file {}: {} pages to compress, {} already done",
            file_id, pages_to_compress.len(), already_compressed
        );

        // Concurrent compression with semaphore
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_OCR_CONCURRENCY));
        let compressed_count = Arc::new(AtomicUsize::new(already_compressed));
        let skipped_count = Arc::new(AtomicUsize::new(0));
        // Store results: (page_index, compressed_blob_hash)
        let page_results: Arc<Mutex<Vec<(usize, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let db = self.db.clone();
        let file_manager = self.file_manager.clone();
        let file_id_owned = file_id.to_string();
        let service = self;

        let tasks: Vec<_> = pages_to_compress
            .iter()
            .map(|(index, page)| {
                let page_index = *index;
                let blob_hash = page.blob_hash.clone();
                let blobs_dir = blobs_dir.clone();
                let semaphore = semaphore.clone();
                let compressed_count = compressed_count.clone();
                let skipped_count = skipped_count.clone();
                let page_results = page_results.clone();
                let cancel_token = cancel_token.clone();
                let db = db.clone();
                let file_manager = file_manager.clone();
                let file_id = file_id_owned.clone();
                let ready_modes_for_task = ready_modes.clone();
                let service = service;
                let generation_for_task = generation;
                let total_pages = total_pages;
                let app_handle = app_handle.clone();

                async move {
                    let _permit = match semaphore.acquire().await {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("[PdfProcessingService] Compression semaphore error: {}", e);
                            return;
                        }
                    };

                    if cancel_token.is_cancelled() {
                        return;
                    }

                    // Get own DB connection
                    let conn = match db.get_conn_safe() {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("[PdfProcessingService] Failed to get connection for page {}: {}", page_index, e);
                            return;
                        }
                    };

                    // Get blob path
                    let blob_path = match VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &blob_hash) {
                        Ok(Some(path)) => path,
                        Ok(None) => {
                            warn!("[PdfProcessingService] Blob not found for page {}: {}", page_index, blob_hash);
                            return;
                        }
                        Err(e) => {
                            warn!("[PdfProcessingService] Blob lookup error for page {}: {}", page_index, e);
                            return;
                        }
                    };

                    // Read image data
                    let image_data = match tokio::fs::read(&blob_path).await {
                        Ok(data) => data,
                        Err(e) => {
                            warn!("[PdfProcessingService] Failed to read page {} blob: {}", page_index, e);
                            return;
                        }
                    };

                    let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);
                    let original_size = base64_data.len();
                    let compressed_base64 = file_manager.adjust_image_quality_base64(&base64_data, "low");
                    let compressed_size = compressed_base64.len();

                    let result_hash = if compressed_size >= original_size * 9 / 10 {
                        // Compression not effective, use original hash
                        skipped_count.fetch_add(1, Ordering::SeqCst);
                        debug!("[PdfProcessingService] Page {} compression not effective, using original", page_index);
                        Some(blob_hash.clone())
                    } else {
                        // Store compressed blob
                        let compressed_data = match base64::engine::general_purpose::STANDARD.decode(&compressed_base64) {
                            Ok(d) => d,
                            Err(e) => {
                                warn!("[PdfProcessingService] Failed to decode compressed page {}: {}", page_index, e);
                                return;
                            }
                        };

                        let mut hasher = Sha256::new();
                        hasher.update(&compressed_data);
                        let compressed_hash = format!("{:x}", hasher.finalize());

                        if VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &compressed_hash)
                            .ok()
                            .flatten()
                            .is_none()
                        {
                            if let Err(e) = VfsBlobRepo::store_blob_with_conn(
                                &conn, &blobs_dir, &compressed_data,
                                Some("image/jpeg"), Some("jpg"),
                            ) {
                                warn!("[PdfProcessingService] Failed to store compressed blob for page {}: {}", page_index, e);
                                return;
                            }
                        }

                        debug!(
                            "[PdfProcessingService] Page {} compressed: {} -> {} bytes ({:.1}% reduction)",
                            page_index, original_size, compressed_size,
                            (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                        );
                        Some(compressed_hash)
                    };

                    let completed = compressed_count.fetch_add(1, Ordering::SeqCst) + 1;
                    page_results.lock().await.push((page_index, result_hash));

                    // Emit progress
                    if app_handle.is_some() {
                        let progress_percent = if total_pages == 0 {
                            5.0
                        } else {
                            5.0 + (completed as f32 / total_pages as f32) * 15.0
                        };
                        let progress = ProcessingProgress {
                            stage: "page_compression".to_string(),
                            current_page: Some(completed),
                            total_pages: Some(total_pages),
                            percent: progress_percent,
                            ready_modes: ready_modes_for_task.clone(),
                            media_type: Some("pdf".to_string()),
                            failed_stages: None,
                        };
                        service.emit_progress(&file_id, progress, MediaType::Pdf, Some(generation_for_task)).await;
                    }
                }
            })
            .collect();

        stream::iter(tasks)
            .for_each_concurrent(MAX_OCR_CONCURRENCY, |task| task)
            .await;

        // Apply results back to preview
        let results = page_results.lock().await;
        for (page_index, compressed_hash) in results.iter() {
            if let Some(page) = preview.pages.get_mut(*page_index) {
                page.compressed_blob_hash = compressed_hash.clone();
            }
        }

        // Update preview_json in DB
        let conn = self.db.get_conn_safe()?;
        let updated_preview_json = serde_json::to_string(&preview)
            .map_err(|e| VfsError::Other(format!("Failed to serialize preview_json: {}", e)))?;

        conn.execute(
            "UPDATE files SET preview_json = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![updated_preview_json, file_id],
        )?;

        info!(
            "[PdfProcessingService] Page compression completed for file {}: {} total pages processed",
            file_id, preview.pages.len()
        );

        Ok(())
    }

    /// Stage 2: 图片 OCR
    ///
    /// 对单张图片进行 OCR 识别。
    async fn stage_image_ocr(
        &self,
        file_id: &str,
        blob_hash: &str,
        mime_type: &str,
        cancel_token: &CancellationToken,
        generation: u64,
    ) -> VfsResult<String> {
        // 获取图片数据
        let conn = self.db.get_conn_safe()?;
        let blobs_dir = self.db.blobs_dir();
        let blob_path = VfsBlobRepo::get_blob_path_with_conn(&conn, blobs_dir, blob_hash)?
            .ok_or_else(|| VfsError::NotFound {
                resource_type: "Blob".to_string(),
                id: blob_hash.to_string(),
            })?;

        // 读取图片并转为 base64
        let image_data = tokio::fs::read(&blob_path).await?;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);
        self.stage_image_ocr_with_base64(file_id, base64_data, mime_type, cancel_token, generation)
            .await
    }

    /// 使用 base64 直接执行图片 OCR（支持 inline 图片）
    async fn stage_image_ocr_with_base64(
        &self,
        file_id: &str,
        base64_data: String,
        mime_type: &str,
        cancel_token: &CancellationToken,
        generation: u64,
    ) -> VfsResult<String> {
        if cancel_token.is_cancelled()
            || self.skip_stale_task_side_effects(file_id, Some(generation), "stage_image_ocr:start")
        {
            return Ok(String::new());
        }

        info!(
            "[OCR_DIAG] stage_image_ocr_with_base64 START: file_id={}, mime_type={}, base64_len={}",
            file_id,
            mime_type,
            base64_data.len()
        );

        let conn = self.db.get_conn_safe()?;

        // ★ 诊断：检查 file_id 关联的 resource_id
        let resource_id_check: Option<String> = conn
            .query_row(
                "SELECT resource_id FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        info!(
            "[OCR_DIAG] file->resource mapping: file_id={} -> resource_id={:?}",
            file_id, resource_id_check
        );

        // 调用 OCR API
        use crate::llm_manager::ImagePayload;
        let adapter = self.llm_manager.get_ocr_adapter().await;
        info!(
            "[OCR_DIAG] OCR adapter obtained, calling OCR model for file_id={}",
            file_id
        );
        // 使用适配器官方 prompt（DeepSeek-OCR → "Free OCR.", PaddleOCR-VL → "OCR:" 等）
        // 注意：不要追加自定义中文指令，专用 OCR 模型只接受其官方 prompt 格式
        let prompt = adapter.build_prompt(crate::ocr_adapters::OcrMode::FreeOcr);
        let image_payload = ImagePayload {
            mime: mime_type.to_string(),
            base64: base64_data,
        };

        let result = self
            .llm_manager
            .call_ocr_model_raw_prompt(&prompt, Some(vec![image_payload]))
            .await
            .map_err(|e| {
                warn!(
                    "[OCR_DIAG] OCR API call FAILED for file_id={}: {}",
                    file_id, e
                );
                VfsError::Other(format!("OCR API call failed: {}", e))
            })?;

        if cancel_token.is_cancelled()
            || self.skip_stale_task_side_effects(
                file_id,
                Some(generation),
                "stage_image_ocr:after_api",
            )
        {
            return Ok(String::new());
        }

        let ocr_text = result.assistant_message;
        info!(
            "[OCR_DIAG] OCR API returned for file_id={}: text_len={}, preview=\"{}\"",
            file_id,
            ocr_text.len(),
            ocr_text.chars().take(100).collect::<String>()
        );

        // ★ 2026-02 修复：OCR 返回空文本时不写入数据库
        // 空字符串会导致 has_ocr_text=true 但 ocr_text_len=0，
        // 后续 get_image_ocr_text 读取时会判定为 EMPTY 并返回 None，
        // 最终导致用户选择 OCR 模式但拿不到任何内容。
        // 不写入空值，让 ocr_text 保持 NULL，后续重试时可以重新触发 OCR。
        if ocr_text.trim().is_empty() {
            warn!(
                "[OCR_DIAG] OCR API returned EMPTY text for file_id={}, NOT writing to DB (keeping ocr_text as NULL so retry is possible). \
                 Possible causes: (1) OCR model does not support vision, (2) API returned empty response, (3) image has no recognizable text",
                file_id
            );
            return Ok(ocr_text);
        }

        // 存储 OCR 结果到关联的 resource.ocr_text
        let rows_affected = conn.execute(
            r#"
            UPDATE resources SET ocr_text = ?1, updated_at = datetime('now')
            WHERE id = (SELECT resource_id FROM files WHERE id = ?2)
            "#,
            params![ocr_text, file_id],
        )?;

        // ★ 2026-02-13 修复：rows_affected=0 说明 resource_id 映射失败，OCR 文本未持久化
        // 此时若返回 Ok，调用方会将 'ocr' 加入 ready_modes，但后端查询时找不到数据
        if rows_affected == 0 {
            warn!(
                "[OCR_DIAG] OCR text NOT persisted: file_id={}, rows_affected=0. \
                 resource_id lookup failed (files.resource_id may be NULL). \
                 Returning error to prevent ready_modes from falsely including 'ocr'.",
                file_id
            );
            return Err(VfsError::Other(format!(
                "OCR text produced but DB save failed (rows_affected=0) for file_id={}",
                file_id
            )));
        }

        info!(
            "[OCR_DIAG] OCR text saved to resources table: file_id={}, rows_affected={}",
            file_id, rows_affected
        );

        // 更新 processing_progress 中的 ready_modes
        let progress_json: Option<String> = conn
            .query_row(
                "SELECT processing_progress FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(ref pj) = progress_json {
            if let Ok(mut progress) = serde_json::from_str::<ProcessingProgress>(pj) {
                if !progress.ready_modes.contains(&"ocr".to_string()) {
                    progress.ready_modes.push("ocr".to_string());
                    if let Ok(new_json) = serde_json::to_string(&progress) {
                        conn.execute(
                            "UPDATE files SET processing_progress = ?1, updated_at = datetime('now') WHERE id = ?2",
                            params![new_json, file_id],
                        )?;
                    }
                }
            }
        }

        Ok(ocr_text)
    }

    /// 更新数据库中的处理状态（支持媒体类型）
    async fn update_processing_status(
        &self,
        file_id: &str,
        stage: ProcessingStage,
        progress: Option<&ProcessingProgress>,
        completed_at: Option<i64>,
        generation: Option<u64>,
    ) -> VfsResult<()> {
        if self.skip_stale_task_side_effects(file_id, generation, "update_processing_status") {
            return Ok(());
        }

        // ★ P1-4 修复：带 busy-retry 的事务开始
        // 并发处理多文件时 BEGIN IMMEDIATE 可能因 SQLITE_BUSY 失败
        // 连接在循环内获取，避免 sleep 期间持有空闲连接导致连接池饥饿
        let conn = {
            let max_retries = 3u32;
            let mut attempt = 0u32;
            loop {
                let conn = self.db.get_conn_safe()?;
                match conn.execute("BEGIN IMMEDIATE", []) {
                    Ok(_) => break conn,
                    Err(e) if attempt < max_retries => {
                        let msg = e.to_string();
                        if msg.contains("database is locked") || msg.contains("SQLITE_BUSY") {
                            attempt += 1;
                            let backoff_ms = 50 * (1u64 << attempt); // 100ms, 200ms, 400ms
                            warn!(
                                "[PdfProcessingService] BEGIN IMMEDIATE busy for file {}, retry {}/{} in {}ms",
                                file_id, attempt, max_retries, backoff_ms
                            );
                            drop(conn);
                            sleep(std::time::Duration::from_millis(backoff_ms)).await;
                            continue;
                        }
                        return Err(VfsError::Database(format!("BEGIN IMMEDIATE failed: {}", e)));
                    }
                    Err(e) => {
                        return Err(VfsError::Database(format!(
                            "BEGIN IMMEDIATE failed after {} retries: {}",
                            max_retries, e
                        )));
                    }
                }
            }
        };

        let result = (|| -> VfsResult<()> {
            let progress_json = progress.map(|p| serde_json::to_string(p).unwrap_or_default());
            let now_ms = chrono::Utc::now().timestamp_millis();

            if stage == ProcessingStage::Completed {
                // 完成
                conn.execute(
                    r#"
                    UPDATE files
                    SET processing_status = ?1,
                        processing_progress = ?2,
                        processing_completed_at = ?3,
                        processing_error = NULL,
                        updated_at = datetime('now')
                    WHERE id = ?4
                    "#,
                    params![
                        stage.as_str(),
                        progress_json,
                        completed_at.unwrap_or(now_ms),
                        file_id
                    ],
                )?;
            } else if stage == ProcessingStage::CompletedWithIssues {
                let warning_summary = progress
                    .and_then(|p| p.failed_stages.as_ref())
                    .map(|issues| {
                        issues
                            .iter()
                            .map(|i| format!("{}: {}", i.stage, i.message))
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|s| !s.trim().is_empty());
                conn.execute(
                    r#"
                    UPDATE files
                    SET processing_status = ?1,
                        processing_progress = ?2,
                        processing_completed_at = ?3,
                        processing_error = COALESCE(?4, processing_error),
                        updated_at = datetime('now')
                    WHERE id = ?5
                    "#,
                    params![
                        stage.as_str(),
                        progress_json,
                        completed_at.unwrap_or(now_ms),
                        warning_summary,
                        file_id
                    ],
                )?;
            } else if stage == ProcessingStage::Error {
                // 错误 - 不设置 completed_at
                conn.execute(
                    r#"
                    UPDATE files
                    SET processing_status = ?1,
                        processing_progress = ?2,
                        updated_at = datetime('now')
                    WHERE id = ?3
                    "#,
                    params![stage.as_str(), progress_json, file_id],
                )?;
            } else {
                // 处理中
                conn.execute(
                    r#"
                    UPDATE files
                    SET processing_status = ?1,
                        processing_progress = ?2,
                        processing_started_at = COALESCE(processing_started_at, ?3),
                        updated_at = datetime('now')
                    WHERE id = ?4
                    "#,
                    params![stage.as_str(), progress_json, now_ms, file_id],
                )?;
            }

            Ok(())
        })();

        // 提交或回滚
        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!(
                    "[PdfProcessingService] Updated status for file {}: {:?}",
                    file_id, stage
                );
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// ★ P0-3 修复：通知性状态写库（非致命版）
    ///
    /// 流水线中间/终态的 processing_status 落库仅用于进度展示与轮询，
    /// 瞬时的 SQLITE_BUSY 不应让整条流水线误判为失败（否则 OCR 数据已写好
    /// 却仍触发 set_error + 错误事件，前端出现"失败与成功同时报"）。
    /// 失败时仅记录日志 —— 后续 emit_progress 会机会主义地重新落库状态；
    /// 即使最终态写库失败，下次启动的 recover_stuck_tasks + ensure_ocr_pipeline
    /// 会依据 ocr_pages_json 检查点自愈。
    async fn try_update_processing_status(
        &self,
        file_id: &str,
        stage: ProcessingStage,
        progress: Option<&ProcessingProgress>,
        completed_at: Option<i64>,
        generation: Option<u64>,
    ) {
        if let Err(e) = self
            .update_processing_status(file_id, stage, progress, completed_at, generation)
            .await
        {
            warn!(
                "[PdfProcessingService] Non-fatal status write failure for file {} (stage: {:?}): {}",
                file_id, stage, e
            );
        }
    }

    /// 设置处理错误
    pub fn set_error(
        &self,
        file_id: &str,
        error: &str,
        stage: ProcessingStage,
        generation: Option<u64>,
    ) -> VfsResult<()> {
        if self.skip_stale_task_side_effects(file_id, generation, "set_error") {
            return Ok(());
        }

        let conn = self.db.get_conn_safe()?;
        let now_ms = chrono::Utc::now().timestamp_millis();

        conn.execute(
            r#"
            UPDATE files
            SET processing_status = 'error',
                processing_error = ?1,
                processing_completed_at = ?2,
                updated_at = datetime('now')
            WHERE id = ?3
            "#,
            params![error, now_ms, file_id],
        )?;

        error!(
            "[PdfProcessingService] Set error for file {}: {} (stage: {:?})",
            file_id, error, stage
        );

        Ok(())
    }

    /// 获取处理状态
    pub fn get_status(&self, file_id: &str) -> VfsResult<Option<ProcessingStatus>> {
        let conn = self.db.get_conn_safe()?;

        let result: Option<(
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
        )> = conn
            .query_row(
                r#"
                SELECT processing_status, processing_progress, processing_error,
                       processing_started_at, processing_completed_at
                FROM files WHERE id = ?1
                "#,
                params![file_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;

        match result {
            Some((status, progress_json, error, started_at, completed_at)) => {
                let stage = status.as_deref().unwrap_or("pending");
                let progress: ProcessingProgress = progress_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();

                Ok(Some(ProcessingStatus {
                    file_id: file_id.to_string(),
                    stage: stage.to_string(),
                    progress,
                    error,
                    started_at,
                    completed_at,
                }))
            }
            None => Ok(None),
        }
    }

    /// 取消处理
    pub fn cancel(&self, file_id: &str) -> VfsResult<bool> {
        if let Some((_, (token, gen))) = self.running_tasks.remove(file_id) {
            token.cancel();
            info!(
                "[PdfProcessingService] Cancelled pipeline for file: {} (gen={})",
                file_id, gen
            );
            Ok(true)
        } else {
            debug!(
                "[PdfProcessingService] No running pipeline to cancel for file: {}",
                file_id
            );
            Ok(false)
        }
    }

    /// 判断 processing_status 是否为"活动中间态"（非终态）
    ///
    /// 与 recover_stuck_tasks 的重置范围一致，另含 pending/processing
    /// （pending 是 recover 重置后的状态，同样不代表有任务在跑）。
    fn is_active_stage_str(stage: &str) -> bool {
        matches!(
            stage,
            "pending"
                | "processing"
                | "text_extraction"
                | "page_rendering"
                | "page_compression"
                | "image_compression"
                | "ocr_processing"
                | "vector_indexing"
        )
    }

    /// 重试失败的处理
    ///
    /// ## P1 修复：根据媒体类型选择正确的重试起始阶段
    /// - PDF：从 OcrProcessing 开始（文本提取和页面渲染在上传时已完成）
    /// - 图片：从 ImageCompression 开始（完整重新处理）
    ///
    /// ## P0-3 修复：识别"假运行中"旧状态
    /// 进程被杀后 DB 可能停留在 pending/中间态，但内存中并无任务。
    /// 此前 retry 对这类状态直接拒绝（仅接受 error/completed_with_issues），
    /// 导致文件永远无法重试。现在以 running_tasks 内存状态为准：
    /// DB 中间态 + 内存无任务 → 视为可重试的旧状态残留。
    pub async fn retry(self: &Arc<Self>, file_id: &str) -> VfsResult<()> {
        // 获取当前状态
        let status = self.get_status(file_id)?;

        match status {
            Some(s) if s.stage == "error" || s.stage == "completed_with_issues" => {
                // 检测媒体类型，选择正确的重试起始阶段
                let media_type = self.detect_media_type(file_id)?;
                let start_stage = match media_type {
                    MediaType::Pdf => ProcessingStage::OcrProcessing,
                    MediaType::Image => ProcessingStage::ImageCompression,
                };

                info!(
                    "[MediaProcessingService] Retrying file {} from stage {:?} (media_type={:?})",
                    file_id, start_stage, media_type
                );

                // 重置状态并重新开始
                self.update_processing_status(file_id, ProcessingStage::Pending, None, None, None)
                    .await?;
                self.start_pipeline(file_id, Some(start_stage)).await
            }
            // ★ P0-3：DB 显示中间态但内存无任务 → 旧状态残留（进程被杀/任务泄漏），允许重试
            Some(s) if Self::is_active_stage_str(&s.stage) && !self.is_running(file_id) => {
                let media_type = self.detect_media_type(file_id)?;
                let start_stage = match media_type {
                    MediaType::Pdf => ProcessingStage::OcrProcessing,
                    MediaType::Image => ProcessingStage::ImageCompression,
                };

                info!(
                    "[MediaProcessingService] Retrying file {} from stale stage '{}' (no running task; media_type={:?})",
                    file_id, s.stage, media_type
                );

                self.update_processing_status(file_id, ProcessingStage::Pending, None, None, None)
                    .await?;
                self.start_pipeline(file_id, Some(start_stage)).await
            }
            Some(s) => {
                warn!(
                    "[PdfProcessingService] Cannot retry file {} in stage: {}",
                    file_id, s.stage
                );
                Ok(())
            }
            None => Err(VfsError::NotFound {
                resource_type: "File".to_string(),
                id: file_id.to_string(),
            }),
        }
    }

    /// 检查是否有运行中的任务
    pub fn is_running(&self, file_id: &str) -> bool {
        self.running_tasks.contains_key(file_id)
    }

    /// 获取所有运行中的任务数量
    pub fn running_count(&self) -> usize {
        self.running_tasks.len()
    }

    /// ★ P0-1 修复：启动时恢复 stuck 任务
    ///
    /// 应用重启后，running_tasks 内存映射为空，但数据库中可能存在
    /// 处于中间状态（ocr_processing / vector_indexing / page_compression 等）的文件。
    /// 这些文件不会自动恢复，用户也无法通过 retry 修复（retry 仅处理 error 状态）。
    ///
    /// 此方法将所有 stuck 文件重置为 pending 状态，允许后续重新处理。
    /// 返回 (总恢复数, OCR 相关文件 ID 列表)，供调用方决定是否自动恢复 OCR 流水线。
    pub fn recover_stuck_tasks(&self) -> VfsResult<(usize, Vec<String>)> {
        let conn = self.db.get_conn_safe()?;

        // 查找所有处于中间处理状态的文件 + 它们所处的阶段
        // 排除 pending/completed/error —— 这些是稳定终态
        let stuck_entries: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id, processing_status FROM files
                   WHERE processing_status IN (
                       'text_extraction', 'page_rendering', 'page_compression',
                       'image_compression', 'ocr_processing', 'vector_indexing'
                   )"#,
                )
                .map_err(|e| VfsError::Database(format!("Failed to prepare stuck query: {}", e)))?;

            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let status: String = row.get(1)?;
                    Ok((id, status))
                })
                .map_err(|e| VfsError::Database(format!("Failed to query stuck tasks: {}", e)))?;

            rows.filter_map(log_and_skip_err).collect()
        };

        // ★ Checkpoint recovery: 查找有 OCR 检查点但未完成的文件
        // 这些文件的 processing_status 可能是 pending（之前被恢复过），
        // 但 ocr_pages_json 中有部分结果（completed_at 为空表示进行中）
        let incomplete_ocr_ids: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id FROM files
                   WHERE ocr_pages_json IS NOT NULL
                     AND ocr_pages_json != '{}'
                     AND ocr_pages_json != ''
                     AND processing_status = 'pending'
                     AND preview_json IS NOT NULL
                     AND preview_json != '{}'"#,
                )
                .map_err(|e| VfsError::Database(format!("Failed to prepare incomplete OCR query: {}", e)))?;

            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    Ok(id)
                })
                .map_err(|e| VfsError::Database(format!("Failed to query incomplete OCR: {}", e)))?;

            let ids: Vec<String> = rows.filter_map(log_and_skip_err).collect();

            // 进一步过滤：只保留 completed_at 为空的（真正未完成的检查点）
            let mut incomplete = Vec::new();
            for id in &ids {
                let completed: Option<String> = conn
                    .query_row(
                        "SELECT json_extract(ocr_pages_json, '$.completedAt') FROM files WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .ok()
                    .flatten();

                // completed_at 为空或不存在 → 检查点未完成，需要恢复
                if completed.is_none() || completed.as_deref() == Some("") {
                    incomplete.push(id.clone());
                }
            }
            incomplete
        };

        if !incomplete_ocr_ids.is_empty() {
            info!(
                "[MediaProcessingService] Found {} files with incomplete OCR checkpoints to resume",
                incomplete_ocr_ids.len()
            );
        }

        if stuck_entries.is_empty() && incomplete_ocr_ids.is_empty() {
            debug!("[MediaProcessingService] No stuck tasks or incomplete checkpoints found at startup");
            return Ok((0, Vec::new()));
        }

        let count = stuck_entries.len();
        // 筛选 OCR 相关阶段（page_rendering 和 ocr_processing）—— 这些是恢复后值得自动重启的
        let mut ocr_relevant_ids: Vec<String> = stuck_entries
            .iter()
            .filter(|(_, status)| status == "page_rendering" || status == "ocr_processing")
            .map(|(id, _)| id.clone())
            .collect();

        // 合并不完整检查点的文件 ID（去重）
        let incomplete_count = incomplete_ocr_ids.len();
        for id in &incomplete_ocr_ids {
            if !ocr_relevant_ids.contains(id) {
                ocr_relevant_ids.push(id.clone());
            }
        }

        info!(
            "[MediaProcessingService] Found {} stuck tasks at startup ({} OCR-relevant including checkpoint resumes), resetting to pending",
            count,
            ocr_relevant_ids.len(),
        );

        for (file_id, old_status) in &stuck_entries {
            let affected = conn
                .execute(
                    r#"UPDATE files
                   SET processing_status = 'pending',
                       processing_error = 'recovered: interrupted by app restart',
                       updated_at = datetime('now')
                   WHERE id = ?1"#,
                    params![file_id],
                )
                .unwrap_or(0);

            if affected > 0 {
                info!(
                    "[MediaProcessingService] Reset stuck file {} (was {}) to pending",
                    file_id, old_status
                );
            }
        }

        Ok((count + incomplete_count, ocr_relevant_ids))
    }

    /// 自动恢复因应用重启而中断的 OCR 流水线。
    ///
    /// 对每个传入的文件 ID 调用 start_pipeline(start_stage = OcrProcessing)，
    /// 但受 MAX_CONCURRENT_OCR_RESUME 限制并发数（当前运行任务数 <= 3）。
    /// 返回实际已启动恢复的任务数。
    pub async fn auto_resume_ocr_tasks(
        self: &Arc<Self>,
        file_ids: Vec<String>,
    ) -> VfsResult<usize> {
        const MAX_CONCURRENT: usize = 3;

        let running = self.running_count();
        let available = MAX_CONCURRENT.saturating_sub(running);
        let to_resume = file_ids.len().min(available);

        if to_resume == 0 {
            info!(
                "[MediaProcessingService] OCR auto-resume skipped: {} running tasks (max {}), {} pending",
                running, MAX_CONCURRENT, file_ids.len()
            );
            return Ok(0);
        }

        if to_resume < file_ids.len() {
            info!(
                "[MediaProcessingService] OCR auto-resume limited: resuming {}/{} tasks ({} running, max {})",
                to_resume, file_ids.len(), running, MAX_CONCURRENT
            );
        }

        let mut resumed = 0;
        for file_id in file_ids.into_iter().take(to_resume) {
            match self
                .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
                .await
            {
                Ok(()) => {
                    info!(
                        "[MediaProcessingService] Auto-resumed OCR pipeline for file: {}",
                        file_id
                    );
                    resumed += 1;
                }
                Err(e) => {
                    warn!(
                        "[MediaProcessingService] Failed to auto-resume OCR for file {}: {}",
                        file_id, e
                    );
                }
            }
        }

        Ok(resumed)
    }

    // ========================================================================
    // 事件发送
    // ========================================================================

    /// 发送进度事件（支持媒体类型）
    async fn emit_progress(
        &self,
        file_id: &str,
        progress: ProcessingProgress,
        media_type: MediaType,
        generation: Option<u64>,
    ) {
        if self.skip_stale_task_side_effects(file_id, generation, "emit_progress") {
            return;
        }

        // 兼容轮询链路：将最新进度持久化到 DB，避免前端只能在 completed 才看到 ready_modes 变化。
        // ★ A4#3 节流: 仅在 阶段变化 / ready_modes 变化 / percent 跨 10% 桶 / 距上次 ≥2s 时写
        // (轮询消费者语义保留: ready_modes 与阶段切换总能及时落盘)
        let percent_bucket = (progress.percent as u32) / 10;
        let ready_modes_sig = progress
            .ready_modes
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let should_persist = match self.progress_persist_state.get(file_id) {
            Some(state) => {
                let (prev_stage, prev_bucket, prev_modes, at) = state.value();
                prev_stage != &progress.stage
                    || *prev_bucket != percent_bucket
                    || prev_modes != &ready_modes_sig
                    || at.elapsed().as_secs() >= 2
            }
            None => true,
        };
        if should_persist {
            if let Ok(conn) = self.db.get_conn_safe() {
            let progress_json = serde_json::to_string(&progress).unwrap_or_default();
            if let Err(e) = conn.execute(
                r#"
                UPDATE files
                SET processing_status = ?1,
                    processing_progress = ?2,
                    updated_at = datetime('now')
                WHERE id = ?3
                "#,
                params![progress.stage.clone(), progress_json, file_id],
            ) {
                warn!(
                    "[MediaProcessingService] Failed to persist progress for {}: {}",
                    file_id, e
                );
            }
            }
            self.progress_persist_state.insert(
                file_id.to_string(),
                (
                    progress.stage.clone(),
                    percent_bucket,
                    ready_modes_sig,
                    std::time::Instant::now(),
                ),
            );
        }

        if let Some(app_handle) = self.get_app_handle().await {
            let mut prog = progress.clone();
            prog.media_type = Some(media_type.as_str().to_string());

            let event = MediaProcessingProgressEvent {
                file_id: file_id.to_string(),
                status: prog.clone(),
                media_type: media_type.as_str().to_string(),
            };

            // 发送新统一事件
            if let Err(e) = app_handle.emit("media-processing-progress", &event) {
                warn!(
                    "[MediaProcessingService] Failed to emit media-processing-progress event: {}",
                    e
                );
            }
        }
    }

    /// 发送完成事件（支持媒体类型）
    async fn emit_completed(
        &self,
        file_id: &str,
        ready_modes: Vec<String>,
        stage: ProcessingStage,
        media_type: MediaType,
        generation: Option<u64>,
        note_id: Option<String>,
    ) {
        if self.skip_stale_task_side_effects(file_id, generation, "emit_completed") {
            return;
        }

        if let Some(app_handle) = self.get_app_handle().await {
            let event = MediaProcessingCompletedEvent {
                file_id: file_id.to_string(),
                ready_modes: ready_modes.clone(),
                stage: stage.as_str().to_string(),
                media_type: media_type.as_str().to_string(),
                note_id: note_id.clone(),
            };

            // 发送新统一事件
            if let Err(e) = app_handle.emit("media-processing-completed", &event) {
                warn!(
                    "[MediaProcessingService] Failed to emit media-processing-completed event: {}",
                    e
                );
            }
        }
    }

    /// 发送错误事件（支持媒体类型）
    pub async fn emit_error(
        &self,
        file_id: &str,
        error: &str,
        stage: &str,
        media_type: MediaType,
        generation: Option<u64>,
    ) {
        if self.skip_stale_task_side_effects(file_id, generation, "emit_error") {
            return;
        }

        if let Some(app_handle) = self.get_app_handle().await {
            let event = MediaProcessingErrorEvent {
                file_id: file_id.to_string(),
                error: error.to_string(),
                stage: stage.to_string(),
                media_type: media_type.as_str().to_string(),
            };

            // 发送新统一事件
            if let Err(e) = app_handle.emit("media-processing-error", &event) {
                warn!(
                    "[MediaProcessingService] Failed to emit media-processing-error event: {}",
                    e
                );
            }
        }
    }

    // ========================================================================
    // Stage 4: 向量索引
    // ========================================================================

    /// 执行向量索引阶段（PDF 和图片共用）
    ///
    /// 在 OCR 完成后自动触发向量索引，使文件内容可被 RAG 检索。
    ///
    /// ## 流程
    /// 1. 获取文件信息
    /// 2. 同步资源到 vfs_index_units 表
    /// 3. 调用 VfsFullIndexingService 生成嵌入并存储
    ///
    /// ## 参数
    /// - `file_id`: 文件 ID
    /// - `ready_modes`: 当前已就绪的模式
    /// - `media_type`: 媒体类型
    ///
    /// ## 错误处理
    /// 索引失败不影响文件的可用性，错误会被记录但不会中断流水线
    async fn stage_vector_indexing(
        &self,
        file_id: &str,
        ready_modes: &mut Vec<String>,
        media_type: MediaType,
        generation: u64,
    ) -> VfsResult<()> {
        if self.skip_stale_task_side_effects(file_id, Some(generation), "stage_vector_indexing") {
            return Ok(());
        }

        info!(
            "[PdfProcessingService] Starting vector indexing for file: {}",
            file_id
        );

        // ★ 2026-02 修复：重复索引防护 - 检查是否已在索引中
        {
            use crate::vfs::repos::index_unit_repo::{self, IndexState};
            let conn = self.db.get_conn_safe()?;

            // 先获取文件的 resource_id
            let resource_id: Option<String> = conn
                .query_row(
                    "SELECT resource_id FROM files WHERE id = ?1",
                    params![file_id],
                    |row| row.get(0),
                )
                .optional()?;

            if let Some(ref res_id) = resource_id {
                let existing_units = index_unit_repo::get_by_resource(&conn, res_id)?;

                // 检查是否有任何 unit 正在索引中
                let any_indexing = existing_units.iter().any(|u| {
                    u.text_state == IndexState::Indexing || u.mm_state == IndexState::Indexing
                });

                if any_indexing {
                    warn!(
                        "[PdfProcessingService] Resource {} is already being indexed, skipping",
                        file_id
                    );
                    return Ok(());
                }
            }
        }

        // 1. 获取 AppHandle 以访问 Tauri State
        let app_handle = self
            .get_app_handle()
            .await
            .ok_or_else(|| VfsError::InvalidOperation {
                operation: "vector_indexing".to_string(),
                reason: "AppHandle not set, cannot perform vector indexing".to_string(),
            })?;

        // 2. 获取 LLMManager
        let _llm_manager: Arc<LLMManager> = app_handle
            .try_state::<Arc<LLMManager>>()
            .ok_or_else(|| VfsError::InvalidOperation {
                operation: "vector_indexing".to_string(),
                reason: "LLMManager not found in Tauri state".to_string(),
            })?
            .inner()
            .clone();

        // 3. 获取文件信息
        let file = VfsFileRepo::get_file(&self.db, file_id)?.ok_or_else(|| VfsError::NotFound {
            resource_type: "File".to_string(),
            id: file_id.to_string(),
        })?;

        // 4. 检查是否有 resource_id（文件必须有关联的资源才能索引）
        let resource_id = file
            .resource_id
            .as_ref()
            .ok_or_else(|| VfsError::InvalidOperation {
                operation: "vector_indexing".to_string(),
                reason: format!("File {} has no associated resource", file_id),
            })?;

        // 5. 发送进度事件：开始向量索引
        // ★ P1-1 修复：向量索引范围 75%-95%，与 OCR 结束的 75% 衔接
        self.emit_progress(
            file_id,
            ProcessingProgress {
                stage: "vector_indexing".to_string(),
                current_page: None,
                total_pages: file.page_count.map(|p| p as usize),
                percent: 75.0,
                ready_modes: ready_modes.clone(),
                media_type: Some(media_type.as_str().to_string()),
                failed_stages: None,
            },
            media_type,
            Some(generation),
        )
        .await;

        // 6. 同步 Units（确保 vfs_index_units 表有最新数据）
        let index_service = VfsIndexService::new(self.db.clone());

        // 根据文件类型判断 resource_type
        let is_pdf = file.file_name.to_lowercase().ends_with(".pdf")
            || file
                .mime_type
                .as_ref()
                .map(|m| m.contains("pdf"))
                .unwrap_or(false);
        let resource_type = if is_pdf {
            "textbook".to_string() // PDF 使用 textbook 以支持多页处理
        } else {
            "file".to_string()
        };

        // ★ P1-2 修复：从数据库读取 ocr_text 用于 Unit 元数据补全
        // 图片 OCR 文本存储在 resources.ocr_text（由 stage_image_ocr 写入），
        // 传入 UnitBuildInput 使 FileBuilder 能正确设置 unit.text_content
        let ocr_text_for_unit: Option<String> = {
            let conn = self.db.get_conn_safe()?;
            conn.query_row(
                r#"
                SELECT r.ocr_text
                FROM files f
                LEFT JOIN resources r ON r.id = f.resource_id
                WHERE f.id = ?1
                "#,
                params![file_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap_or(None)
            .filter(|t| !t.trim().is_empty())
        };

        let unit_input = UnitBuildInput {
            resource_id: resource_id.clone(),
            resource_type,
            data: None,
            ocr_text: ocr_text_for_unit,
            ocr_pages_json: file.ocr_pages_json.clone(),
            blob_hash: file.blob_hash.clone(),
            page_count: file.page_count,
            extracted_text: file.extracted_text.clone(),
            preview_json: file.preview_json.clone(),
        };

        match index_service.sync_resource_units(unit_input) {
            Ok(units) => {
                info!(
                    "[PdfProcessingService] Synced {} units for file {}",
                    units.len(),
                    file_id
                );
            }
            Err(e) => {
                warn!(
                    "[PdfProcessingService] Failed to sync units for file {}: {}",
                    file_id, e
                );
                // 继续处理，不中断流水线
            }
        }

        // 7. 发送进度事件：同步完成，开始嵌入生成
        self.emit_progress(
            file_id,
            ProcessingProgress {
                stage: "vector_indexing".to_string(),
                current_page: None,
                total_pages: file.page_count.map(|p| p as usize),
                percent: 85.0,
                ready_modes: ready_modes.clone(),
                media_type: Some(media_type.as_str().to_string()),
                failed_stages: None,
            },
            media_type,
            Some(generation),
        )
        .await;

        // 8. 通过协调器的回调执行向量索引（避免直接依赖 VfsFullIndexingService）
        if let Some(ref callback) = self.vector_index_callback {
            match callback(resource_id.clone()).await {
                Ok(()) => {
                    info!(
                        "[PdfProcessingService] Vector indexing completed for file {}",
                        file_id
                    );
                    log::debug!(
                        "[PdfProcessingService] Vector indexing completed for file {}",
                        file_id
                    );
                }
                Err(e) => {
                    warn!(
                        "[PdfProcessingService] Vector indexing failed for file {}: {}",
                        file_id, e
                    );
                    // 索引失败不中断流水线，文件仍可使用
                }
            }
        } else {
            warn!(
                "[PdfProcessingService] No vector index callback set, skipping vector indexing for file {}",
                file_id
            );
        }

        // 10. 发送进度事件：索引完成
        self.emit_progress(
            file_id,
            ProcessingProgress {
                stage: "vector_indexing".to_string(),
                current_page: None,
                total_pages: file.page_count.map(|p| p as usize),
                percent: 95.0,
                ready_modes: ready_modes.clone(),
                media_type: Some(media_type.as_str().to_string()),
                failed_stages: None,
            },
            media_type,
            Some(generation),
        )
        .await;

        Ok(())
    }

    // ========================================================================
    // Stage 3: OCR 处理（复用预渲染图片）
    // ========================================================================

    /// 执行 Stage 3: OCR 处理
    ///
    /// ## 核心逻辑
    /// 1. 解析 preview_json 获取每页的 blob_hash
    /// 2. 从 VFS Blob 存储获取预渲染图片路径（复用 Stage 2 渲染结果）
    /// 3. 并发调用 OCR API（最多 4 并发）
    /// 4. 将 OCR 结果存入 ocr_pages_json 字段
    ///
    /// ## 图片复用
    /// Stage 2 已将 PDF 页面渲染为 PNG 图片存储在 vfs_blobs 中。
    /// Stage 3 直接复用这些图片，无需重新渲染。
    /// - 预渲染图片格式：PNG
    /// - 预渲染图片 DPI：150
    /// - 存储位置：`vfs_blobs/{hash[0:2]}/{hash}.png`
    async fn stage_ocr_processing(
        &self,
        file_id: &str,
        preview_json: &str,
        total_pages: usize,
        ready_modes: &mut Vec<String>,
        cancel_token: &CancellationToken,
        generation: u64,
    ) -> VfsResult<(String, Option<String>)> {
        if self.skip_stale_task_side_effects(
            file_id,
            Some(generation),
            "stage_ocr_processing:start",
        ) {
            return Ok(("{}".to_string(), None));
        }

        info!(
            "[PdfProcessingService] Starting OCR processing for file: {}",
            file_id
        );

        // 1. 解析 preview_json 获取所有页面的 blob_hash
        let preview: PdfPreviewJson = serde_json::from_str(preview_json).map_err(|e| {
            error!("[PdfProcessingService] Failed to parse preview_json: {}", e);
            VfsError::Serialization(format!("Failed to parse preview_json: {}", e))
        })?;

        let preview_page_count = preview.pages.len();
        if preview_page_count == 0 {
            warn!(
                "[PdfProcessingService] No pages in preview_json for file: {}",
                file_id
            );
            return Ok(("{}".to_string(), None));
        }

        // 2. 获取 blob 目录
        let blobs_dir = self.db.blobs_dir().to_path_buf();

        // 3. 获取 OCR 模型配置
        let config = Arc::new(
            self.llm_manager
                .get_pdf_ocr_model_config()
                .await
                .map_err(|e| {
                    error!(
                        "[PdfProcessingService] Failed to get OCR model config: {}",
                        e
                    );
                    VfsError::Other(format!("Failed to get OCR model config: {}", e))
                })?,
        );

        info!(
            "[PdfProcessingService] Using OCR model: {} for {} pages",
            config.model, preview_page_count
        );

        // ★ Checkpoint/Resume: 加载已完成的 OCR 结果，跳过已处理的页面
        let existing_results: Vec<OcrPageResult> = self
            .load_existing_ocr_results(file_id)
            .unwrap_or_default();
        let already_processed: std::collections::HashSet<usize> = existing_results
            .iter()
            .map(|r| r.page_index)
            .collect();

        let previously_completed = already_processed.len();
        if previously_completed > 0 {
            info!(
                "[PdfProcessingService] Resuming OCR for file {}: {} pages already completed, {} remaining",
                file_id, previously_completed, preview_page_count.saturating_sub(previously_completed)
            );
        }

        // Filter out already-processed pages
        let pending_pages: Vec<_> = preview
            .pages
            .iter()
            .filter(|p| !already_processed.contains(&p.page_index))
            .collect();

        let pending_count = pending_pages.len();
        if pending_count == 0 {
            info!(
                "[PdfProcessingService] All {} pages already have OCR results for file: {}",
                preview_page_count, file_id
            );
            // Build final result from existing data
            let ocr_json = OcrPagesJson {
                total_pages: preview_page_count,
                pages: existing_results,
                completed_at: chrono::Utc::now().to_rfc3339(),
            };
            let ocr_json_str = serde_json::to_string(&ocr_json).map_err(|e| {
                VfsError::Serialization(format!("Failed to serialize OCR result: {}", e))
            })?;
            self.update_file_ocr(file_id, &ocr_json_str, true).await?;
            if !ready_modes.contains(&"ocr".to_string()) {
                ready_modes.push("ocr".to_string());
            }
            // Try to create OCR note for already-processed content
            let note_id = self.create_ocr_note(file_id, &ocr_json).await;
            return Ok((ocr_json_str, note_id));
        }

        // 4. 并发处理 OCR（仅处理未完成的页面）
        let completed_counter = Arc::new(AtomicUsize::new(previously_completed));
        let failed_pages = Arc::new(Mutex::new(Vec::<(usize, String)>::new()));
        // Pre-populate with existing results
        let all_results = Arc::new(Mutex::new(existing_results));
        // ★ Checkpoint mutex: 序列化增量保存，避免并发写入覆盖
        let checkpoint_mutex = Arc::new(Mutex::new(()));
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_OCR_CONCURRENCY));

        // 发送初始进度
        // ★ P1-1 修复：OCR 范围 20%-75%
        let initial_percent = if previously_completed > 0 {
            20.0 + (previously_completed as f64 / preview_page_count as f64) * 55.0
        } else {
            20.0
        };
        self.emit_progress(
            file_id,
            ProcessingProgress {
                stage: "ocr_processing".to_string(),
                current_page: Some(previously_completed),
                total_pages: Some(total_pages),
                percent: initial_percent as f32,
                ready_modes: ready_modes.clone(),
                media_type: Some("pdf".to_string()),
                failed_stages: None,
            },
            MediaType::Pdf,
            Some(generation),
        )
        .await;

        // 5. 使用 futures::stream 并发处理每一页（仅未完成的）
        let db = self.db.clone();
        let llm_manager = self.llm_manager.clone();
        let file_id_owned = file_id.to_string();
        let ready_modes_clone = ready_modes.clone();
        let service = self;

        let tasks: Vec<_> = pending_pages
            .iter()
            .map(|page| {
                let page_index = page.page_index;
                let blob_hash = page.blob_hash.clone();
                let blobs_dir = blobs_dir.clone();
                let config = config.clone();
                let llm_manager = llm_manager.clone();
                let semaphore = semaphore.clone();
                let completed_counter = completed_counter.clone();
                let failed_pages = failed_pages.clone();
                let all_results = all_results.clone();
                let cancel_token = cancel_token.clone();
                let file_id = file_id_owned.clone();
                let db = db.clone();
                let ready_modes_for_task = ready_modes_clone.clone();
                let service = service;
                let generation_for_task = generation;
                let checkpoint_mutex = checkpoint_mutex.clone();
                let preview_page_count = preview_page_count;

                async move {
                    // ★ P0 修复：正确处理信号量获取错误，避免 panic
                    let _permit = match semaphore.acquire().await {
                        Ok(permit) => permit,
                        Err(e) => {
                            error!("[PdfProcessingService] Failed to acquire semaphore: {}", e);
                            failed_pages.lock().await.push((page_index, format!("Semaphore error: {}", e)));
                            return;
                        }
                    };

                    // 检查取消
                    if cancel_token.is_cancelled() {
                        return;
                    }

                    // 获取 blob 文件路径
                    let conn = match db.get_conn_safe() {
                        Ok(c) => c,
                        Err(e) => {
                            failed_pages.lock().await.push((page_index, e.to_string()));
                            return;
                        }
                    };

                    let blob_path = match VfsBlobRepo::get_blob_path_with_conn(
                        &conn,
                        &blobs_dir,
                        &blob_hash,
                    ) {
                        Ok(Some(path)) => path,
                        Ok(None) => {
                            let err = format!("Blob not found: {}", blob_hash);
                            error!("[PdfProcessingService] {}", err);
                            failed_pages.lock().await.push((page_index, err));
                            return;
                        }
                        Err(e) => {
                            failed_pages.lock().await.push((page_index, e.to_string()));
                            return;
                        }
                    };

                    // 执行 OCR（带重试）
                    let ocr_result = Self::call_ocr_with_retry(
                        &llm_manager,
                        &config,
                        &blob_path,
                        page_index,
                        &cancel_token,
                    )
                    .await;

                    match ocr_result {
                        Ok(blocks) => {
                            let completed = completed_counter.fetch_add(1, Ordering::SeqCst) + 1;
                            all_results.lock().await.push(OcrPageResult {
                                page_index,
                                blocks,
                            });

                            // ★ Checkpoint: 每完成一页，立即持久化到数据库（断点恢复）
                            {
                                let _checkpoint_guard = checkpoint_mutex.lock().await;
                                let current_results = all_results.lock().await.clone();
                                let checkpoint_json = OcrPagesJson {
                                    total_pages: preview_page_count,
                                    pages: current_results,
                                    // 未完成时使用空字符串标记为进行中
                                    completed_at: String::new(),
                                };
                                if let Ok(json_str) = serde_json::to_string(&checkpoint_json) {
                                    // 使用非阻塞方式保存检查点（忽略错误，不中断处理）
                                    if let Ok(conn) = db.get_conn_safe() {
                                        let _ = conn.execute(
                                            "UPDATE files SET ocr_pages_json = ?1, updated_at = datetime('now') WHERE id = ?2",
                                            rusqlite::params![json_str, file_id],
                                        );
                                        debug!(
                                            "[PdfProcessingService] Checkpoint saved: page {}/{} of file {}",
                                            completed, preview_page_count, file_id
                                        );
                                    }
                                }
                            }

                            // 发送进度更新
                            // ★ P1-1 修复：OCR 进度范围 20%-75%，保证单调递增
                            let ocr_progress = 20.0 + (completed as f64 / preview_page_count as f64) * 55.0;
                            let progress = ProcessingProgress {
                                stage: "ocr_processing".to_string(),
                                current_page: Some(completed),
                                total_pages: Some(total_pages),
                                percent: ocr_progress as f32,
                                ready_modes: ready_modes_for_task.clone(),
                                media_type: Some("pdf".to_string()),
                                failed_stages: None,
                            };
                            service
                                .emit_progress(
                                    &file_id,
                                    progress,
                                    MediaType::Pdf,
                                    Some(generation_for_task),
                                )
                                .await;

                            debug!(
                                "[PdfProcessingService] OCR completed for page {}/{} of file {} ({}%)",
                                completed, preview_page_count, file_id, ocr_progress as i32
                            );
                        }
                        Err(e) => {
                            error!(
                                "[PdfProcessingService] OCR failed for page {} of file {}: {}",
                                page_index, file_id, e
                            );
                            failed_pages.lock().await.push((page_index, e));
                        }
                    }
                }
            })
            .collect();

        // 并发执行所有 OCR 任务
        stream::iter(tasks)
            .for_each_concurrent(MAX_OCR_CONCURRENCY, |task| task)
            .await;

        // 6. 检查结果
        let failed = failed_pages.lock().await;
        let mut results = all_results.lock().await;
        let success_count = results.len();
        let failed_count = failed.len();

        info!(
            "[PdfProcessingService] OCR processing completed for file {}: success={}, failed={}",
            file_id, success_count, failed_count
        );

        // 按页码排序
        results.sort_by_key(|r| r.page_index);

        // 7. 构建 ocr_pages_json（标记为已完成）
        let ocr_json = OcrPagesJson {
            total_pages: preview_page_count,
            pages: results.clone(),
            completed_at: chrono::Utc::now().to_rfc3339(),
        };

        let ocr_json_str = serde_json::to_string(&ocr_json).map_err(|e| {
            VfsError::Serialization(format!("Failed to serialize OCR result: {}", e))
        })?;

        // 8. 更新数据库
        // ★ P1-3 修复：部分成功也标记 OCR 可用（成功率 >= 50%）
        // 原策略要求 100% 成功才标记 ocr ready，导致 99/100 页成功也不可用
        let success_rate = if preview_page_count > 0 {
            success_count as f64 / preview_page_count as f64
        } else {
            0.0
        };
        let ocr_usable = success_rate >= 0.5; // 50% 以上成功即可用
        self.update_file_ocr(file_id, &ocr_json_str, ocr_usable)
            .await?;

        // 同步内存中的 ready_modes
        if ocr_usable {
            if !ready_modes.contains(&"ocr".to_string()) {
                ready_modes.push("ocr".to_string());
            }
            if failed_count > 0 {
                info!(
                    "[PdfProcessingService] OCR partially succeeded for file {}: {}/{} pages OK ({:.0}%), marking ocr as ready",
                    file_id, success_count, preview_page_count, success_rate * 100.0
                );
            }
        } else {
            warn!(
                "[PdfProcessingService] OCR mostly failed for file {}: {}/{} pages failed ({:.0}% success), not marking ocr as ready",
                file_id, failed_count, preview_page_count, success_rate * 100.0
            );
        }

        // ★ FIX 1: Auto-create note with OCR text
        // Always try to create note — even if ocr_usable=false, the partial text is still valuable
        let note_id = self.create_ocr_note(file_id, &ocr_json).await;
        if note_id.is_none() && ocr_usable {
            warn!(
                "[PdfProcessingService] OCR was usable but note creation failed for file {}",
                file_id
            );
        }

        Ok((ocr_json_str, note_id))
    }

    /// 从数据库加载已有的 OCR 结果（用于断点恢复）
    fn load_existing_ocr_results(&self, file_id: &str) -> Option<Vec<OcrPageResult>> {
        let conn = self.db.get_conn_safe().ok()?;
        let ocr_json_str: Option<String> = conn
            .query_row(
                "SELECT ocr_pages_json FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |row| row.get(0),
            )
            .ok()?;

        let json_str = ocr_json_str?;
        if json_str.is_empty() || json_str == "{}" {
            return Some(Vec::new());
        }

        match serde_json::from_str::<OcrPagesJson>(&json_str) {
            Ok(ocr_json) => {
                info!(
                    "[PdfProcessingService] Loaded {} existing OCR results for file: {}",
                    ocr_json.pages.len(), file_id
                );
                Some(ocr_json.pages)
            }
            Err(e) => {
                warn!(
                    "[PdfProcessingService] Failed to parse existing OCR results for file {}: {}",
                    file_id, e
                );
                Some(Vec::new())
            }
        }
    }

    /// 调用 OCR API（带重试机制）
    ///
    /// 使用指数退避重试策略处理速率限制错误
    async fn call_ocr_with_retry(
        llm_manager: &Arc<LLMManager>,
        _config: &crate::llm_manager::ApiConfig,
        image_path: &PathBuf,
        page_index: usize,
        cancel_token: &CancellationToken,
    ) -> Result<Vec<PdfOcrTextBlock>, String> {
        let path_str = image_path.to_string_lossy().to_string();
        let mut attempt = 0;
        let mut backoff = INITIAL_BACKOFF_MS;

        loop {
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
            }

            match llm_manager
                .call_ocr_page_with_fallback(
                    &path_str,
                    page_index,
                    crate::ocr_adapters::OcrTaskType::FreeText,
                )
                .await
            {
                Ok(cards) => {
                    // 转换 ExamSegmentationCard -> PdfOcrTextBlock
                    let blocks: Vec<PdfOcrTextBlock> = cards
                        .iter()
                        .map(|c| PdfOcrTextBlock {
                            text: c.ocr_text.clone().unwrap_or_default(),
                            bbox: c.bbox.clone(),
                        })
                        .collect();
                    return Ok(blocks);
                }
                Err(e) => {
                    // 检查是否是速率限制错误
                    if Self::is_rate_limit_error(&e) && attempt < MAX_OCR_RETRY_ATTEMPTS {
                        attempt += 1;
                        warn!(
                            "[PdfProcessingService] OCR rate limited for page {}, retrying ({}/{}), backoff={}ms",
                            page_index, attempt, MAX_OCR_RETRY_ATTEMPTS, backoff
                        );
                        sleep(Duration::from_millis(backoff)).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF_MS);
                        continue;
                    }

                    return Err(e.to_string());
                }
            }
        }
    }

    /// 检查是否是速率限制错误
    fn is_rate_limit_error(error: &crate::models::AppError) -> bool {
        if let Some(details) = &error.details {
            if details.get("status").and_then(|v| v.as_u64()) == Some(429) {
                return true;
            }
        }

        let message = error.message.to_ascii_lowercase();
        message.contains("429")
            || message.contains("rate limit")
            || message.contains("too many requests")
    }

    /// 更新文件的 OCR 结果到数据库
    ///
    /// ★ P1-3 修复：`ocr_usable` 参数改为成功率 >=50% 即为 true（原为全部成功）
    /// ★ P1-4 修复：增加 SQLITE_BUSY retry 逻辑，与 update_processing_status 一致
    async fn update_file_ocr(
        &self,
        file_id: &str,
        ocr_json: &str,
        ocr_usable: bool,
    ) -> VfsResult<()> {
        // ★ P1-4 修复：带 busy-retry 的事务开始
        // 并发处理多文件时 BEGIN IMMEDIATE 可能因 SQLITE_BUSY 失败
        // 连接在循环内获取，避免 sleep 期间持有空闲连接导致连接池饥饿
        let conn = {
            let max_retries = 3u32;
            let mut attempt = 0u32;
            loop {
                let conn = self.db.get_conn_safe()?;
                match conn.execute("BEGIN IMMEDIATE", []) {
                    Ok(_) => break conn,
                    Err(e) if attempt < max_retries => {
                        let msg = e.to_string();
                        if msg.contains("database is locked") || msg.contains("SQLITE_BUSY") {
                            attempt += 1;
                            let backoff_ms = 50 * (1u64 << attempt); // 100ms, 200ms, 400ms
                            warn!(
                                "[PdfProcessingService] update_file_ocr BEGIN IMMEDIATE busy for file {}, retry {}/{} in {}ms",
                                file_id, attempt, max_retries, backoff_ms
                            );
                            drop(conn);
                            sleep(std::time::Duration::from_millis(backoff_ms)).await;
                            continue;
                        }
                        return Err(VfsError::Database(format!("BEGIN IMMEDIATE failed: {}", e)));
                    }
                    Err(e) => {
                        error!(
                            "[PdfProcessingService] update_file_ocr BEGIN IMMEDIATE failed after {} retries for file {}: {}",
                            max_retries, file_id, e
                        );
                        return Err(VfsError::Database(format!(
                            "BEGIN IMMEDIATE failed after {} retries: {}",
                            max_retries, e
                        )));
                    }
                }
            }
        };

        let result = (|| -> VfsResult<()> {
            // 1. 更新 OCR 数据
            conn.execute(
                r#"
                UPDATE files
                SET ocr_pages_json = ?1,
                    updated_at = datetime('now')
                WHERE id = ?2
                "#,
                params![ocr_json, file_id],
            )?;

            // 2. 更新 processing_progress 中的 ready_modes
            // ★ P1-3 修复：成功率 >=50% 即标记 ocr 可用
            if ocr_usable {
                // 先获取当前进度
                let current_progress: Option<String> = conn
                    .query_row(
                        "SELECT processing_progress FROM files WHERE id = ?1",
                        params![file_id],
                        |row| row.get(0),
                    )
                    .ok();

                if let Some(progress_str) = current_progress {
                    if let Ok(mut progress) =
                        serde_json::from_str::<ProcessingProgress>(&progress_str)
                    {
                        if !progress.ready_modes.contains(&"ocr".to_string()) {
                            progress.ready_modes.push("ocr".to_string());
                            let updated_json = serde_json::to_string(&progress).unwrap_or_default();
                            conn.execute(
                                "UPDATE files SET processing_progress = ?1 WHERE id = ?2",
                                params![updated_json, file_id],
                            )?;
                        }
                    }
                }
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                info!(
                    "[PdfProcessingService] Updated OCR result for file: {} (ocr_usable={})",
                    file_id, ocr_usable
                );
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// ★★ 创建 OCR 逐页笔记文件夹
    ///
    /// 每页创建一个独立笔记，存放在一个文件夹中，避免巨型 MD 文件导致编辑器卡死。
    /// 返回第一页的 note_id（兼容旧调用方），或 None 如果创建失败。
    async fn create_ocr_note(
        &self,
        file_id: &str,
        ocr_json: &OcrPagesJson,
    ) -> Option<String> {
        // 1. Get file name from database
        let file_name = match (|| -> VfsResult<String> {
            let conn = self.db.get_conn_safe()?;
            let name: String = conn
                .query_row(
                    "SELECT file_name FROM files WHERE id = ?1",
                    params![file_id],
                    |row| row.get(0),
                )
                .map_err(|e| VfsError::Database(format!("Failed to get file name: {}", e)))?;
            Ok(name)
        })() {
            Ok(name) => name,
            Err(e) => {
                warn!("[PdfProcessingService] Failed to get file name: {}", e);
                return None;
            }
        };

        if ocr_json.pages.is_empty() {
            warn!("[PdfProcessingService] No OCR pages for file: {}", file_id);
            return None;
        }

        let total_pages = ocr_json.pages.len();
        info!(
            "[PdfProcessingService] Creating per-page OCR notes for file {} ({} pages)",
            file_id, total_pages
        );

        // 2. Create folder for per-page notes
        let folder = VfsFolder::new(
            format!("{} (OCR)", file_name),
            None, // root level
            Some("📖".to_string()),
            Some("#4A90D9".to_string()),
        );
        let folder_id = folder.id.clone();

        let conn = match self.db.get_conn_safe() {
            Ok(c) => c,
            Err(e) => {
                warn!("[PdfProcessingService] Failed to get DB connection: {}", e);
                return None;
            }
        };

        if let Err(e) = VfsFolderRepo::create_folder_with_conn(&conn, &folder) {
            warn!("[PdfProcessingService] Failed to create OCR folder: {}", e);
            // Continue anyway — folder is optional
        }

        // 3. Create per-page notes
        let mut first_note_id: Option<String> = None;
        let mut created_count = 0u32;

        for page in &ocr_json.pages {
            let page_num = page.page_index + 1;
            let mut content = format!("# Page {}\n\n> 📄 来源: {} | 页码: {}/{}\n\n",
                page_num, file_name, page_num, total_pages);

            let mut has_text = false;
            for block in &page.blocks {
                if !block.text.trim().is_empty() {
                    content.push_str(&block.text);
                    content.push('\n');
                    has_text = true;
                }
            }

            if !has_text {
                content.push_str("*(此页未识别到文字)*\n");
            }

            let params = VfsCreateNoteParams {
                title: format!("第 {} 页", page_num),
                content,
                tags: vec![
                    "ocr".to_string(),
                    "auto-generated".to_string(),
                    format!("source:{}", file_id),
                    format!("page:{}", page_num),
                    format!("folder:{}", folder_id),
                ],
            };

            match VfsNoteRepo::create_note_with_conn(&conn, params) {
                Ok(note) => {
                    // Add note to folder
                    let folder_item = VfsFolderItem::new(
                        Some(folder_id.clone()),
                        "note".to_string(),
                        note.id.clone(),
                    );
                    let _ = VfsFolderRepo::add_item_to_folder_with_conn(&conn, &folder_item);

                    if first_note_id.is_none() {
                        first_note_id = Some(note.id.clone());
                    }
                    created_count += 1;
                }
                Err(e) => {
                    warn!(
                        "[PdfProcessingService] Failed to create page {} note: {}",
                        page_num, e
                    );
                }
            }
        }

        // Drop conn before async operations
        drop(conn);

        info!(
            "[PdfProcessingService] Created {}/{} per-page OCR notes for file {} in folder {}",
            created_count, total_pages, file_id, folder_id
        );

        // 4. Emit DSTU watch events for folder and first note
        if let Some(app_handle) = self.get_app_handle().await {
            use crate::dstu::types::{DstuWatchEvent, DstuWatchEventType};
            // Emit folder creation
            let folder_event = DstuWatchEvent {
                event_type: DstuWatchEventType::Created,
                path: format!("/{}", folder_id),
                old_path: None,
                node: None,
            };
            let _ = app_handle.emit("dstu:change", &folder_event);
            // Emit first note creation
            if let Some(ref nid) = first_note_id {
                let note_event = DstuWatchEvent {
                    event_type: DstuWatchEventType::Created,
                    path: format!("/{}", nid),
                    old_path: None,
                    node: None,
                };
                let _ = app_handle.emit("dstu:change", &note_event);
            }
        }

        first_note_id
    }

    /// ★★ 公开方法：确保 OCR 逐页笔记存在（供 vfs_ensure_ocr_pipeline 调用）
    /// 先检查是否已有逐页笔记，有则返回第一页 ID；否则创建逐页文件夹。
    /// 返回 note_id（第一页）如果成功或已存在，否则返回 None。
    pub async fn ensure_ocr_note(&self, file_id: &str) -> Option<String> {
        // 0. 先检查是否已有逐页笔记（搜索带 source: 和 page: 标签的笔记）
        {
            let conn = self.db.get_conn_safe().ok()?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notes n
                     WHERE n.deleted_at IS NULL
                       AND n.tags LIKE ?1
                       AND n.tags LIKE ?2",
                    rusqlite::params![format!("%source:{}%", file_id), "%page:%"],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if count > 0 {
                // 已有逐页笔记，返回第一页的 ID
                let first_id: String = conn
                    .query_row(
                        "SELECT n.id FROM notes n
                         WHERE n.deleted_at IS NULL
                           AND n.tags LIKE ?1
                           AND n.tags LIKE ?2
                         ORDER BY n.created_at ASC LIMIT 1",
                        rusqlite::params![format!("%source:{}%", file_id), "%page:1%"],
                        |row| row.get(0),
                    )
                    .ok()?;
                info!(
                    "[PdfProcessingService] OCR notes already exist for file {} (found {} notes), returning first: {}",
                    file_id, count, first_id
                );
                return Some(first_id);
            }
        }

        // 1. 加载 ocr_pages_json
        let ocr_json_str = {
            let conn = self.db.get_conn_safe().ok()?;
            let json_str: String = conn
                .query_row(
                    "SELECT ocr_pages_json FROM files WHERE id = ?1",
                    rusqlite::params![file_id],
                    |row| row.get(0),
                )
                .ok()?;
            if json_str.is_empty() || json_str == "{}" {
                return None;
            }
            json_str
        };

        // 2. 解析 OCR 数据
        let ocr_json: OcrPagesJson = serde_json::from_str(&ocr_json_str).ok()?;
        if ocr_json.pages.is_empty() {
            return None;
        }

        // 3. 创建逐页笔记
        info!(
            "[PdfProcessingService] ensure_ocr_note: creating per-page notes for file {} ({} pages)",
            file_id,
            ocr_json.pages.len()
        );
        self.create_ocr_note(file_id, &ocr_json).await
    }

    /// ★ FIX 2: Find PDF files that have not yet been OCR-processed.
    ///
    /// Queries the files table for PDFs where `ocr_pages_json IS NULL`,
    /// filtered by optional limit. Returns file IDs that can be passed
    /// to `start_pipeline()` for re-processing.
    pub fn find_unprocessed_pdfs(&self, limit: usize) -> VfsResult<Vec<String>> {
        let conn = self.db.get_conn_safe()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id FROM files
                   WHERE mime_type = 'application/pdf'
                     AND ocr_pages_json IS NULL
                     AND preview_json IS NOT NULL
                   LIMIT ?1"#,
            )
            .map_err(|e| VfsError::Database(format!("Failed to prepare unprocessed PDF query: {}", e)))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let id: String = row.get(0)?;
                Ok(id)
            })
            .map_err(|e| VfsError::Database(format!("Failed to query unprocessed PDFs: {}", e)))?;

        let file_ids: Vec<String> = rows.filter_map(log_and_skip_err).collect();
        debug!(
            "[PdfProcessingService] Found {} unprocessed PDFs (limit={})",
            file_ids.len(),
            limit
        );
        Ok(file_ids)
    }

    /// Backfill preview_json for historical PDFs that lack it.
    ///
    /// 在应用启动时调用，补全历史 PDF 的 preview_json，确保旧教科书拥有 OCR 所需的数据。
    /// 每次启动最多处理 5 个文件，避免阻塞应用启动。
    pub fn backfill_missing_previews(&self) -> VfsResult<usize> {
        const BACKFILL_LIMIT: usize = 5;

        let conn = self.db.get_conn_safe()?;

        // 查询缺少 preview_json 但有 page_count 的 PDF（page_count > 0 说明文件可访问）
        let files: Vec<(String, Option<String>, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, blob_hash, original_path \
                     FROM files \
                     WHERE mime_type = 'application/pdf' \
                       AND preview_json IS NULL \
                       AND page_count > 0 \
                     LIMIT ?1",
                )
                .map_err(|e| {
                    VfsError::Database(format!("[Backfill] Failed to prepare query: {}", e))
                })?;

            let rows = stmt
                .query_map(params![BACKFILL_LIMIT as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| {
                    VfsError::Database(format!("[Backfill] Failed to query: {}", e))
                })?;

            rows.filter_map(log_and_skip_err).collect()
        };

        let count = files.len();
        if count == 0 {
            info!(
                "[PdfProcessingService] Backfill: no historical PDFs missing preview_json"
            );
            return Ok(0);
        }

        info!(
            "[PdfProcessingService] Backfill: found {} historical PDFs missing preview_json, processing in background",
            count
        );

        let db = self.db.clone();

        // 后台处理：逐个文件生成 preview（不阻塞启动）
        tauri::async_runtime::spawn(async move {
            let blobs_dir = db.blobs_dir().to_path_buf();
            let total = files.len();

            for (idx, (file_id, blob_hash, original_path)) in files.iter().enumerate() {
                // Step a/b: 获取 PDF 字节 — 优先 blob_hash，其次 original_path（带安全校验）
                let pdf_bytes = {
                    let conn = match db.get_conn_safe() {
                        Ok(c) => c,
                        Err(e) => {
                            warn!(
                                "[Backfill] Failed to get DB connection: {}",
                                e
                            );
                            continue;
                        }
                    };

                    if let Some(ref hash) = blob_hash {
                        match VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, hash) {
                            Ok(Some(path)) => match std::fs::read(&path) {
                                Ok(bytes) => Some(bytes),
                                Err(e) => {
                                    warn!(
                                        "[Backfill] Failed to read blob {} for {}: {}",
                                        hash, file_id, e
                                    );
                                    None
                                }
                            },
                            Ok(None) => {
                                warn!(
                                    "[Backfill] Blob path not found for hash {} (file {})",
                                    hash, file_id
                                );
                                None
                            }
                            Err(e) => {
                                warn!(
                                    "[Backfill] Failed to resolve blob path for {}: {}",
                                    file_id, e
                                );
                                None
                            }
                        }
                    } else if let Some(ref path_str) = original_path {
                        if !crate::vfs::repos::attachment_repo::VfsAttachmentRepo::is_safe_original_path(
                            &blobs_dir,
                            path_str,
                        ) {
                            warn!(
                                "[Backfill] Unsafe original_path for {}: {}",
                                file_id, path_str
                            );
                            None
                        } else {
                            match std::fs::read(path_str) {
                                Ok(bytes) => Some(bytes),
                                Err(e) => {
                                    warn!(
                                        "[Backfill] Failed to read {} for {}: {}",
                                        path_str, file_id, e
                                    );
                                    None
                                }
                            }
                        }
                    } else {
                        warn!(
                            "[Backfill] No blob_hash or original_path for file: {}",
                            file_id
                        );
                        None
                    }
                };

                if let Some(bytes) = pdf_bytes {
                    // Step c: 调用 render_pdf_preview 生成 preview_json + extracted_text
                    let config = PdfPreviewConfig::default();
                    let db_clone = db.clone();
                    let blobs_dir_clone = blobs_dir.clone();
                    let file_id_clone = file_id.clone();

                    let render_result = tokio::task::spawn_blocking(move || {
                        let conn = match db_clone.get_conn_safe() {
                            Ok(c) => c,
                            Err(e) => {
                                warn!(
                                    "[Backfill] spawn_blocking: failed to get DB conn: {}",
                                    e
                                );
                                return None;
                            }
                        };
                        match render_pdf_preview_with_progress(
                            &conn,
                            &blobs_dir_clone,
                            &bytes,
                            &config,
                            |_, _| {},
                        ) {
                            Ok(result) => Some((result, conn)),
                            Err(e) => {
                                warn!(
                                    "[Backfill] render_pdf_preview failed for {}: {}",
                                    file_id_clone, e
                                );
                                None
                            }
                        }
                    })
                    .await
                    .unwrap_or_else(|e| {
                        warn!("[Backfill] spawn_blocking panicked: {}", e);
                        None
                    });

                    if let Some((result, conn)) = render_result {
                        // Step d: UPDATE files SET preview_json=?, extracted_text=?, page_count=? WHERE id=?
                        if let Some(pj_ref) = &result.preview_json {
                            let pj_str = match serde_json::to_string(pj_ref) {
                                Ok(s) => s,
                                Err(e) => {
                                    warn!(
                                        "[Backfill] Failed to serialize preview_json: {}",
                                        e
                                    );
                                    continue;
                                }
                            };

                            if let Err(e) = conn.execute(
                                "UPDATE files SET preview_json = ?1, extracted_text = ?2, page_count = ?3 WHERE id = ?4",
                                params![
                                    pj_str,
                                    result.extracted_text.as_deref(),
                                    result.page_count as i32,
                                    file_id,
                                ],
                            ) {
                                warn!(
                                    "[Backfill] Failed to update files row for {}: {}",
                                    file_id, e
                                );
                            } else {
                                // Step e: 日志进度
                                info!(
                                    "[Backfill] Backfilled preview for file {} ({}/{})",
                                    file_id,
                                    idx + 1,
                                    total
                                );
                            }
                        }
                    }
                }
            }

            info!(
                "[PdfProcessingService] Backfill completed: processed {}/{} files",
                total, total
            );
        });

        Ok(count)
    }
}

// 实现比较操作以支持 start_stage <= ProcessingStage::XXX
impl PartialOrd for ProcessingStage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProcessingStage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_order = match self {
            ProcessingStage::Pending => 0,
            ProcessingStage::TextExtraction => 1,
            ProcessingStage::PageRendering => 2,
            ProcessingStage::PageCompression => 3,
            ProcessingStage::ImageCompression => 4,
            ProcessingStage::OcrProcessing => 5,
            ProcessingStage::VectorIndexing => 6,
            ProcessingStage::Completed => 7,
            ProcessingStage::CompletedWithIssues => 8,
            ProcessingStage::Error => 9,
        };
        let other_order = match other {
            ProcessingStage::Pending => 0,
            ProcessingStage::TextExtraction => 1,
            ProcessingStage::PageRendering => 2,
            ProcessingStage::PageCompression => 3,
            ProcessingStage::ImageCompression => 4,
            ProcessingStage::OcrProcessing => 5,
            ProcessingStage::VectorIndexing => 6,
            ProcessingStage::Completed => 7,
            ProcessingStage::CompletedWithIssues => 8,
            ProcessingStage::Error => 9,
        };
        self_order.cmp(&other_order)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_stage_as_str() {
        assert_eq!(ProcessingStage::Pending.as_str(), "pending");
        assert_eq!(ProcessingStage::OcrProcessing.as_str(), "ocr_processing");
        assert_eq!(ProcessingStage::Completed.as_str(), "completed");
    }

    #[test]
    fn test_processing_stage_from_str() {
        assert_eq!(
            ProcessingStage::from_str("pending"),
            ProcessingStage::Pending
        );
        assert_eq!(
            ProcessingStage::from_str("ocr_processing"),
            ProcessingStage::OcrProcessing
        );
        assert_eq!(
            ProcessingStage::from_str("unknown"),
            ProcessingStage::Pending
        );
    }

    #[test]
    fn test_processing_stage_order() {
        assert!(ProcessingStage::Pending < ProcessingStage::TextExtraction);
        assert!(ProcessingStage::OcrProcessing < ProcessingStage::VectorIndexing);
        assert!(ProcessingStage::VectorIndexing < ProcessingStage::Completed);
    }

    #[test]
    fn test_processing_progress_default() {
        let progress = ProcessingProgress::default();
        assert_eq!(progress.stage, "pending");
        assert_eq!(progress.percent, 0.0);
        assert!(progress.ready_modes.is_empty());
    }
}
