//! 教材库命令模块
//! 从 commands.rs 剥离 (原始行号: 7077-7400)

use crate::commands::{read_file_bytes, AppState};
use crate::document_parser::DocumentParser;
use crate::models::AppError;
use crate::textbooks_db::{ListQuery as TextbooksListQuery, Textbook as TextbookDto, TextbooksDb};
use crate::unified_file_manager;
use crate::vfs::repos::blob_repo::VfsBlobRepo;
use crate::vfs::repos::pdf_preview::{render_pdf_preview_with_progress, PdfPreviewConfig};
// ★ 2026-02 移除：VfsIndexService 和 UnitBuildInput 不再需要
// sync_resource_units 调用已移除，由 Pipeline 统一处理
use crate::vfs::indexing::types::OcrPagesJson;
use crate::vfs::{PdfProcessingService, ProcessingStage};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, State, Window};
use tracing::{info, warn};

/// PDF 导入进度事件
#[derive(Debug, Clone, Serialize)]
pub struct TextbookImportProgress {
    /// 当前文件名
    pub file_name: String,
    /// 当前阶段: "hashing" | "copying" | "rendering" | "saving" | "done" | "error"
    pub stage: String,
    /// 当前页码（仅 rendering 阶段有效）
    pub current_page: Option<usize>,
    /// 总页数（仅 rendering 阶段有效）
    pub total_pages: Option<usize>,
    /// 进度百分比 0-100
    pub progress: u8,
    /// 错误信息（仅 error 阶段有效）
    pub error: Option<String>,
}

type Result<T> = std::result::Result<T, AppError>;

fn attach_textbook_to_folder(
    vfs_db: &crate::vfs::VfsDatabase,
    textbook_id: &str,
    folder_id: Option<&str>,
) {
    if let Some(fid) = folder_id {
        let folder_item = crate::vfs::VfsFolderItem::new(
            Some(fid.to_string()),
            "file".to_string(),
            textbook_id.to_string(),
        );
        if let Err(e) = crate::vfs::VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            warn!(
                "[Textbooks] Failed to attach textbook {} to folder {}: {}",
                textbook_id, fid, e
            );
        }
    }
}

fn emit_textbook_watch_event(window: &Window, textbook_id: &str, event_type: &str) {
    let dstu_path = format!("/{}", textbook_id);
    let watch_event = serde_json::json!({
        "type": event_type,
        "path": dstu_path,
    });

    if let Err(err) = window.emit(&format!("dstu:change:{}", dstu_path), &watch_event) {
        warn!(
            "[Textbooks] Failed to emit dstu:change:{} for {}: {}",
            event_type, textbook_id, err
        );
    }
    if let Err(err) = window.emit("dstu:change", &watch_event) {
        warn!(
            "[Textbooks] Failed to emit global dstu:change:{} for {}: {}",
            event_type, textbook_id, err
        );
    }
}

fn start_textbook_pipeline_if_needed(
    pdf_processing_service: &Arc<PdfProcessingService>,
    textbook_id: &str,
    extension: &str,
) {
    if extension != "pdf" {
        return;
    }

    let textbook_id = textbook_id.to_string();
    let pdf_service = pdf_processing_service.clone();
    tokio::spawn(async move {
        info!(
            "[Textbooks] Starting PDF pipeline for textbook: {}",
            textbook_id
        );
        if let Err(e) = pdf_service
            .start_pipeline(&textbook_id, Some(ProcessingStage::OcrProcessing))
            .await
        {
            warn!(
                "[Textbooks] Failed to start PDF pipeline for textbook {}: {}",
                textbook_id, e
            );
        }
    });
}

// ==================== 教材库（独立数据库）命令 ====================

