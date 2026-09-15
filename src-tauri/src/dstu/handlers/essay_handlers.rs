//! 作文批改 (Essay) 类型处理器
//!
//! 处理作文批改特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{
    VfsCreateEssaySessionParams, VfsDatabase, VfsEssayRepo, VfsFolderItem, VfsFolderRepo,
};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{essay_to_dstu_node, session_to_dstu_node};
use super::super::types::{DstuCreateOptions, DstuNode};

/// 获取作文（支持 essay 和 essay_session）
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsEssayRepo::get_essay(vfs_db, id) {
        Ok(Some(essay)) => Ok(Some(essay_to_dstu_node(&essay))),
        Ok(None) => {
            // 再尝试 essay_sessions 表
            match VfsEssayRepo::get_session(vfs_db, id) {
                Ok(Some(session)) => Ok(Some(session_to_dstu_node(&session))),
                Ok(None) => Ok(None),
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_get: FAILED - get_session error, id={}, error={}",
                        id,
                        e
                    );
                    Err(DstuError::from(e.to_string()))
                }
            }
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_essay error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建作文会话
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    _path: &str,
) -> DstuResult<DstuNode> {
    let essay_type = options
        .metadata
        .as_ref()
        .and_then(|m| m.get("essayType"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let grade_level = options
        .metadata
        .as_ref()
        .and_then(|m| m.get("gradeLevel"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let session = match VfsEssayRepo::create_session(
        vfs_db,
        VfsCreateEssaySessionParams {
            title: options.name.clone(),
            essay_type,
            grade_level,
            custom_prompt: options.content.clone(),
        },
    ) {
        Ok(s) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=essay_session, id={}",
                s.id
            );
            s
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=essay, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = options.resolved_folder_id() {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "essay".to_string(),
            session.id.clone(),
        );
        let _ = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item);
    }

    Ok(session_to_dstu_node(&session))
}

/// 复制作文会话
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let session = match VfsEssayRepo::get_session(vfs_db, src_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - essay session not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_session error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let new_session = match VfsEssayRepo::create_session(
        vfs_db,
        VfsCreateEssaySessionParams {
            title: format!("{} (副本)", session.title),
            essay_type: session.essay_type.clone(),
            grade_level: session.grade_level.clone(),
            custom_prompt: session.custom_prompt.clone(),
        },
    ) {
        Ok(s) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created essay session copy, id={}",
                s.id
            );
            s
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_session error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = dest_folder_id {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "essay".to_string(),
            new_session.id.clone(),
        );
        if let Err(e) = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] dstu_copy: failed to add essay to folder {}: {}",
                folder_id,
                e
            );
        }
    }

    Ok(session_to_dstu_node(&new_session))
}

/// 设置作文收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsEssayRepo::update_session(vfs_db, id, None, Some(favorite), None, None, None) {
        Ok(_) => {
            log::info!(
                "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=essay_session, id={}, favorite={}",
                id,
                favorite
            );
            match VfsEssayRepo::get_session(vfs_db, id) {
                Ok(Some(session)) => return Ok(session_to_dstu_node(&session)),
                Ok(None) => {
                    log::warn!(
                        "[DSTU::handlers] dstu_set_favorite: session not found, trying essay, id={}",
                        id
                    );
                    match VfsEssayRepo::get_essay(vfs_db, id) {
                        Ok(Some(essay)) => return Ok(essay_to_dstu_node(&essay)),
                        Ok(None) => {
                            log::warn!(
                                "[DSTU::handlers] dstu_set_favorite: FAILED - essay not found, id={}",
                                id
                            );
                            return Err(DstuError::from("操作失败".to_string()));
                        }
                        Err(e) => {
                            log::error!(
                                "[DSTU::handlers] dstu_set_favorite: FAILED - get_essay error, id={}, error={}",
                                id,
                                e
                            );
                            return Err(DstuError::from(e.to_string()));
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_set_favorite: FAILED - get_session error, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            }
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=essay, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 恢复已删除的作文
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    if id.starts_with("essay_session_") {
        match VfsEssayRepo::get_session(vfs_db, id) {
            Ok(Some(s)) => Ok(Some(session_to_dstu_node(&s))),
            Ok(None) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: essay_session not found after restore, id={}",
                    id
                );
                Ok(None)
            }
            Err(e) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: get_session error, id={}, error={}",
                    id,
                    e
                );
                Ok(None)
            }
        }
    } else {
        match VfsEssayRepo::get_essay(vfs_db, id) {
            Ok(Some(e)) => Ok(Some(essay_to_dstu_node(&e))),
            Ok(None) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: essay not found after restore, id={}",
                    id
                );
                Ok(None)
            }
            Err(e) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: get_essay error, id={}, error={}",
                    id,
                    e
                );
                Ok(None)
            }
        }
    }
}

/// 根据路径获取作文
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsEssayRepo::get_session(vfs_db, resource_id) {
        Ok(Some(session)) => Ok(Some(session_to_dstu_node(&session))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
