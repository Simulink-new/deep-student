//! 教材 (Textbook) 类型处理器
//!
//! 处理教材特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{VfsDatabase, VfsTextbookRepo, VfsFolderItem, VfsFolderRepo};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{
    parse_timestamp, textbook_to_dstu_node,
};
use super::super::types::{DstuCreateOptions, DstuNode, DstuNodeType};

/// 获取教材
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTextbookRepo::get_textbook(vfs_db, id) {
        Ok(Some(textbook)) => Ok(Some(textbook_to_dstu_node(&textbook))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_textbook error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建教材
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    folder_id: Option<String>,
) -> DstuResult<DstuNode> {
    // 教材需要 file_data（兼容前端只传 fileBase64 的情况）
    let file_data = options
        .resolved_file_data()
        .ok_or_else(|| DstuError::from("教材创建需要 file_data 参数".to_string()))?;

    // 解码 Base64 内容
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use super::super::super::vfs::VfsBlobRepo;

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

    // 存储文件到 Blob
    let blob = match VfsBlobRepo::store_blob(vfs_db, &decoded, Some("application/pdf"), Some("pdf"))
    {
        Ok(b) => b,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - blob store error: {}",
                e
            );
            return Err(DstuError::from(format!("Failed to store blob: {}", e)));
        }
    };

    let file_name = if options.name.trim().is_empty() {
        format!("unnamed.pdf")
    } else {
        let name = options.name.trim();
        if name.ends_with(".pdf") {
            name.to_string()
        } else {
            format!("{}.pdf", name)
        }
    };

    let textbook = match VfsTextbookRepo::create_textbook(
        vfs_db,
        &blob.hash,
        &file_name,
        decoded.len() as i64,
        Some(&blob.hash),
        None,
    ) {
        Ok(t) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=textbook, id={}, name='{}'",
                t.id,
                t.file_name
            );
            t
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=textbook, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 将教材添加到文件夹
    if let Some(ref fid) = folder_id {
        let folder_item = VfsFolderItem::new(
            Some(fid.clone()),
            "textbook".to_string(),
            textbook.id.clone(),
        );
        let _ = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item);
    }

    Ok(textbook_to_dstu_node(&textbook))
}

/// 删除教材（软删除）
pub async fn handle_delete(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<()> {
    match VfsTextbookRepo::delete_textbook(vfs_db, id) {
        Ok(_) => {
            log::info!(
                "[DSTU::handlers] dstu_delete: SUCCESS - type=textbook, id={}",
                id
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_delete: FAILED - type=textbook, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 重命名教材
pub async fn handle_rename(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    new_name: &str,
) -> DstuResult<DstuNode> {
    // 教材重命名通过 update_file_name 实现
    match VfsTextbookRepo::update_file_name(vfs_db, id, new_name) {
        Ok(tb) => {
            log::info!(
                "[DSTU::handlers] dstu_rename: SUCCESS - type=textbook, id={}",
                id
            );
            Ok(textbook_to_dstu_node(&tb))
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_rename: FAILED - type=textbook, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 复制教材
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let textbook = match VfsTextbookRepo::get_textbook(vfs_db, src_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - textbook not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_textbook error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let new_file_name = format!("{} (副本)", textbook.file_name.trim_end_matches(".pdf"));
    let new_file_name = if textbook.file_name.ends_with(".pdf") {
        format!("{}.pdf", new_file_name)
    } else {
        new_file_name
    };

    let new_sha256 = format!(
        "{}_{}",
        textbook.sha256,
        chrono::Utc::now().timestamp_millis()
    );

    let new_textbook = match VfsTextbookRepo::create_textbook(
        vfs_db,
        &new_sha256,
        &new_file_name,
        textbook.size,
        textbook.blob_hash.as_deref(),
        textbook.original_path.as_deref(),
    ) {
        Ok(t) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created textbook copy, id={}",
                t.id
            );
            t
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_textbook error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = dest_folder_id {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "textbook".to_string(),
            new_textbook.id.clone(),
        );
        if let Err(e) = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] dstu_copy: failed to add textbook to folder {}: {}",
                folder_id,
                e
            );
        }
    }

    Ok(textbook_to_dstu_node(&new_textbook))
}

/// 设置教材收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsTextbookRepo::set_favorite(vfs_db, id, favorite) {
        Ok(_) => log::info!(
            "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=textbook, id={}, favorite={}",
            id,
            favorite
        ),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=textbook, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    }

    let textbook = match VfsTextbookRepo::get_textbook(vfs_db, id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - textbook not found after set_favorite, id={}",
                id
            );
            return Err(DstuError::from("操作失败".to_string()));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - get_textbook error, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(textbook_to_dstu_node(&textbook))
}

/// 恢复已删除的教材
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTextbookRepo::get_textbook(vfs_db, id) {
        Ok(Some(t)) => Ok(Some(textbook_to_dstu_node(&t))),
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: textbook not found after restore, id={}",
                id
            );
            Ok(None)
        }
        Err(e) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: get_textbook error, id={}, error={}",
                id,
                e
            );
            Ok(None)
        }
    }
}

/// 列出已删除的教材
pub fn handle_list_deleted(
    vfs_db: &Arc<VfsDatabase>,
    limit: u32,
    offset: u32,
) -> DstuResult<Vec<DstuNode>> {
    let textbooks = match VfsTextbookRepo::list_deleted_textbooks(vfs_db, limit, offset) {
        Ok(t) => t,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_list_deleted: FAILED - list_deleted_textbooks error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let nodes: Vec<DstuNode> = textbooks
        .into_iter()
        .map(|tb| {
            let path = format!("/{}", tb.id);
            let created_at = parse_timestamp(&tb.created_at);
            let updated_at = parse_timestamp(&tb.updated_at);

            DstuNode {
                id: tb.id.clone(),
                source_id: tb.id.clone(),
                name: tb.file_name.clone(),
                path,
                node_type: DstuNodeType::Textbook,
                size: Some(tb.size as u64),
                created_at,
                updated_at,
                children: None,
                child_count: None,
                resource_id: tb.resource_id,
                resource_hash: None,
                preview_type: Some("pdf".to_string()),
                metadata: Some(serde_json::json!({
                    "is_favorite": tb.is_favorite,
                })),
            }
        })
        .collect();

    Ok(nodes)
}

/// 根据路径获取教材
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTextbookRepo::get_textbook(vfs_db, resource_id) {
        Ok(Some(tb)) => Ok(Some(textbook_to_dstu_node(&tb))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