#[tauri::command]
pub async fn textbooks_add(
    window: Window,
    state: State<'_, AppState>,
    pdf_processing_service: State<'_, Arc<PdfProcessingService>>,
    sources: Vec<String>,
    folder_id: Option<String>,
) -> Result<Vec<TextbookDto>> {
    if sources.is_empty() {
        return Ok(vec![]);
    }

    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    // 辅助函数：发送进度事件
    let emit_progress = |window: &Window,
                         file_name: &str,
                         stage: &str,
                         current_page: Option<usize>,
                         total_pages: Option<usize>,
                         progress: u8,
                         error: Option<String>| {
        log::info!(
            "📤 [Textbook] 发送进度事件: file={}, stage={}, page={:?}/{:?}, progress={}%",
            file_name,
            stage,
            current_page,
            total_pages,
            progress
        );
        let payload = TextbookImportProgress {
            file_name: file_name.to_string(),
            stage: stage.to_string(),
            current_page,
            total_pages,
            progress,
            error,
        };
        if let Err(err) = window.emit("textbook-import-progress", &payload) {
            warn!(
                "[Textbooks] 发送 textbook-import-progress 事件失败: file={}, stage={}, err={}",
                file_name, stage, err
            );
        }
    };

    let mut out: Vec<TextbookDto> = Vec::new();
    let mut skipped_reasons: Vec<String> = Vec::new();

    for src in &sources {
        // ★ Android 修复：使用三层降级策略解析文件名和扩展名
        // Layer 1: URI 路径提取（适用于 ExternalStorage / raw: 路径）
        // Layer 2: Magic bytes 检测（适用于 Media Provider / Downloads 等不透明 ID）
        // Layer 3: 无法识别 → 跳过并记录原因
        let (resolved_name, resolved_ext) = unified_file_manager::resolve_file_info(&window, src);
        // ★ 移动端修复：当 URI 提取的文件名是不透明 document ID（如 446、document:123）时，
        // 生成用户友好的显示名称，避免在 UI 上显示无意义的数字 ID
        let uri_raw_name = unified_file_manager::extract_file_name(src);
        let display_name_owned = if unified_file_manager::is_opaque_document_id(&uri_raw_name) {
            let ext_suffix = resolved_ext
                .as_ref()
                .map(|e| format!(".{}", e))
                .unwrap_or_default();
            format!(
                "导入文档_{}{}",
                chrono::Utc::now().format("%Y%m%d_%H%M%S"),
                ext_suffix
            )
        } else {
            resolved_name.clone()
        };
        let display_name = display_name_owned.as_str();

        info!(
            "[Textbooks] Resolved file info: uri={}, name={}, ext={:?}",
            src, display_name, resolved_ext
        );

        // ★ 校验提前：在哈希和复制之前验证扩展名
        let extension = match resolved_ext {
            Some(ref ext) if ext == "pdf" => ext.clone(),
            Some(ref ext) => {
                let supported_extensions = [
                    "docx", "txt", "md", "markdown", "xlsx", "xls", "xlsb", "ods", "html", "htm",
                    "pptx", "epub", "rtf", "csv", "json", "xml",
                ];
                if supported_extensions.contains(&ext.as_str()) {
                    ext.clone()
                } else {
                    let reason = format!("{}: 不支持的文件格式 ({})", display_name, ext);
                    warn!("[Textbooks] {}", reason);
                    emit_progress(
                        &window,
                        display_name,
                        "error",
                        None,
                        None,
                        0,
                        Some(format!("不支持的文件格式: {}", ext)),
                    );
                    skipped_reasons.push(reason);
                    continue;
                }
            }
            None => {
                let reason = format!("{}: 无法识别文件格式", display_name);
                warn!("[Textbooks] {}", reason);
                emit_progress(
                    &window,
                    display_name,
                    "error",
                    None,
                    None,
                    0,
                    Some("无法识别文件格式，请确认文件类型后重试".to_string()),
                );
                skipped_reasons.push(reason);
                continue;
            }
        };

        // 阶段1：计算哈希
        emit_progress(&window, display_name, "hashing", None, None, 5, None);
        let sha256 = match unified_file_manager::hash_file_sha256(&window, src) {
            Ok(h) => h,
            Err(e) => {
                let reason = format!("{}: 读取文件失败 ({})", display_name, e);
                warn!("[Textbooks] {}", reason);
                emit_progress(
                    &window,
                    display_name,
                    "error",
                    None,
                    None,
                    0,
                    Some(format!("读取文件失败: {}", e)),
                );
                skipped_reasons.push(reason);
                continue;
            }
        };

        // 若已存在，恢复并重新挂载到目标位置
        if let Some(tb) = crate::vfs::VfsTextbookRepo::get_by_sha256(vfs_db, &sha256)
            .map_err(|e| AppError::database(format!("VFS 查询教材失败: {}", e)))?
        {
            let mut watch_event_type = "created";
            if tb.status != "active" {
                crate::vfs::VfsTextbookRepo::restore_textbook(vfs_db, &tb.id)
                    .map_err(|e| AppError::database(format!("VFS 恢复教材失败: {}", e)))?;
                watch_event_type = "restored";
            }
            attach_textbook_to_folder(vfs_db, &tb.id, folder_id.as_deref());
            emit_textbook_watch_event(&window, &tb.id, watch_event_type);
            start_textbook_pipeline_if_needed(pdf_processing_service.inner(), &tb.id, &extension);
            emit_progress(&window, display_name, "done", None, None, 100, None);
            out.push(tb.to_textbook());
            continue;
        }

        let size = unified_file_manager::get_file_size(&window, src).unwrap_or(0);
        let file_name = display_name.to_string();

        // 阶段3：根据文件类型处理
        let conn = vfs_db
            .get_conn_safe()
            .map_err(|e| AppError::database(format!("获取 VFS 连接失败: {}", e)))?;

        let blobs_dir = vfs_db.blobs_dir();

        let (preview_json_str, extracted_text, page_count, blob_hash) = if extension == "pdf" {
            // PDF 文件：使用 PDF 预渲染流程
            emit_progress(&window, &file_name, "rendering", Some(0), None, 15, None);
            let pdf_bytes = read_file_bytes(window.clone(), src.to_string())
                .await
                .map_err(|e| {
                    emit_progress(
                        &window,
                        &file_name,
                        "error",
                        None,
                        None,
                        0,
                        Some(format!("读取 PDF 失败: {}", e)),
                    );
                    AppError::file_system(format!("读取 PDF 文件失败: {}", e))
                })?;

            // ★ PDF-403 修复：将 PDF 二进制内容存储为 VFS blob，确保即使 original_path
            // 在 pdfstream 协议目录白名单之外也能通过 vfs_get_attachment_content 正常读取
            let pdf_blob_hash: Option<String> = match VfsBlobRepo::store_blob_with_conn(
                &conn,
                &blobs_dir,
                &pdf_bytes,
                Some("application/pdf"),
                Some("pdf"),
            ) {
                Ok(blob) => {
                    info!(
                        "[Textbooks] Stored PDF blob: hash={}, path={:?}",
                        blob.hash,
                        VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &blob.hash)
                            .ok().flatten()
                    );
                    Some(blob.hash)
                }
                Err(e) => {
                    warn!("[Textbooks] Failed to store PDF blob, will rely on original_path: {}", e);
                    None
                }
            };

            let window_clone = window.clone();
            let file_name_clone = file_name.clone();
            let progress_callback = move |current_page: usize, total_pages: usize| {
                let render_progress =
                    ((current_page as f32 / total_pages as f32) * 70.0) as u8 + 15;
                let payload = TextbookImportProgress {
                    file_name: file_name_clone.clone(),
                    stage: "rendering".to_string(),
                    current_page: Some(current_page),
                    total_pages: Some(total_pages),
                    progress: render_progress.min(85),
                    error: None,
                };
                if let Err(err) = window_clone.emit("textbook-import-progress", &payload) {
                    warn!(
                        "[Textbooks] 发送渲染进度事件失败: file={}, page={}/{}, err={}",
                        file_name_clone, current_page, total_pages, err
                    );
                }
            };

            let (preview, text, pages) = match render_pdf_preview_with_progress(
                &conn,
                &blobs_dir,
                &pdf_bytes,
                &PdfPreviewConfig::default(),
                progress_callback,
            ) {
                Ok(result) => {
                    let preview_str = result
                        .preview_json
                        .as_ref()
                        .and_then(|p| serde_json::to_string(p).ok());
                    info!(
                        "[Textbooks] PDF preview rendered: {} pages, text_len={}, has_preview={}",
                        result.page_count,
                        result.extracted_text.as_ref().map(|t| t.len()).unwrap_or(0),
                        preview_str.is_some()
                    );
                    (
                        preview_str,
                        result.extracted_text,
                        Some(result.page_count as i32),
                    )
                }
                Err(e) => {
                    warn!(
                        "[Textbooks] PDF preview failed, storing without preview: {}",
                        e
                    );
                    (None, None, None)
                }
            };
            (preview, text, pages, pdf_blob_hash)
        } else {
            // 非 PDF 文件：使用 DocumentParser 提取文本
            emit_progress(&window, &file_name, "parsing", None, None, 15, None);
            let parser = DocumentParser::new();
            match parser.extract_text_from_path(src) {
                Ok(text) => {
                    info!(
                        "[Textbooks] Document text extracted: {} chars from {}",
                        text.len(),
                        file_name
                    );
                    (None, Some(text), Some(1), None)
                }
                Err(e) => {
                    warn!(
                        "[Textbooks] Document parsing failed for {}: {}",
                        file_name, e
                    );
                    emit_progress(
                        &window,
                        &file_name,
                        "error",
                        None,
                        None,
                        0,
                        Some(format!("文档解析失败: {}", e)),
                    );
                    skipped_reasons.push(format!("{}: 文档解析失败 ({})", display_name, e));
                    continue;
                }
            }
        };

        // ★ 扫描版 PDF 检测：提取文本过少（每页 < 100 字符）则标记为扫描版
        let (is_scanned, needs_ocr) = if extension == "pdf" {
            match (extracted_text.as_deref(), page_count) {
                (Some(text), Some(pages)) if pages > 0 => {
                    let per_page = text.len() as f64 / pages as f64;
                    let scanned = per_page < 100.0;
                    (Some(scanned), Some(scanned))
                }
                (None, _) => (Some(true), Some(true)), // 无提取文本 → 扫描版
                _ => (None, None),
            }
        } else {
            (None, None)
        };

        // 阶段4：入库
        emit_progress(&window, &file_name, "saving", None, None, 90, None);
        let tb = crate::vfs::VfsTextbookRepo::create_textbook_with_preview(
            &conn,
            &sha256,
            &file_name,
            size as i64,
            blob_hash.as_deref(), // blob_hash (PDF-403: stored VFS blob, may be None on error)
            Some(src), // original_path
            preview_json_str.as_deref(),
            extracted_text.as_deref(),
            page_count,
            is_scanned,
            needs_ocr,
        )
        .map_err(|e| {
            emit_progress(
                &window,
                &file_name,
                "error",
                None,
                None,
                0,
                Some(format!("入库失败: {}", e)),
            );
            AppError::database(format!("VFS 创建教材失败: {}", e))
        })?;

        // ★ 创建教材后，将其挂载到指定文件夹（若有 folder_id）
        if let Some(ref fid) = folder_id {
            let folder_item = crate::vfs::VfsFolderItem::new(
                Some(fid.to_string()),
                "file".to_string(),
                tb.id.clone(),
            );
            crate::vfs::VfsFolderRepo::add_item_to_folder_with_conn(&conn, &folder_item)
                .map_err(|e| AppError::database(format!("VFS 挂载教材失败: {}", e)))?;
        }

        // ★ 2026-02 修复：移除 sync_resource_units 调用
        // 原因：Pipeline 的 stage_vector_indexing 会统一处理 Units 同步
        // 这里提前同步会导致 index_resource 内部再次同步时产生冲突
        emit_progress(&window, &file_name, "indexing", None, None, 95, None);

        // ★ 2026-02 修复：PDF 上传后异步触发 Pipeline（从 OCR 阶段开始）
        // Stage 1-2（文本提取、页面渲染）已在上面完成
        start_textbook_pipeline_if_needed(pdf_processing_service.inner(), &tb.id, &extension);
        emit_textbook_watch_event(&window, &tb.id, "created");

        // 阶段5：完成
        emit_progress(&window, &file_name, "done", None, None, 100, None);
        out.push(tb.to_textbook());
    }

    // ★ Android 修复：当所有文件都被跳过时，通过 progress 事件发送汇总原因
    if out.is_empty() && !skipped_reasons.is_empty() {
        let summary = skipped_reasons.join("; ");
        info!("[Textbooks] All files skipped. Reasons: {}", summary);
        emit_progress(&window, "", "error", None, None, 0, Some(summary));
    }

    Ok(out)
}

