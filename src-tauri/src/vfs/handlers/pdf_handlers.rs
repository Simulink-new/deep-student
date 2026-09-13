//! VFS PDF/媒体处理 Tauri 命令处理器
//!
//! 提供 PDF 预处理流水线、页面图片获取、媒体缓存等命令。
//!
//! ## 命令
//! - `vfs_get_pdf_page_image`: 获取 PDF 指定页面的预渲染图片
//! - `vfs_get_pdf_processing_status`: 获取 PDF 处理状态
//! - `vfs_cancel_pdf_processing`: 取消 PDF 处理
//! - `vfs_retry_pdf_processing`: 重试 PDF 处理
//! - `vfs_start_pdf_processing`: 启动 PDF 预处理流水线
//! - `vfs_get_batch_pdf_processing_status`: 批量获取 PDF 处理状态
//! - `vfs_list_pending_pdf_processing`: 列出待处理的 PDF 文件
//! - `vfs_download_paper`: 独立下载论文 PDF
//! - `vfs_get_blob_base64`: 根据 blob hash 获取图片的 base64 内容
//! - `vfs_get_blob_pdfstream_url`: 根据文件 ID 获取 blob 的绝对路径（供 pdfstream:// 协议使用）

use std::collections::HashMap;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, State};

use crate::vfs::database::VfsDatabase;
use crate::vfs::error::{VfsError, VfsResult};
use crate::vfs::index_service::VfsIndexService;
use crate::vfs::pdf_processing_service::{
    PdfProcessingService, ProcessingStage, ProcessingStatus,
};
use crate::vfs::repos::pdf_preview::{render_pdf_preview, PdfPreviewConfig};
use crate::vfs::repos::{VfsBlobRepo, VfsFileRepo, VfsResourceRepo};
use crate::vfs::types::*;
use crate::vfs::unit_builder::UnitBuildInput;

use super::resource_handlers::validate_id_format_any;

// ============================================================================
// 共享工具函数（PDF/图片压缩检测）
// ============================================================================

/// 检查 PDF 是否需要页面压缩
pub fn pdf_preview_needs_compression(preview_json: &str) -> bool {
    let preview: PdfPreviewJson = match serde_json::from_str(preview_json) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if preview.pages.is_empty() {
        return false;
    }
    preview.pages.iter().any(|page| {
        page.compressed_blob_hash
            .as_ref()
            .map(|h| h.trim().is_empty())
            .unwrap_or(true)
    })
}

/// 检查图片是否缺少压缩版本
pub fn image_needs_compression_with_conn(
    conn: &rusqlite::Connection,
    blobs_dir: &std::path::Path,
    file_id: &str,
) -> bool {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT compressed_blob_hash, blob_hash FROM files WHERE id = ?1",
            rusqlite::params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();

    let Some((compressed_hash, _blob_hash)) = row else {
        return false;
    };

    let Some(ch) = compressed_hash else {
        return true;
    };
    if ch.trim().is_empty() {
        return true;
    }
    VfsBlobRepo::get_blob_path_with_conn(conn, blobs_dir, &ch)
        .ok()
        .flatten()
        .is_none()
}

// ============================================================================
// 输出类型
// ============================================================================

/// vfs_get_blob_base64 返回结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsBlobBase64Result {
    /// Base64 编码的文件内容（不含 data: 前缀）
    pub base64: String,
    /// MIME 类型（如 "image/jpeg"）
    pub mime_type: String,
    /// 文件大小（字节）
    pub size: i64,
}

/// 论文下载参数
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsDownloadPaperParams {
    /// PDF 下载 URL
    pub url: String,
    /// 论文标题（用作文件名）
    pub title: String,
    /// 目标文件夹 ID（可选，默认根目录）
    pub folder_id: Option<String>,
}

/// 论文下载结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsDownloadPaperResult {
    pub success: bool,
    pub file_id: Option<String>,
    pub file_name: Option<String>,
    pub size_bytes: Option<u64>,
    pub page_count: Option<i32>,
    pub error: Option<String>,
}

// ============================================================================
// Blob 相关命令
// ============================================================================

