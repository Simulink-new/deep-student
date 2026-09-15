//! 文件附件 (File) 类型处理器
//!
//! 处理文件附件特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{
    VfsBlobRepo, VfsDatabase, VfsFileRepo,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::file_to_dstu_node;
use super::super::types::{DstuCreateOptions, DstuNode};

/// 获取文件（通过 VfsFileRepo）
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsFileRepo::get_file(vfs_db, id) {
        Ok(Some(file)) => Ok(Some(file_to_dstu_node(&file))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_file error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建文件
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    _path: &str,
    folder_id: Option<String>,
) -> DstuResult<DstuNode> {
    let file_data = options
        .resolved_file_data()
        .ok_or_else(|| DstuError::from("文件创建需要 file_data 参数".to_string()))?;

    let decoded = match BASE64.decode(file_data) {
        Ok(d) => d,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - base64 decode error: {}",
                e
            );
            return Err(DstuError::from(format!("Base64 解码失败: {}", e)));
        }
    };

    let metadata = options.metadata.as_ref();

    let default_mime = "application/octet-stream";
    let raw_mime = metadata
        .and_then(|m| m.get("mimeType"))
        .and_then(|v| v.as_str())
        .unwrap_or(default_mime);
    let mime_type = if raw_mime.contains('/') {
        raw_mime
    } else {
        log::warn!(
            "[DSTU::handlers] dstu_create: invalid mime type '{}', fallback to {}",
            raw_mime,
            default_mime
        );
        default_mime
    };

    let extension = match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => mime_type.split('/').last().unwrap_or("bin"),
    };

    let blob = match VfsBlobRepo::store_blob(vfs_db, &decoded, Some(mime_type), Some(extension)) {
        Ok(b) => {
            log::info!("[DSTU::handlers] dstu_create: blob stored, hash={}", b.hash);
            b
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - blob store error: {}",
                e
            );
            return Err(DstuError::from(format!("Failed to store blob: {}", e)));
        }
    };

    let file_name = if options.name.trim().is_empty() {
        format!("unnamed.{}", extension)
    } else {
        options.name.clone()
    };

    let file = match VfsFileRepo::create_file_in_folder(
        vfs_db,
        &blob.hash,
        &file_name,
        decoded.len() as i64,
        "file",
        Some(mime_type),
        Some(&blob.hash),
        None,
        folder_id.as_deref(),
    ) {
        Ok(f) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=file, id={}, name='{}'",
                f.id,
                f.file_name
            );
            f
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=file, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(file_to_dstu_node(&file))
}