#[tauri::command]
pub async fn textbooks_list(
    state: State<'_, AppState>,
    query: Option<TextbooksListQuery>,
) -> Result<Vec<TextbookDto>> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let q = query.unwrap_or(TextbooksListQuery {
        q: None,
        favorite: None,
        status: None,
        limit: Some(500),
        offset: Some(0),
        sort_by: Some("time".into()),
        order: Some("desc".into()),
    });

    let limit = q.limit.unwrap_or(500) as u32;
    let offset = q.offset.unwrap_or(0) as u32;
    // VFS 版本：include_global = true 以包含全局教材
    let vfs_items = TextbooksDb::list_vfs(vfs_db, None, true, limit, offset)?;

    // 转换为旧版 TextbookDto 以保持兼容性
    let items: Vec<TextbookDto> = vfs_items.into_iter().map(|v| v.to_textbook()).collect();
    Ok(items)
}

#[tauri::command]
pub async fn textbooks_remove(
    window: Window,
    state: State<'_, AppState>,
    id: String,
) -> Result<bool> {
    warn!(
        "[Textbooks] textbooks_remove is deprecated; prefer DSTU trash/purge flows for new callers. id={}",
        id
    );
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let deleted = TextbooksDb::delete_vfs(vfs_db, &id)?;

    if deleted {
        emit_textbook_watch_event(&window, &id, "purged");
    }

    Ok(deleted)
}