/// ★ 根据 blob hash 获取图片的 base64 内容
///
/// ## 用途
/// 题目集识别多模态改造后，图片存储在 VFS blobs 表中，
/// 前端需要通过 blob_hash 获取图片的 base64 数据用于：
/// 1. 前端显示（Canvas 裁剪）
/// 2. 上下文注入（多模态请求）
///
/// ## 参数
/// - `blob_hash`: Blob 的 SHA-256 哈希值
///
/// ## 返回
/// - `Ok(VfsBlobBase64Result)`: 包含 base64 数据和 mime_type
/// - `Err(String)`: Blob 不存在或读取失败
#[tauri::command]
pub async fn vfs_get_blob_base64(
    blob_hash: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> VfsResult<VfsBlobBase64Result> {
    log::debug!("[VFS::handlers] vfs_get_blob_base64: hash={}", blob_hash);

    // ★ 规则12：获取连接后全程使用 _with_conn 方法，避免死锁
    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| format!("获取数据库连接失败: {}", e))?;
    let blobs_dir = vfs_db.blobs_dir();

    // 1. 获取 blob 元数据（使用已有连接）
    let blob = VfsBlobRepo::get_blob_with_conn(&conn, &blob_hash)
        .map_err(|e| format!("获取 blob 元数据失败: {}", e))?
        .ok_or_else(|| format!("Blob 不存在: {}", blob_hash))?;

    // 2. 获取 blob 文件路径（使用已有连接）
    let blob_path = VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &blob_hash)
        .map_err(|e| format!("获取 blob 路径失败: {}", e))?
        .ok_or_else(|| format!("Blob 文件路径不存在: {}", blob_hash))?;

    // 3. 读取文件内容
    let file_data = std::fs::read(&blob_path).map_err(|e| format!("读取 blob 文件失败: {}", e))?;

    // 4. 转换为 base64
    let base64_data = BASE64.encode(&file_data);

    log::info!(
        "[VFS::handlers] vfs_get_blob_base64: hash={}, size={} bytes",
        blob_hash,
        file_data.len()
    );

    Ok(VfsBlobBase64Result {
        base64: base64_data,
        mime_type: blob.mime_type.unwrap_or_else(|| "image/jpeg".to_string()),
        size: blob.size,
    })
}

