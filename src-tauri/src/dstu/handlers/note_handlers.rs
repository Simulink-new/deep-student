//! 笔记 (Note) 类型处理器
//!
//! 处理笔记特有的 DSTU 操作逻辑

use std::sync::Arc;

use tauri::Window;

use crate::vfs::{VfsCreateNoteParams, VfsDatabase, VfsNoteRepo, VfsUpdateNoteParams};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{
    note_to_dstu_node, parse_timestamp,
};
use super::super::types::{DstuCreateOptions, DstuNode, DstuNodeType};

/// 获取笔记
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsNoteRepo::get_note(vfs_db, id) {
        Ok(Some(note)) => Ok(Some(note_to_dstu_node(&note))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_note error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建笔记
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    _window: &Window,
    options: &DstuCreateOptions,
    _path: &str,
    _resource_type: &str,
) -> DstuResult<DstuNode> {
    // 创建笔记
    let note = match VfsNoteRepo::create_note(
        vfs_db,
        VfsCreateNoteParams {
            title: options.name.clone(),
            content: options.content.clone().unwrap_or_default(),
            tags: options.tags.clone().unwrap_or_default(),
        },
    ) {
        Ok(n) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=note, id={}",
                n.id
            );
            n
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=note, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 将笔记添加到文件夹（如果指定了 folder_id）
    if let Some(ref folder_id) = options.resolved_folder_id() {
        let _ = crate::vfs::VfsFolderRepo::add_item_to_folder(
            vfs_db,
            &crate::vfs::VfsFolderItem::new(
                Some(folder_id.clone()),
                "note".to_string(),
                note.id.clone(),
            ),
        );
    }

    Ok(note_to_dstu_node(&note))
}

/// 更新笔记内容
pub async fn handle_update(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    content: &str,
) -> DstuResult<DstuNode> {
    let updated_note = match VfsNoteRepo::update_note(
        vfs_db,
        id,
        VfsUpdateNoteParams {
            content: Some(content.to_string()),
            title: None,
            tags: None,
            expected_updated_at: None,
        },
    ) {
        Ok(n) => {
            log::info!(
                "[DSTU::handlers] dstu_update: SUCCESS - type=note, id={}",
                id
            );
            n
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_update: FAILED - type=note, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(note_to_dstu_node(&updated_note))
}

/// 删除笔记（软删除）
pub async fn handle_delete(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<()> {
    match VfsNoteRepo::delete_note(vfs_db, id) {
        Ok(_) => {
            log::info!(
                "[DSTU::handlers] dstu_delete: SUCCESS - type=note, id={}",
                id
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_delete: FAILED - type=note, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 重命名笔记
pub async fn handle_rename(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    new_name: &str,
) -> DstuResult<DstuNode> {
    let updated_note = match VfsNoteRepo::update_note(
        vfs_db,
        id,
        VfsUpdateNoteParams {
            title: Some(new_name.to_string()),
            content: None,
            tags: None,
            expected_updated_at: None,
        },
    ) {
        Ok(n) => {
            log::info!(
                "[DSTU::handlers] dstu_rename: SUCCESS - type=note, id={}",
                id
            );
            n
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_rename: FAILED - type=note, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(note_to_dstu_node(&updated_note))
}

/// 复制笔记
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let note = match VfsNoteRepo::get_note(vfs_db, src_id) {
        Ok(Some(n)) => n,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - note not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_note error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let content = match VfsNoteRepo::get_note_content(vfs_db, src_id) {
        Ok(Some(c)) => c,
        Ok(None) => String::new(),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_note_content error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let new_note = match VfsNoteRepo::create_note(
        vfs_db,
        VfsCreateNoteParams {
            title: format!("{} (副本)", note.title),
            content,
            tags: note.tags.clone(),
        },
    ) {
        Ok(n) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created copy, id={}",
                n.id
            );
            n
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_note error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 如果指定了目标文件夹，将新资源添加到文件夹
    if let Some(ref folder_id) = dest_folder_id {
        let folder_item = crate::vfs::VfsFolderItem::new(
            Some(folder_id.clone()),
            "note".to_string(),
            new_note.id.clone(),
        );
        if let Err(e) = crate::vfs::VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] dstu_copy: failed to add note to folder {}: {}",
                folder_id,
                e
            );
        }
    }

    Ok(note_to_dstu_node(&new_note))
}

/// 设置笔记收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsNoteRepo::set_favorite(vfs_db, id, favorite) {
        Ok(_) => log::info!(
            "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=note, id={}, favorite={}",
            id,
            favorite
        ),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=note, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    }

    let note = match VfsNoteRepo::get_note(vfs_db, id) {
        Ok(Some(n)) => n,
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - note not found after set_favorite, id={}",
                id
            );
            return Err(DstuError::from("操作失败".to_string()));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - get_note error, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(note_to_dstu_node(&note))
}

/// 恢复已删除的笔记
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsNoteRepo::get_note(vfs_db, id) {
        Ok(Some(n)) => Ok(Some(note_to_dstu_node(&n))),
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: note not found after restore, id={}",
                id
            );
            Ok(None)
        }
        Err(e) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: get_note error, id={}, error={}",
                id,
                e
            );
            Ok(None)
        }
    }
}

/// 列出已删除的笔记
pub fn handle_list_deleted(
    vfs_db: &Arc<VfsDatabase>,
    limit: u32,
    offset: u32,
) -> DstuResult<Vec<DstuNode>> {
    let notes = match VfsNoteRepo::list_deleted_notes(vfs_db, limit, offset) {
        Ok(n) => n,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_list_deleted: FAILED - list_deleted_notes error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let nodes: Vec<DstuNode> = notes
        .into_iter()
        .map(|n| {
            let path = format!("/{}", n.id);
            let created_at = parse_timestamp(&n.created_at);
            let updated_at = parse_timestamp(&n.updated_at);

            DstuNode {
                id: n.id.clone(),
                source_id: n.id.clone(),
                name: n.title.clone(),
                path,
                node_type: DstuNodeType::Note,
                size: None,
                created_at,
                updated_at,
                children: None,
                child_count: None,
                resource_id: Some(n.resource_id),
                resource_hash: None,
                preview_type: Some("markdown".to_string()),
                metadata: Some(serde_json::json!({
                    "tags": n.tags,
                    "is_favorite": n.is_favorite,
                    "deleted_at": n.deleted_at,
                })),
            }
        })
        .collect();

    Ok(nodes)
}

/// 根据路径获取笔记
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsNoteRepo::get_note(vfs_db, resource_id) {
        Ok(Some(note)) => Ok(Some(note_to_dstu_node(&note))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