/// 采用已有文件（不复制），直接计算哈希并入库
#[tauri::command]
pub async fn textbooks_adopt(
    window: Window,
    state: State<'_, AppState>,
    pdf_processing_service: State<'_, Arc<PdfProcessingService>>,
    paths: Vec<String>,
    folder_id: Option<String>,
) -> Result<Vec<TextbookDto>> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let mut out: Vec<TextbookDto> = Vec::new();
    for p in paths {
        let size = unified_file_manager::get_file_size(&window, &p)?;
        if size == 0 {
            continue;
        }
        let sha256 = unified_file_manager::hash_file_sha256(&window, &p)?;
        let (resolved_name, resolved_ext) = unified_file_manager::resolve_file_info(&window, &p);
        // ★ 移动端修复：不透明 document ID → 生成友好文件名
        let uri_raw_name = unified_file_manager::extract_file_name(&p);
        let file_name = if unified_file_manager::is_opaque_document_id(&uri_raw_name) {
            let ext_suffix = resolved_ext
                .as_ref()
                .map(|e| format!(".{}", e))
                .unwrap_or_default();
            format!(
                "导入文档_{}{}",
                Utc::now().format("%Y%m%d_%H%M%S"),
                ext_suffix
            )
        } else {
            resolved_name
        };
        let extension = resolved_ext.unwrap_or_else(|| {
            std::path::Path::new(&file_name)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_lowercase()
        });

        if let Some(tb) = crate::vfs::VfsTextbookRepo::get_by_sha256(vfs_db, &sha256)
            .map_err(|e| AppError::database(format!("VFS 查询教材失败: {}", e)))?
        {
            let mut watch_event_type = "created";
            if tb.status != "active" {
                crate::vfs::VfsTextbookRepo::restore_textbook(vfs_db, &tb.id)
                    .map_err(|e| AppError::database(format!("VFS 恢复教材失败: {}", e)))?;
                watch_event_type = "restored";
            }
            attach_textbook_to_folder(vfs_db, &tb.id, folder_id.as_deref());
            emit_textbook_watch_event(&window, &tb.id, watch_event_type);
            start_textbook_pipeline_if_needed(pdf_processing_service.inner(), &tb.id, &extension);
            out.push(tb.to_textbook());
            continue;
        }

        let conn = vfs_db
            .get_conn_safe()
            .map_err(|e| AppError::database(format!("获取 VFS 连接失败: {}", e)))?;
        let blobs_dir = vfs_db.blobs_dir();

        let (preview_json_str, extracted_text, page_count, blob_hash) = if extension == "pdf" {
            let pdf_bytes = read_file_bytes(window.clone(), p.clone())
                .await
                .map_err(|e| AppError::file_system(format!("读取 PDF 文件失败: {}", e)))?;

            // ★ PDF-403 修复：将 PDF 二进制内容存储为 VFS blob
            let pdf_blob_hash: Option<String> = match VfsBlobRepo::store_blob_with_conn(
                &conn,
                &blobs_dir,
                &pdf_bytes,
                Some("application/pdf"),
                Some("pdf"),
            ) {
                Ok(blob) => {
                    info!(
                        "[Textbooks] adopt: Stored PDF blob: hash={}, path={:?}",
                        blob.hash,
                        VfsBlobRepo::get_blob_path_with_conn(&conn, &blobs_dir, &blob.hash)
                            .ok().flatten()
                    );
                    Some(blob.hash)
                }
                Err(e) => {
                    warn!("[Textbooks] adopt: Failed to store PDF blob, will rely on original_path: {}", e);
                    None
                }
            };

            let (preview, text, pages) = match render_pdf_preview_with_progress(
                &conn,
                &blobs_dir,
                &pdf_bytes,
                &PdfPreviewConfig::default(),
                |_current_page, _total_pages| {},
            ) {
                Ok(result) => (
                    result
                        .preview_json
                        .as_ref()
                        .and_then(|preview| serde_json::to_string(preview).ok()),
                    result.extracted_text,
                    Some(result.page_count as i32),
                ),
                Err(e) => {
                    warn!(
                        "[Textbooks] PDF preview failed during adopt, storing without preview: {}",
                        e
                    );
                    (None, None, None)
                }
            };
            (preview, text, pages, pdf_blob_hash)
        } else {
            let parser = DocumentParser::new();
            match parser.extract_text_from_path(&p) {
                Ok(text) => (None, Some(text), Some(1), None),
                Err(e) => {
                    warn!(
                        "[Textbooks] Document parsing failed during adopt for {}: {}",
                        file_name, e
                    );
                    (None, None, None, None)
                }
            }
        };

        // ★ 扫描版 PDF 检测：提取文本过少（每页 < 100 字符）则标记为扫描版
        let (is_scanned, needs_ocr) = if extension == "pdf" {
            match (extracted_text.as_deref(), page_count) {
                (Some(text), Some(pages)) if pages > 0 => {
                    let per_page = text.len() as f64 / pages as f64;
                    let scanned = per_page < 100.0;
                    (Some(scanned), Some(scanned))
                }
                (None, _) => (Some(true), Some(true)),
                _ => (None, None),
            }
        } else {
            (None, None)
        };

        let tb = crate::vfs::VfsTextbookRepo::create_textbook_with_preview(
            &conn,
            &sha256,
            &file_name,
            size as i64,
            blob_hash.as_deref(), // blob_hash (PDF-403: stored VFS blob)
            Some(&p), // original_path
            preview_json_str.as_deref(),
            extracted_text.as_deref(),
            page_count,
            is_scanned,
            needs_ocr,
        )
        .map_err(|e| AppError::database(format!("VFS 创建教材失败: {}", e)))?;

        attach_textbook_to_folder(vfs_db, &tb.id, folder_id.as_deref());

        emit_textbook_watch_event(&window, &tb.id, "created");
        start_textbook_pipeline_if_needed(pdf_processing_service.inner(), &tb.id, &extension);

        out.push(tb.to_textbook());
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeTrashOptions {
    pub delete_files: Option<bool>,
}

/// 恢复回收站中的教材
#[tauri::command]
pub async fn textbooks_recover(state: State<'_, AppState>, id: String) -> Result<bool> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;
    crate::vfs::VfsTextbookRepo::restore_textbook(vfs_db, &id)
        .map_err(|e| AppError::database(format!("VFS 恢复教材失败: {}", e)))?;
    Ok(true)
}

/// 清空回收站（可选物理删除文件）
#[tauri::command]
pub async fn textbooks_purge_trash(
    _window: Window,
    state: State<'_, AppState>,
    options: Option<PurgeTrashOptions>,
) -> Result<serde_json::Value> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let delete_files = options.and_then(|o| o.delete_files).unwrap_or(false);
    let mut deleted_files: Vec<String> = Vec::new();

    if delete_files {
        // 先获取所有已删除的教材，删除物理文件
        let trashed = crate::vfs::VfsTextbookRepo::list_deleted_textbooks(vfs_db, 10000, 0)
            .map_err(|e| AppError::database(format!("VFS 列出回收站失败: {}", e)))?;
        for tb in &trashed {
            if let Some(ref path) = tb.original_path {
                // content:// 等虚拟 URI 无法通过 std::fs 操作，跳过物理删除
                if unified_file_manager::is_virtual_uri(path) {
                    continue;
                }
                if std::path::Path::new(path).exists() {
                    if let Err(e) = std::fs::remove_file(path) {
                        warn!("[Textbooks] 删除文件失败: {} ({})", path, e);
                    } else {
                        deleted_files.push(path.clone());
                    }
                }
            }
        }
    }

    let purged = crate::vfs::VfsFileRepo::purge_deleted_files(vfs_db)
        .map_err(|e| AppError::database(format!("VFS 清空回收站失败: {}", e)))?;
    Ok(serde_json::json!({ "purged": purged, "deleted_files": deleted_files }))
}