/// 获取文件 blob 的绝对文件系统路径（供前端通过 pdfstream:// 协议加载）
///
/// ## 用途
/// 前端需要直接通过 pdfstream:// 协议访问 VFS blob 存储中的文件（如教材 PDF），
/// 而不是通过 base64 传输。此命令返回文件的绝对路径，前端调用
/// `convertFileSrc(path, 'pdfstream')` 将其转换为可用的 URL。
///
/// ## 参数
/// - `file_id`: 文件 ID（支持 `file_`、`tb_`、`att_`、`img_` 前缀）
///
/// ## 返回
/// - `Ok(Some(path))`: 文件的绝对路径
/// - `Ok(None)`: 文件未找到或没有 blob 存储
/// - `Err(String)`: 查询失败
#[tauri::command]
pub async fn vfs_get_blob_pdfstream_url(
    file_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> VfsResult<Option<String>> {
    log::debug!(
        "[VFS::handlers] vfs_get_blob_pdfstream_url: file_id={}",
        file_id
    );

    // 获取数据库连接和 blobs 目录
    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| VfsError::Database(format!("获取数据库连接失败: {}", e)))?;
    let blobs_dir = vfs_db.blobs_dir().to_path_buf();

    // 1. 从 files 表查询 blob_hash
    // 支持 file_, tb_, att_, img_ 前缀的 ID
    let blob_hash: Option<String> = conn
        .query_row(
            "SELECT blob_hash FROM files WHERE id = ?1",
            rusqlite::params![&file_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| VfsError::Database(format!("查询文件 blob_hash 失败: {}", e)))?
        .flatten();

    let blob_hash = match blob_hash {
        Some(h) if !h.trim().is_empty() => h,
        _ => {
            log::warn!(
                "[VFS::handlers] vfs_get_blob_pdfstream_url: no blob_hash for file_id={}",
                file_id
            );
            return Ok(None);
        }
    };

    // 2. 从 blobs 表查询 relative_path
    let relative_path: Option<String> = conn
        .query_row(
            "SELECT relative_path FROM blobs WHERE hash = ?1",
            rusqlite::params![&blob_hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| VfsError::Database(format!("查询 blob relative_path 失败: {}", e)))?
        .flatten();

    let relative_path = match relative_path {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            log::warn!(
                "[VFS::handlers] vfs_get_blob_pdfstream_url: blob record not found for hash={}",
                blob_hash
            );
            return Ok(None);
        }
    };

    // 3. 拼接绝对路径
    let abs_path = blobs_dir.join(&relative_path);

    log::info!(
        "[VFS::handlers] vfs_get_blob_pdfstream_url: file_id={}, blob_hash={}, path={:?}",
        file_id,
        &blob_hash[..16.min(blob_hash.len())],
        abs_path
    );

    Ok(Some(abs_path.to_string_lossy().to_string()))
}

// ============================================================================
// PDF 页面图片获取
// ============================================================================

/// 获取 PDF 指定页面的预渲染图片
///
/// 根据资源 ID 和页码获取 PDF 页面的预渲染图片。
/// 支持 textbook、attachment 类型的 PDF 资源。
///
/// ## 参数
/// - `resource_id`: 资源 ID（textbooks/attachments 表关联的 resource_id）
/// - `page_index`: 页码（0-indexed）
///
/// ## 返回
/// - `Ok(VfsBlobBase64Result)`: 包含 base64 数据和 mime_type
/// - `Err(String)`: 资源不存在、无预渲染数据、或页码越界
///
/// ## 使用场景
/// - RAG 检索结果中引用 PDF 页面时，前端调用此 API 获取页面图片
/// - 支持 OCR + 文本索引（有预渲染）和多模态索引两种场景
#[tauri::command]
pub async fn vfs_get_pdf_page_image(
    resource_id: String,
    page_index: usize,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> VfsResult<VfsBlobBase64Result> {
    use crate::vfs::types::PdfPreviewJson;

    log::debug!(
        "[VFS::handlers] vfs_get_pdf_page_image: resource_id={}, page_index={}",
        resource_id,
        page_index
    );

    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| format!("获取数据库连接失败: {}", e))?;
    let blobs_dir = vfs_db.blobs_dir();

    // 1. 获取资源信息，确定来源表
    let resource = VfsResourceRepo::get_resource_with_conn(&conn, &resource_id)
        .map_err(|e| format!("获取资源失败: {}", e))?
        .ok_or_else(|| format!("资源不存在: {}", resource_id))?;

    // 2. 根据 source_table 查询 preview_json
    let preview_json_str: Option<String> = match resource.source_table.as_deref() {
        Some("textbooks") => conn
            .query_row(
                "SELECT preview_json FROM files WHERE resource_id = ?1",
                rusqlite::params![&resource_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("查询教材 preview_json 失败: {}", e))?,
        Some("files") => conn
            .query_row(
                "SELECT preview_json FROM files WHERE resource_id = ?1",
                rusqlite::params![&resource_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("查询附件 preview_json 失败: {}", e))?,
        Some("exam_sheets") => conn
            .query_row(
                "SELECT preview_json FROM exam_sheets WHERE resource_id = ?1",
                rusqlite::params![&resource_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("查询题目集 preview_json 失败: {}", e))?,
        _ => None,
    };

    let preview_json_str = preview_json_str.ok_or_else(|| {
        format!(
            "资源无 PDF 预渲染数据: {} (source_table: {:?})",
            resource_id, resource.source_table
        )
    })?;

    // 3. 解析 preview_json 并查找 blob_hash 和 mime_type
    // 兼容 PdfPreviewJson（textbooks/files）和 ExamSheetPreviewResult（exam_sheets）两种格式
    let (blob_hash, mime_type): (String, String) =
        if resource.source_table.as_deref() == Some("exam_sheets") {
            // exam_sheets 使用 ExamSheetPreviewResult 格式
            use crate::models::ExamSheetPreviewResult;
            let preview: ExamSheetPreviewResult = serde_json::from_str(&preview_json_str)
                .map_err(|e| format!("解析 exam preview_json 失败: {}", e))?;
            let page = preview
                .pages
                .iter()
                .find(|p| p.page_index == page_index)
                .ok_or_else(|| {
                    format!(
                        "页码越界: page_index={}, total_pages={}",
                        page_index,
                        preview.pages.len()
                    )
                })?;
            let hash = page
                .blob_hash
                .clone()
                .ok_or_else(|| format!("页面 {} 无 blob_hash（可能是旧数据）", page_index))?;
            // exam_sheets 的页面默认是 PNG 格式
            (hash, "image/png".to_string())
        } else {
            // textbooks/files 使用 PdfPreviewJson 格式
            let preview: PdfPreviewJson = serde_json::from_str(&preview_json_str)
                .map_err(|e| format!("解析 preview_json 失败: {}", e))?;
            let page = preview
                .pages
                .iter()
                .find(|p| p.page_index == page_index)
                .ok_or_else(|| {
                    format!(
                        "页码越界: page_index={}, total_pages={}",
                        page_index, preview.total_pages
                    )
                })?;

            let (hash, mime) = if let Some(compressed) = page.compressed_blob_hash.as_ref() {
                if !compressed.is_empty() {
                    let mime_type = if compressed != &page.blob_hash {
                        "image/jpeg".to_string()
                    } else {
                        page.mime_type.clone()
                    };
                    (compressed.clone(), mime_type)
                } else {
                    (page.blob_hash.clone(), page.mime_type.clone())
                }
            } else {
                (page.blob_hash.clone(), page.mime_type.clone())
            };
            (hash, mime)
        };

    // 4. 获取 blob 元数据
    let blob = VfsBlobRepo::get_blob_with_conn(&conn, &blob_hash)
        .map_err(|e| format!("获取 blob 元数据失败: {}", e))?
        .ok_or_else(|| format!("Blob 不存在: {}", blob_hash))?;

    // 5. 获取 blob 文件路径
    let blob_path = VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &blob_hash)
        .map_err(|e| format!("获取 blob 路径失败: {}", e))?
        .ok_or_else(|| format!("Blob 文件路径不存在: {}", blob_hash))?;

    // 7. 读取文件内容
    let file_data = std::fs::read(&blob_path).map_err(|e| format!("读取 blob 文件失败: {}", e))?;

    // 8. 转换为 base64
    let base64_data = BASE64.encode(&file_data);

    log::info!(
        "[VFS::handlers] vfs_get_pdf_page_image: resource_id={}, page_index={}, size={} bytes",
        resource_id,
        page_index,
        file_data.len()
    );

    Ok(VfsBlobBase64Result {
        base64: base64_data,
        mime_type,
        size: blob.size,
    })
}

// ============================================================================
// PDF 预处理流水线命令
// ============================================================================

/// 获取 PDF 处理状态
///
/// ## 参数
/// - `file_id`: 文件 ID
///
/// ## 返回
/// - `ProcessingStatus`: 处理状态信息
#[tauri::command]
pub async fn vfs_get_pdf_processing_status(
    file_id: String,
    pdf_processing_service: State<
        '_,
        Arc<PdfProcessingService>,
    >,
) -> VfsResult<Option<ProcessingStatus>> {
    log::info!(
        "[VFS::handlers] vfs_get_pdf_processing_status: file_id={}",
        file_id
    );

    // 验证 file_id 格式
    validate_id_format_any(&file_id, &["file_", "tb_", "att_"], "file_id")?;

    // 使用 Tauri State 中的服务实例
    pdf_processing_service
        .get_status(&file_id)
}

/// 取消 PDF 处理
///
/// ## 参数
/// - `file_id`: 文件 ID
///
/// ## 返回
/// - `bool`: 是否成功取消（false 表示没有正在运行的任务）
#[tauri::command]
pub async fn vfs_cancel_pdf_processing(
    app_handle: tauri::AppHandle,
    file_id: String,
    pdf_processing_service: State<
        '_,
        Arc<PdfProcessingService>,
    >,
) -> VfsResult<bool> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::vfs::write_gate::check_vfs_write_gate(&app_handle.state::<crate::commands::AppState>())?;
    log::info!(
        "[VFS::handlers] vfs_cancel_pdf_processing: file_id={}",
        file_id
    );

    // 验证 file_id 格式
    validate_id_format_any(&file_id, &["file_", "tb_", "att_"], "file_id")?;

    pdf_processing_service
        .cancel(&file_id)
}

/// 重试 PDF 处理
///
/// ## 参数
/// - `file_id`: 文件 ID
#[tauri::command]
pub async fn vfs_retry_pdf_processing(
    app_handle: tauri::AppHandle,
    file_id: String,
    pdf_processing_service: State<
        '_,
        Arc<PdfProcessingService>,
    >,
) -> VfsResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::vfs::write_gate::check_vfs_write_gate(&app_handle.state::<crate::commands::AppState>())?;
    log::info!(
        "[VFS::handlers] vfs_retry_pdf_processing: file_id={}",
        file_id
    );

    // 验证 file_id 格式
    validate_id_format_any(&file_id, &["file_", "tb_", "att_"], "file_id")?;

    pdf_processing_service
        .retry(&file_id)
        .await
}

/// 启动 PDF 预处理流水线
///
/// ## 参数
/// - `file_id`: 文件 ID
/// - `start_from_stage`: 从哪个阶段开始（可选，默认从 OCR 阶段开始）
///
/// ## 说明
/// 此命令异步启动流水线，立即返回。
/// 前端应监听以下事件获取进度：
/// - `media-processing-progress`: 进度更新
/// - `media-processing-completed`: 处理完成
/// - `media-processing-error`: 处理错误
#[tauri::command]
pub async fn vfs_start_pdf_processing(
    app_handle: tauri::AppHandle,
    file_id: String,
    start_from_stage: Option<String>,
    pdf_processing_service: State<
        '_,
        Arc<PdfProcessingService>,
    >,
) -> VfsResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::vfs::write_gate::check_vfs_write_gate(&app_handle.state::<crate::commands::AppState>())?;
    log::info!(
        "[VFS::handlers] vfs_start_pdf_processing: file_id={}, start_from_stage={:?}",
        file_id,
        start_from_stage
    );

    // 验证 file_id 格式
    validate_id_format_any(&file_id, &["file_", "tb_", "att_"], "file_id")?;

    // 解析起始阶段
    let stage = start_from_stage.map(|s| ProcessingStage::from_str(&s));

    pdf_processing_service
        .start_pipeline(&file_id, stage)
        .await
}

