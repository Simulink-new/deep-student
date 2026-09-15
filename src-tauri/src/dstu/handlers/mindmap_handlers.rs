//! 知识导图 (MindMap) 类型处理器
//!
//! 处理知识导图特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{
    repos::VfsMindMapRepo, VfsCreateMindMapParams, VfsDatabase, VfsFolderItem, VfsFolderRepo,
};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::mindmap_to_dstu_node;
use super::super::types::{DstuCreateOptions, DstuNode};

/// 获取知识导图
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsMindMapRepo::get_mindmap(vfs_db, id) {
        Ok(Some(mindmap)) => Ok(Some(mindmap_to_dstu_node(&mindmap))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_mindmap error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建知识导图
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    _path: &str,
) -> DstuResult<DstuNode> {
    let mindmap = match VfsMindMapRepo::create_mindmap(
        vfs_db,
        VfsCreateMindMapParams {
            title: options.name.clone(),
            description: None,
            content: options.content.clone().unwrap_or_default(),
            default_view: "".to_string(),
            theme: None,
        },
    ) {
        Ok(m) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=mindmap, id={}",
                m.id
            );
            m
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=mindmap, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = options.resolved_folder_id() {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "mindmap".to_string(),
            mindmap.id.clone(),
        );
        let _ = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item);
    }

    Ok(mindmap_to_dstu_node(&mindmap))
}

/// 复制知识导图
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let mindmap = match VfsMindMapRepo::get_mindmap(vfs_db, src_id) {
        Ok(Some(m)) => m,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - mindmap not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_mindmap error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let content = match VfsMindMapRepo::get_mindmap_content(vfs_db, src_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            r#"{"version":"1.0","root":{"id":"root","text":"根节点","children":[]}}"#.to_string()
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_mindmap_content error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let new_mindmap = match VfsMindMapRepo::create_mindmap_in_folder(
        vfs_db,
        VfsCreateMindMapParams {
            title: format!("{} (副本)", mindmap.title),
            description: mindmap.description.clone(),
            content,
            default_view: mindmap.default_view.clone(),
            theme: mindmap.theme.clone(),
        },
        dest_folder_id.as_deref(),
    ) {
        Ok(m) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created mindmap copy, id={}",
                m.id
            );
            m
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_mindmap error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(mindmap_to_dstu_node(&new_mindmap))
}

/// 设置知识导图收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsMindMapRepo::set_favorite(vfs_db, id, favorite) {
        Ok(_) => log::info!(
            "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=mindmap, id={}, favorite={}",
            id,
            favorite
        ),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=mindmap, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    }

    let mindmap = match VfsMindMapRepo::get_mindmap(vfs_db, id) {
        Ok(Some(m)) => m,
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - mindmap not found after set_favorite, id={}",
                id
            );
            return Err(DstuError::from("操作失败".to_string()));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - get_mindmap error, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(mindmap_to_dstu_node(&mindmap))
}

/// 恢复已删除的知识导图
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsMindMapRepo::get_mindmap(vfs_db, id) {
        Ok(Some(m)) => Ok(Some(mindmap_to_dstu_node(&m))),
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: mindmap not found after restore, id={}",
                id
            );
            Ok(None)
        }
        Err(e) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: get_mindmap error, id={}, error={}",
                id,
                e
            );
            Ok(None)
        }
    }
}

/// 根据路径获取知识导图
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsMindMapRepo::get_mindmap(vfs_db, resource_id) {
        Ok(Some(mindmap)) => Ok(Some(mindmap_to_dstu_node(&mindmap))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