/// 永久删除单个教材（可选物理删除）
#[tauri::command]
pub async fn textbooks_delete_permanent(
    _window: Window,
    state: State<'_, AppState>,
    id: String,
    delete_file: Option<bool>,
) -> Result<bool> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    // 如果需要删除物理文件，先获取教材信息
    if delete_file.unwrap_or(false) {
        if let Ok(Some(tb)) = crate::vfs::VfsTextbookRepo::get_textbook(vfs_db, &id) {
            if let Some(ref path) = tb.original_path {
                // content:// 等虚拟 URI 无法通过 std::fs 操作，跳过物理删除
                if !unified_file_manager::is_virtual_uri(path) {
                    let p = std::path::Path::new(path);
                    if p.exists() {
                        if let Err(err) = std::fs::remove_file(p) {
                            warn!(
                                "[Textbooks] 永久删除教材时清理文件失败: {} ({})",
                                p.display(),
                                err
                            );
                        }
                    }
                }
            }
        }
    }

    crate::vfs::VfsTextbookRepo::purge_textbook_with_folder_item(vfs_db, &id)
        .map_err(|e| AppError::database(format!("VFS 永久删除教材失败: {}", e)))?;
    Ok(true)
}

/// 更新教材阅读进度（打开时间和页码）
#[tauri::command]
pub async fn textbooks_update_reading_progress(
    state: State<'_, AppState>,
    id: String,
    last_page: Option<i64>,
) -> Result<bool> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;
    let params = crate::textbooks_db::VfsUpdateTextbookParams {
        last_page: last_page.map(|p| p as i32),
        ..Default::default()
    };
    TextbooksDb::update_vfs(vfs_db, &id, params)?;
    Ok(true)
}

/// 设置教材收藏状态
#[tauri::command]
pub async fn textbooks_set_favorite(
    state: State<'_, AppState>,
    id: String,
    favorite: bool,
) -> Result<bool> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;
    let params = crate::textbooks_db::VfsUpdateTextbookParams {
        favorite: Some(favorite),
        ..Default::default()
    };
    TextbooksDb::update_vfs(vfs_db, &id, params)?;
    Ok(true)
}

/// 更新教材页数
#[tauri::command]
pub async fn textbooks_update_page_count(
    state: State<'_, AppState>,
    id: String,
    page_count: i64,
) -> Result<bool> {
    // ★ 切换到 VFS 版本
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;
    let params = crate::textbooks_db::VfsUpdateTextbookParams {
        page_count: Some(page_count as i32),
        ..Default::default()
    };
    TextbooksDb::update_vfs(vfs_db, &id, params)?;
    Ok(true)
}

/// 更新教材书签
#[tauri::command]
pub async fn textbooks_update_bookmarks(
    state: State<'_, AppState>,
    id: String,
    bookmarks: Vec<serde_json::Value>,
) -> Result<bool> {
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;
    let params = crate::textbooks_db::VfsUpdateTextbookParams {
        bookmarks: Some(bookmarks),
        ..Default::default()
    };
    TextbooksDb::update_vfs(vfs_db, &id, params)?;
    Ok(true)
}

// ==================== Blob 修复命令 ====================

/// 单个教材 Blob 修复结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairTextbookBlobResult {
    /// 教材 ID
    pub textbook_id: String,
    /// 是否修复成功
    pub success: bool,
    /// 描述信息
    pub message: String,
    /// 原始路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    /// 旧的 blob_hash（修复前）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_blob_hash: Option<String>,
    /// 新的 blob_hash（修复后）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_blob_hash: Option<String>,
}

/// 批量修复结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairAllTextbookBlobsResult {
    /// 扫描的教材总数
    pub total: usize,
    /// 成功修复数
    pub repaired: usize,
    /// 跳过数（引用正常或无需修复）
    pub skipped: usize,
    /// 失败数
    pub failed: usize,
    /// 错误详情
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// 每项的修复详情
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<RepairTextbookBlobResult>,
}