/// 批量获取 PDF 处理状态
///
/// ## 参数
/// - `file_ids`: 文件 ID 列表
///
/// ## 返回
/// - `HashMap<String, ProcessingStatus>`: 文件 ID -> 处理状态映射
#[tauri::command]
pub async fn vfs_get_batch_pdf_processing_status(
    file_ids: Vec<String>,
    pdf_processing_service: State<
        '_,
        Arc<PdfProcessingService>,
    >,
) -> VfsResult<HashMap<String, ProcessingStatus>> {
    log::info!(
        "[VFS::handlers] vfs_get_batch_pdf_processing_status: count={}",
        file_ids.len()
    );

    let mut results = HashMap::new();

    for file_id in file_ids {
        if let Err(e) = validate_id_format_any(&file_id, &["file_", "tb_", "att_"], "file_id") {
            log::warn!(
                "[VFS::handlers] Invalid file_id in batch: {} - {}",
                file_id,
                e
            );
            continue;
        }

        match pdf_processing_service.get_status(&file_id) {
            Ok(Some(status)) => {
                results.insert(file_id, status);
            }
            Ok(None) => {
                log::debug!("[VFS::handlers] No processing status for file: {}", file_id);
            }
            Err(e) => {
                log::warn!(
                    "[VFS::handlers] Failed to get status for {}: {}",
                    file_id,
                    e
                );
            }
        }
    }

    Ok(results)
}