/// 修复单个教材的 Blob 引用
///
/// 如果 textbook 的 blob_hash 为 NULL 或指向不存在的 blob，
/// 则尝试从 original_path 重新读取文件内容并存储为新的 VFS blob。
#[tauri::command]
pub async fn vfs_repair_textbook_blob(
    window: Window,
    state: State<'_, AppState>,
    textbook_id: String,
) -> Result<RepairTextbookBlobResult> {
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| AppError::database(format!("获取 VFS 连接失败: {}", e)))?;

    let blobs_dir = vfs_db.blobs_dir();

    // 1. 查询教材
    let textbook = match crate::vfs::VfsTextbookRepo::get_textbook_with_conn(&conn, &textbook_id) {
        Ok(Some(tb)) => tb,
        Ok(None) => {
            return Ok(RepairTextbookBlobResult {
                textbook_id,
                success: false,
                message: "教材不存在".to_string(),
                original_path: None,
                old_blob_hash: None,
                new_blob_hash: None,
            });
        }
        Err(e) => {
            return Ok(RepairTextbookBlobResult {
                textbook_id,
                success: false,
                message: format!("查询教材失败: {}", e),
                original_path: None,
                old_blob_hash: None,
                new_blob_hash: None,
            });
        }
    };

    let old_blob_hash = textbook.blob_hash.clone();
    let original_path = textbook.original_path.clone();

    // 2. 检查 blob_hash 是否有效
    let blob_valid = match &old_blob_hash {
        Some(hash) => match crate::vfs::repos::blob_repo::VfsBlobRepo::blob_exists_with_conn(
            &conn, hash,
        ) {
            Ok(exists) => exists,
            Err(_) => false,
        },
        None => false,
    };

    if blob_valid {
        return Ok(RepairTextbookBlobResult {
            textbook_id,
            success: true,
            message: "Blob 引用有效，无需修复".to_string(),
            original_path,
            old_blob_hash,
            new_blob_hash: None,
        });
    }

    // 3. 检查 original_path
    let orig_path = match &original_path {
        Some(p) if !p.trim().is_empty() => p.clone(),
        _ => {
            return Ok(RepairTextbookBlobResult {
                textbook_id,
                success: false,
                message: "教材无 original_path，无法修复".to_string(),
                original_path,
                old_blob_hash,
                new_blob_hash: None,
            });
        }
    };

    // 4. 读取文件内容
    let file_bytes = match unified_file_manager::read_all_bytes(&window, &orig_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Ok(RepairTextbookBlobResult {
                textbook_id,
                success: false,
                message: format!("读取原始文件失败: {}", e),
                original_path,
                old_blob_hash,
                new_blob_hash: None,
            });
        }
    };

    // 5. 存储为新的 VFS blob
    // 从文件名推断扩展名和 MIME 类型
    let ext = std::path::Path::new(&textbook.file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    let mime = ext.as_deref().and_then(|e| match e {
        "pdf" => Some("application/pdf"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "mp4" => Some("video/mp4"),
        "mp3" => Some("audio/mpeg"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        _ => None,
    });

    let blob = match crate::vfs::repos::blob_repo::VfsBlobRepo::store_blob_with_conn(
        &conn,
        blobs_dir,
        &file_bytes,
        mime,
        ext.as_deref(),
    ) {
        Ok(b) => b,
        Err(e) => {
            return Ok(RepairTextbookBlobResult {
                textbook_id,
                success: false,
                message: format!("存储 Blob 失败: {}", e),
                original_path,
                old_blob_hash,
                new_blob_hash: None,
            });
        }
    };

    let new_hash = blob.hash;

    // 6. 更新 files 表中的 blob_hash
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    if let Err(e) = conn.execute(
        "UPDATE files SET blob_hash = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![&new_hash, &now, &textbook_id],
    ) {
        return Ok(RepairTextbookBlobResult {
            textbook_id,
            success: false,
            message: format!("更新 blob_hash 失败: {}", e),
            original_path,
            old_blob_hash,
            new_blob_hash: Some(new_hash),
        });
    }

    info!(
        "[Textbooks] Repaired blob reference for textbook {}: {:?} -> {}",
        textbook_id, old_blob_hash, new_hash
    );

    Ok(RepairTextbookBlobResult {
        textbook_id,
        success: true,
        message: "Blob 引用已修复".to_string(),
        original_path,
        old_blob_hash,
        new_blob_hash: Some(new_hash),
    })
}

/// 修复所有教材中损坏的 Blob 引用
///
/// 遍历所有 active 状态的教材，逐一检查 blob_hash 的有效性，
/// 对有问题的教材尝试从 original_path 重建 blob 引用。
#[tauri::command]
pub async fn vfs_repair_all_textbook_blobs(
    window: Window,
    state: State<'_, AppState>,
) -> Result<RepairAllTextbookBlobsResult> {
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| AppError::database(format!("获取 VFS 连接失败: {}", e)))?;

    // 1. 列出所有 active 教材
    let textbooks = crate::vfs::VfsTextbookRepo::list_all_textbooks_with_conn(&conn, 10000, 0)
        .map_err(|e| AppError::database(format!("列出教材失败: {}", e)))?;

    let total = textbooks.len();
    let mut repaired: usize = 0;
    let mut skipped: usize = 0;
    let mut failed: usize = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut details: Vec<RepairTextbookBlobResult> = Vec::new();

    for tb in &textbooks {
        // 对每个教材检查并修复 blob 引用
        let blob_hash = tb.blob_hash.clone();
        let orig_path = tb.original_path.clone();

        // 检查 blob 有效性
        let blob_valid = match &blob_hash {
            Some(hash) => {
                match crate::vfs::repos::blob_repo::VfsBlobRepo::blob_exists_with_conn(
                    &conn, hash,
                ) {
                    Ok(exists) => exists,
                    Err(_) => false,
                }
            }
            None => false,
        };

        if blob_valid {
            skipped += 1;
            details.push(RepairTextbookBlobResult {
                textbook_id: tb.id.clone(),
                success: true,
                message: "Blob 引用有效，无需修复".to_string(),
                original_path: orig_path,
                old_blob_hash: blob_hash,
                new_blob_hash: None,
            });
            continue;
        }

        // original_path 检查
        let orig_path_str = match &orig_path {
            Some(p) if !p.trim().is_empty() => p.clone(),
            _ => {
                failed += 1;
                errors.push(format!("{}: 无 original_path", tb.id));
                details.push(RepairTextbookBlobResult {
                    textbook_id: tb.id.clone(),
                    success: false,
                    message: "教材无 original_path".to_string(),
                    original_path: None,
                    old_blob_hash: blob_hash,
                    new_blob_hash: None,
                });
                continue;
            }
        };

        // 读取文件
        let file_bytes = match unified_file_manager::read_all_bytes(&window, &orig_path_str) {
            Ok(bytes) => bytes,
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: 读取文件失败 - {}", tb.id, e));
                details.push(RepairTextbookBlobResult {
                    textbook_id: tb.id.clone(),
                    success: false,
                    message: format!("读取原始文件失败: {}", e),
                    original_path: Some(orig_path_str),
                    old_blob_hash: blob_hash,
                    new_blob_hash: None,
                });
                continue;
            }
        };

        // 存储 blob
        let ext = std::path::Path::new(&tb.file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        let mime = ext.as_deref().and_then(|e| match e {
            "pdf" => Some("application/pdf"),
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "mp4" => Some("video/mp4"),
            "mp3" => Some("audio/mpeg"),
            "docx" => Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            _ => None,
        });

        let blob = match crate::vfs::repos::blob_repo::VfsBlobRepo::store_blob_with_conn(
            &conn,
            vfs_db.blobs_dir(),
            &file_bytes,
            mime,
            ext.as_deref(),
        ) {
            Ok(b) => b,
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: 存储 blob 失败 - {}", tb.id, e));
                details.push(RepairTextbookBlobResult {
                    textbook_id: tb.id.clone(),
                    success: false,
                    message: format!("存储 Blob 失败: {}", e),
                    original_path: Some(orig_path_str),
                    old_blob_hash: blob_hash,
                    new_blob_hash: None,
                });
                continue;
            }
        };

        let new_hash = blob.hash;

        // 更新 files 表
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();

        match conn.execute(
            "UPDATE files SET blob_hash = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![&new_hash, &now, &tb.id],
        ) {
            Ok(_) => {
                repaired += 1;
                info!(
                    "[Textbooks] Batch repair: fixed blob for textbook {}: {:?} -> {}",
                    tb.id, blob_hash, new_hash
                );
                details.push(RepairTextbookBlobResult {
                    textbook_id: tb.id.clone(),
                    success: true,
                    message: "Blob 引用已修复".to_string(),
                    original_path: Some(orig_path_str),
                    old_blob_hash: blob_hash,
                    new_blob_hash: Some(new_hash),
                });
            }
            Err(e) => {
                failed += 1;
                errors.push(format!("{}: 更新 blob_hash 失败 - {}", tb.id, e));
                details.push(RepairTextbookBlobResult {
                    textbook_id: tb.id.clone(),
                    success: false,
                    message: format!("更新 blob_hash 失败: {}", e),
                    original_path: Some(orig_path_str),
                    old_blob_hash: blob_hash,
                    new_blob_hash: Some(new_hash),
                });
            }
        }
    }

    info!(
        "[Textbooks] Batch blob repair complete: total={}, repaired={}, skipped={}, failed={}",
        total, repaired, skipped, failed
    );

    Ok(RepairAllTextbookBlobsResult {
        total,
        repaired,
        skipped,
        failed,
        errors,
        details,
    })
}

/// 确保教材的 OCR 流水线已启动
///
/// 对已有但尚未完成 OCR 的扫描版 PDF 教材，自动触发 OCR 流水线。
/// 1. 检查文件是否已有 ocr_pages_json → 已完成则跳过
/// 2. 检测是否为扫描版（每页提取文本 < 100 字符）→ 触发 OCR 流水线
/// 3. 返回处理状态供前端判断
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsEnsureOcrPipelineResponse {
    /// 状态: "ocr_completed" | "ocr_started" | "text_sufficient" | "not_applicable"
    pub status: String,
    /// 描述信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[tauri::command]
pub async fn vfs_ensure_ocr_pipeline(
    state: State<'_, AppState>,
    pdf_processing_service: State<'_, Arc<PdfProcessingService>>,
    file_id: String,
    force: Option<bool>,
) -> Result<VfsEnsureOcrPipelineResponse> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    // 本命令错误类型为 AppError, 在调用点手工映射（消息保留"请稍后重试"语义）。
    crate::vfs::write_gate::check_vfs_write_gate(&state)
        .map_err(|e| AppError::unknown(e.to_string()))?;

    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| AppError::configuration("VFS database not configured"))?;

    // 1. 查询文件 — 在块作用域中获取连接并释放，避免在后续 .await 期间持有连接池连接（连接池饥饿修复）
    let file = {
        let conn = vfs_db
            .get_conn_safe()
            .map_err(|e| AppError::database(format!("获取 VFS 连接失败: {}", e)))?;

        crate::vfs::VfsFileRepo::get_file_with_conn(&conn, &file_id)
            .map_err(|e| AppError::database(format!("查询文件失败: {}", e)))?
            .ok_or_else(|| AppError::not_found(format!("文件不存在: {}", file_id)))?
        // conn 在此作用域结束时自动归还到连接池
    };

    // ★★ 诊断日志 — 记录文件 OCR 状态
    // ★ task-049: 增加 preview_pages/ocr_completed —— 「preview 截断」与「半成品检查点」
    // 是本类故障的两个判据,日志中应直接可见。
    let diag_preview_pages = file
        .preview_json
        .as_deref()
        .and_then(|pj| serde_json::from_str::<serde_json::Value>(pj).ok())
        .and_then(|v| v.get("pages").and_then(|p| p.as_array()).map(|a| a.len()));
    let diag_ocr_completed = file
        .ocr_pages_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("completedAt").and_then(|c| c.as_str()).map(|s| !s.is_empty()))
        .unwrap_or(false);
    info!(
        "[OCR_DIAG] vfs_ensure_ocr_pipeline called: file_id={}, has_ocr_json={}, ocr_completed={}, has_preview={}, preview_pages={:?}, text_len={}, page_count={:?}, processing_status={:?}",
        file_id,
        file.ocr_pages_json.as_ref().map(|s| !s.is_empty() && s != "{}").unwrap_or(false),
        diag_ocr_completed,
        file.preview_json.as_ref().map(|s| !s.is_empty() && s != "{}").unwrap_or(false),
        diag_preview_pages,
        file.extracted_text.as_ref().map(|t| t.len()).unwrap_or(0),
        file.page_count,
        file.processing_status,
    );

    // ★★ 检查文件状态：序列化冲突和系统占用
    // ★ P0-3 修复：以 running_tasks 内存状态为"正在运行"的唯一判据。
    // DB 中的 pending/中间态可能是进程被杀后的旧状态残留
    //（recover_stuck_tasks 会把中断文件重置为 pending，但未必全部自动恢复），
    // 仅凭 DB 状态拒绝会造成永远无法重启的死锁（ensure 拒绝 + retry 不收）。
    let actually_running = pdf_processing_service.is_running(&file_id);
    if let Some(ref status) = file.processing_status {
        let looks_active = matches!(
            status.as_str(),
            "pending" | "processing" | "ocr_processing" | "page_compression" | "page_rendering"
        );
        if looks_active {
            if actually_running {
                return Ok(VfsEnsureOcrPipelineResponse {
                    status: "already_running".to_string(),
                    message: Some(format!("文件正在处理中 ({}), 请等待完成", status)),
                });
            }
            // 旧状态残留：打日志后继续走下方检查点/启动逻辑，让流水线自愈
            info!(
                "[Textbooks] Stale processing_status '{}' for file {} (no running task in memory), treating as resumable",
                status, file_id
            );
        }
    }

    // ★★ 检查 processing_progress 中的阶段：同样以内存运行状态为准
    if let Some(ref progress) = file.processing_progress {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(progress) {
            let stage = parsed.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(stage, "ocr_processing" | "page_compression" | "page_rendering") {
                if actually_running {
                    return Ok(VfsEnsureOcrPipelineResponse {
                        status: "already_running".to_string(),
                        message: Some(format!("OCR 正在进行中 (stage: {})", stage)),
                    });
                }
                info!(
                    "[Textbooks] Stale progress stage '{}' for file {} (no running task in memory), treating as resumable",
                    stage, file_id
                );
            }
        }
    }

    // 2. 检查 OCR 是否已完成（解析检查点，区分部分完成和全部完成）
    if let Some(ref ocr_json_str) = file.ocr_pages_json {
        if !ocr_json_str.is_empty() && ocr_json_str != "{}" {
            // 尝试解析 completed_at 字段判断是否真正完成
            let is_completed = serde_json::from_str::<serde_json::Value>(ocr_json_str)
                .ok()
                .and_then(|v| v.get("completedAt").cloned())
                .and_then(|v| v.as_str().map(|s| !s.is_empty()))
                .unwrap_or(false);

            if is_completed {
                // ★ task-045 修复: 用户手动"重新 OCR"(force=true)时强制重启流水线。
                // 旧实现对已完成 OCR 只补笔记直接返回——任务从未重启,前端表现为
                // "成功+失败双弹窗(后续 info 查询失败)且无进度条"。
                if force.unwrap_or(false) {
                    pdf_processing_service.mark_force_ocr(&file_id);
                    pdf_processing_service
                        .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
                        .await
                        .map_err(|e| {
                            pdf_processing_service.unmark_force_ocr(&file_id);
                            AppError::database(format!("强制重启 OCR 流水线失败: {}", e))
                        })?;
                    return Ok(VfsEnsureOcrPipelineResponse {
                        status: "ocr_started".to_string(),
                        message: Some("OCR 流水线已强制重启（忽略已有结果）".to_string()),
                    });
                }
                // ★ task-049 修复：历史截断 preview 自愈。
                // 2026-06 前旧版渲染有 50 页上限,遗留文件 OCR「完成」但只有前 50 页。
                // completedAt 已落库使下方 completed 分支直接返回,残缺被永久固化。
                // 检测 preview 页数 < 真实页数 → 重启流水线(非 force:
                // 流水线内会全量重渲染 preview 并以续跑模式只补缺失页)。
                let preview_page_count = file
                    .preview_json
                    .as_deref()
                    .and_then(|pj| serde_json::from_str::<serde_json::Value>(pj).ok())
                    .and_then(|v| v.get("pages").and_then(|p| p.as_array()).map(|a| a.len()));
                let real_pages = file.page_count.map(|p| p as usize).unwrap_or(0);
                if let Some(pp) = preview_page_count {
                    if real_pages > 0 && pp < real_pages {
                        info!(
                            "[Textbooks] Stale preview for file {} ({}/{} pages), restarting pipeline to complete remaining pages",
                            file_id, pp, real_pages
                        );
                        pdf_processing_service
                            .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
                            .await
                            .map_err(|e| {
                                AppError::database(format!("启动 OCR 补全流水线失败: {}", e))
                            })?;
                        return Ok(VfsEnsureOcrPipelineResponse {
                            status: "ocr_resumed".to_string(),
                            message: Some(format!(
                                "检测到页面渲染不完整({}/{} 页),已启动补全流水线",
                                pp, real_pages
                            )),
                        });
                    }
                }
                // ★ 检查 OCR 笔记是否存在，如果缺失则补建
                let note_id = pdf_processing_service.ensure_ocr_note(&file_id).await;
                if let Some(ref nid) = note_id {
                    info!(
                        "[Textbooks] OCR note ensured for file {}: note_id={}",
                        file_id, nid
                    );
                    return Ok(VfsEnsureOcrPipelineResponse {
                        status: "note_created".to_string(),
                        message: Some(format!("OCR 笔记已补建: {}", nid)),
                    });
                }
                return Ok(VfsEnsureOcrPipelineResponse {
                    status: "ocr_completed".to_string(),
                    message: Some("OCR 已完成（笔记创建失败或无可提取文本）".to_string()),
                });
            }

            // OCR 检查点存在但未完成 → 恢复处理
            info!(
                "[Textbooks] Found incomplete OCR checkpoint for file {}, resuming pipeline",
                file_id
            );
            // ★ task-049: 续跑不标记 force_ocr —— force 会让 stage-3 丢弃检查点全页重跑,
            // 毁掉断点续传(旧实现在此标 force,与「用户手动重新 OCR」共用同一标记)。
            // stage-3 门控现已按 completedAt 区分完成态,半成品检查点会自动重开 OCR 阶段。
            pdf_processing_service
                .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
                .await
                .map_err(|e| {
                    AppError::database(format!("启动 OCR 流水线失败: {}", e))
                })?;

            return Ok(VfsEnsureOcrPipelineResponse {
                status: "ocr_resumed".to_string(),
                message: Some("OCR 流水线已恢复（从检查点继续）".to_string()),
            });
        }
    }

    // 3. 检查是否为扫描版（提取文本过少）或没有 preview_json
    let is_scanned = match (&file.extracted_text, file.page_count) {
        (Some(text), Some(pages)) if pages > 0 => {
            let per_page = text.len() as f64 / pages as f64;
            per_page < 100.0
        }
        (None, _) => true,  // 无提取文本 → 扫描版
        _ => false,
    };

    // ★ 增强：即使文本足够，如果 preview_json 存在但没有 OCR，也检查 OCR 设置
    let has_preview = file.preview_json.as_ref().map(|p| !p.is_empty() && p != "{}").unwrap_or(false);
    let ocr_wanted = is_scanned || (has_preview && file.ocr_pages_json.is_none());

    if ocr_wanted {
        // ★ Fix 3: 标记强制 OCR，标记将在 run_pdf_pipeline_internal 的
        // ForceOcrGuard 中自动清理（包括任何非正常退出路径）
        pdf_processing_service.mark_force_ocr(&file_id);
        // 触发 OCR 流水线
        pdf_processing_service
            .start_pipeline(&file_id, Some(ProcessingStage::OcrProcessing))
            .await
            .map_err(|e| {
                // start_pipeline 失败（任务生成前）时手动清理
                pdf_processing_service.unmark_force_ocr(&file_id);
                AppError::database(format!("启动 OCR 流水线失败: {}", e))
            })?;

        info!(
            "[Textbooks] OCR pipeline started for file: {} (scanned={}, has_preview={})",
            file_id, is_scanned, has_preview
        );

        Ok(VfsEnsureOcrPipelineResponse {
            status: "ocr_started".to_string(),
            message: Some("OCR 流水线已启动".to_string()),
        })
    } else {
        Ok(VfsEnsureOcrPipelineResponse {
            status: "text_sufficient".to_string(),
            message: Some("文本提取已足够，无需 OCR".to_string()),
        })
    }
}