/// 列出待处理的 PDF 文件
///
/// ## 参数
/// - `limit`: 最大返回数量（默认 50）
///
/// ## 返回
/// - `Vec<VfsFile>`: 待处理的 PDF 文件列表
#[tauri::command]
pub async fn vfs_list_pending_pdf_processing(
    limit: Option<u32>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> VfsResult<Vec<VfsFile>> {
    use rusqlite::params;

    log::info!(
        "[VFS::handlers] vfs_list_pending_pdf_processing: limit={:?}",
        limit
    );

    let conn = vfs_db.get_conn_safe()?;
    let limit = limit.unwrap_or(50);

    let mut stmt = conn
        .prepare(
            r#"
        SELECT id, resource_id, blob_hash, sha256, file_name, original_path, size, page_count,
               "type", mime_type, tags_json, is_favorite, last_opened_at, last_page, bookmarks_json,
               cover_key, extracted_text, preview_json, ocr_pages_json, description,
               status, created_at, updated_at, deleted_at,
               processing_status, processing_progress, processing_error,
               processing_started_at, processing_completed_at,
               compressed_blob_hash
        FROM files
        WHERE mime_type = 'application/pdf'
          AND status = 'active'
          AND (processing_status = 'pending' OR processing_status IS NULL)
        ORDER BY created_at DESC
        LIMIT ?1
        "#,
        )
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let rows = stmt
        .query_map(params![limit], |row| {
            let tags_json: Option<String> = row.get(10)?;
            let bookmarks_json: Option<String> = row.get(14)?;

            let tags: Vec<String> = tags_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let bookmarks: Vec<serde_json::Value> = bookmarks_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            Ok(VfsFile {
                id: row.get(0)?,
                resource_id: row.get(1)?,
                blob_hash: row.get(2)?,
                sha256: row.get(3)?,
                file_name: row.get(4)?,
                original_path: row.get(5)?,
                size: row.get(6)?,
                page_count: row.get(7)?,
                file_type: row.get(8)?,
                mime_type: row.get(9)?,
                tags,
                is_favorite: row.get::<_, i32>(11)? != 0,
                last_opened_at: row.get(12)?,
                last_page: row.get(13)?,
                bookmarks,
                cover_key: row.get(15)?,
                extracted_text: row.get(16)?,
                preview_json: row.get(17)?,
                ocr_pages_json: row.get(18)?,
                description: row.get(19)?,
                status: row.get(20)?,
                created_at: row.get(21)?,
                updated_at: row.get(22)?,
                deleted_at: row.get(23)?,
                // PDF 预处理流水线字段
                processing_status: row.get(24)?,
                processing_progress: row.get(25)?,
                processing_error: row.get(26)?,
                processing_started_at: row.get(27)?,
                processing_completed_at: row.get(28)?,
                // ★ P0 架构改造：压缩图片字段
                compressed_blob_hash: row.get(29)?,
            })
        })
        .map_err(|e| format!("Failed to query: {}", e))?;

    let files: Vec<VfsFile> = rows
        .filter_map(|r| match r {
            Ok(val) => Some(val),
            Err(e) => {
                log::warn!("[VfsHandlers] Skipping malformed row: {}", e);
                None
            }
        })
        .collect();
    log::info!(
        "[VFS::handlers] vfs_list_pending_pdf_processing: found {} files",
        files.len()
    );

    Ok(files)
}

// ============================================================================
// 论文下载
// ============================================================================

/// 独立下载论文 PDF 并保存到 VFS（用于前端重试）
///
/// 不依赖 chat pipeline，直接下载 + 保存。
#[tauri::command]
pub async fn vfs_download_paper(
    app_handle: tauri::AppHandle,
    params: VfsDownloadPaperParams,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    pdf_processing_service: State<'_, Arc<PdfProcessingService>>,
) -> VfsResult<VfsDownloadPaperResult> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::vfs::write_gate::check_vfs_write_gate(&app_handle.state::<crate::commands::AppState>())?;
    log::info!(
        "[VFS::download_paper] Downloading '{}' from: {}",
        params.title,
        params.url
    );

    // 安全检查
    if !params.url.starts_with("https://") {
        return Err(VfsError::Other("Only HTTPS URLs are allowed".to_string()));
    }

    // 下载 PDF
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let response = client
        .get(&params.url)
        .header("User-Agent", "DeepStudent/1.0 (Academic Paper Save)")
        .send()
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    if !response.status().is_success() {
        return Err(VfsError::Other(format!("HTTP {}", response.status().as_u16())));
    }

    let pdf_bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Read failed: {}", e))?
        .to_vec();

    // PDF 签名验证
    if pdf_bytes.len() < 4 || &pdf_bytes[..4] != b"%PDF" {
        return Err(VfsError::Other("Downloaded file is not a valid PDF".to_string()));
    }

    // SHA256 去重
    let mut hasher = Sha256::new();
    hasher.update(&pdf_bytes);
    let sha256 = format!("{:x}", hasher.finalize());

    let conn = vfs_db.get_conn_safe()?;

    if let Ok(Some(existing)) = VfsFileRepo::get_by_sha256_with_conn(&conn, &sha256) {
        if existing.status == "active" {
            return Ok(VfsDownloadPaperResult {
                success: true,
                file_id: Some(existing.id),
                file_name: None,
                size_bytes: Some(pdf_bytes.len() as u64),
                page_count: existing.page_count,
                error: None,
            });
        }
    }

    // Blob 存储
    let blobs_dir = vfs_db.blobs_dir();
    let blob_hash = VfsBlobRepo::store_blob_with_conn(
        &conn,
        &blobs_dir,
        &pdf_bytes,
        Some("application/pdf"),
        None,
    )
    .map_err(|e| format!("Blob storage failed: {}", e))?
    .hash;

    // PDF 预览 + 文本提取（spawn_blocking 避免阻塞 tokio 线程）
    let (preview_json, extracted_text, page_count) = {
        let vfs_db_clone = vfs_db.inner().clone();
        let blobs_dir_clone = blobs_dir.to_path_buf();
        let pdf_bytes_clone = pdf_bytes.clone();
        match tokio::task::spawn_blocking(move || {
            let conn = vfs_db_clone.get_conn_safe()?;
            render_pdf_preview(
                &conn,
                &blobs_dir_clone,
                &pdf_bytes_clone,
                &PdfPreviewConfig::default(),
            )
        })
        .await
        {
            Ok(Ok(result)) => {
                let preview_str = result
                    .preview_json
                    .as_ref()
                    .and_then(|p| serde_json::to_string(p).ok());
                (
                    preview_str,
                    result.extracted_text,
                    Some(result.page_count as i32),
                )
            }
            Ok(Err(e)) => {
                log::warn!("[VFS::download_paper] PDF preview failed: {}", e);
                (None, None, None)
            }
            Err(e) => {
                log::warn!("[VFS::download_paper] PDF render task panicked: {}", e);
                (None, None, None)
            }
        }
    };

    // 文件名
    let safe_title = params
        .title
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'], "_");
    let file_name = if safe_title.to_lowercase().ends_with(".pdf") {
        safe_title
    } else {
        format!("{}.pdf", safe_title)
    };

    let folder_id = params.folder_id.as_deref().filter(|s| !s.is_empty());

    let file = VfsFileRepo::create_file_with_doc_data_in_folder(
        &conn,
        &sha256,
        &file_name,
        pdf_bytes.len() as i64,
        "pdf",
        Some("application/pdf"),
        Some(&blob_hash),
        None,
        folder_id,
        preview_json.as_deref(),
        extracted_text.as_deref(),
        page_count,
    )
    .map_err(|e| format!("File creation failed: {}", e))?;

    // 索引
    if let Some(ref resource_id) = file.resource_id {
        let index_service = VfsIndexService::new((*vfs_db).clone());
        let input = UnitBuildInput {
            resource_id: resource_id.clone(),
            resource_type: "file".to_string(),
            data: None,
            ocr_text: None,
            ocr_pages_json: None,
            blob_hash: Some(blob_hash.clone()),
            page_count: file.page_count,
            extracted_text: file.extracted_text.clone(),
            preview_json: file.preview_json.clone(),
        };
        let _ = index_service.sync_resource_units(input);
    }

    // 异步 PDF Pipeline
    {
        let file_id = file.id.clone();
        let service = (*pdf_processing_service).clone();
        tokio::spawn(async move {
            let _ = service
                .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
                .await;
        });
    }

    Ok(VfsDownloadPaperResult {
        success: true,
        file_id: Some(file.id),
        file_name: Some(file_name),
        size_bytes: Some(pdf_bytes.len() as u64),
        page_count,
        error: None,
    })
}