/// ★ 获取单页 OCR Markdown 内容（按需计算，避免加载全部页面到编辑器卡死）
#[tauri::command]
pub async fn get_ocr_page_md(
    state: State<'_, AppState>,
    file_id: String,
    page_index: usize,
) -> std::result::Result<String, String> {
    let vfs_db = state
        .vfs_db
        .as_ref()
        .ok_or_else(|| "VFS database not configured".to_string())?;

    let conn = vfs_db
        .get_conn_safe()
        .map_err(|e| format!("Failed to get DB connection: {}", e))?;

    let json_str: String = conn
        .query_row(
            "SELECT ocr_pages_json FROM files WHERE id = ?1",
            params![file_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to load OCR data: {}", e))?;

    if json_str.is_empty() || json_str == "{}" {
        return Err("No OCR data available".to_string());
    }

    let ocr_json: OcrPagesJson =
        serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse OCR data: {}", e))?;

    let page = ocr_json
        .pages
        .iter()
        .find(|p| p.page_index == page_index)
        .ok_or_else(|| format!("Page {} not found (total: {})", page_index + 1, ocr_json.pages.len()))?;

    let mut md = format!("# Page {}\n\n", page_index + 1);
    for block in &page.blocks {
        if !block.text.trim().is_empty() {
            md.push_str(&block.text);
            md.push('\n');
        }
    }

    Ok(md)
}
