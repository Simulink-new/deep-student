//! 题目集 (Exam) 类型处理器
//!
//! 处理题目集特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{
    VfsCreateExamSheetParams, VfsDatabase, VfsExamRepo, VfsFolderItem, VfsFolderRepo,
};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{
    exam_to_dstu_node, parse_timestamp,
};
use super::super::types::{DstuCreateOptions, DstuNode, DstuNodeType};

/// 获取题目集
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsExamRepo::get_exam_sheet(vfs_db, id) {
        Ok(Some(exam)) => Ok(Some(exam_to_dstu_node(&exam))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_exam_sheet error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建题目集
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    _path: &str,
) -> DstuResult<DstuNode> {
    use nanoid;

    let temp_id = format!("exam_{}", nanoid::nanoid!(10));

    let exam = match VfsExamRepo::create_exam_sheet(
        vfs_db,
        VfsCreateExamSheetParams {
            exam_name: Some(options.name.clone()),
            temp_id,
            metadata_json: options.metadata.clone().unwrap_or(serde_json::Value::Null),
            preview_json: options.content.clone().map(|c| serde_json::Value::String(c)).unwrap_or(serde_json::Value::Null),
            status: "active".to_string(),
            folder_id: options.resolved_folder_id(),
        },
    ) {
        Ok(e) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=exam, id={}",
                e.id
            );
            e
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=exam, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 将题目集添加到文件夹
    if let Some(ref folder_id) = options.resolved_folder_id() {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "exam".to_string(),
            exam.id.clone(),
        );
        let _ = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item);
    }

    Ok(exam_to_dstu_node(&exam))
}

/// 删除题目集（软删除）
pub async fn handle_delete(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<()> {
    match VfsExamRepo::delete_exam_sheet(vfs_db, id) {
        Ok(_) => {
            log::info!(
                "[DSTU::handlers] dstu_delete: SUCCESS - type=exam, id={}",
                id
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_delete: FAILED - type=exam, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 重命名题目集
pub async fn handle_rename(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    new_name: &str,
) -> DstuResult<DstuNode> {
    let updated_exam = match VfsExamRepo::update_exam_name(vfs_db, id, new_name) {
        Ok(e) => {
            log::info!(
                "[DSTU::handlers] dstu_rename: SUCCESS - type=exam, id={}",
                id
            );
            e
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_rename: FAILED - type=exam, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(exam_to_dstu_node(&updated_exam))
}

/// 复制题目集
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let exam = match VfsExamRepo::get_exam_sheet(vfs_db, src_id) {
        Ok(Some(e)) => e,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - exam not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_exam_sheet error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    use nanoid;
    let new_temp_id = format!("copy_{}", nanoid::nanoid!(10));

    let new_exam = match VfsExamRepo::create_exam_sheet(
        vfs_db,
        VfsCreateExamSheetParams {
            exam_name: exam.exam_name.clone(),
            temp_id: new_temp_id,
            metadata_json: exam.metadata_json.clone(),
            preview_json: exam.preview_json.clone(),
            status: exam.status.clone(),
            folder_id: dest_folder_id.clone(),
        },
    ) {
        Ok(e) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created exam copy, id={}",
                e.id
            );
            e
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_exam_sheet error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = dest_folder_id {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "exam".to_string(),
            new_exam.id.clone(),
        );
        if let Err(e) = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] dstu_copy: failed to add exam to folder {}: {}",
                folder_id,
                e
            );
        }
    }

    Ok(exam_to_dstu_node(&new_exam))
}

/// 设置题目集收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsExamRepo::set_favorite(vfs_db, id, favorite) {
        Ok(_) => log::info!(
            "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=exam, id={}, favorite={}",
            id,
            favorite
        ),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=exam, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    }

    let exam = match VfsExamRepo::get_exam_sheet(vfs_db, id) {
        Ok(Some(e)) => e,
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - exam not found after set_favorite, id={}",
                id
            );
            return Err(DstuError::from("操作失败".to_string()));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - get_exam_sheet error, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(exam_to_dstu_node(&exam))
}

/// 恢复已删除的题目集
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsExamRepo::get_exam_sheet(vfs_db, id) {
        Ok(Some(e)) => Ok(Some(exam_to_dstu_node(&e))),
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: exam not found after restore, id={}",
                id
            );
            Ok(None)
        }
        Err(e) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: get_exam_sheet error, id={}, error={}",
                id,
                e
            );
            Ok(None)
        }
    }
}

/// 列出已删除的题目集
pub fn handle_list_deleted(
    vfs_db: &Arc<VfsDatabase>,
    limit: u32,
    offset: u32,
) -> DstuResult<Vec<DstuNode>> {
    let exams = match VfsExamRepo::list_deleted_exams(vfs_db, limit, offset) {
        Ok(e) => e,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_list_deleted: FAILED - list_deleted_exam_sheets error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let nodes: Vec<DstuNode> = exams
        .into_iter()
        .map(|e| {
            let path = format!("/{}", e.id);
            let created_at = parse_timestamp(&e.created_at);
            let updated_at = parse_timestamp(&e.updated_at);

            DstuNode {
                id: e.id.clone(),
                source_id: e.id.clone(),
                name: e.exam_name.unwrap_or_else(|| "未命名题目集".to_string()),
                path,
                node_type: DstuNodeType::Exam,
                size: None,
                created_at,
                updated_at,
                children: None,
                child_count: None,
                resource_id: e.resource_id,
                resource_hash: None,
                preview_type: Some("exam".to_string()),
                metadata: Some(serde_json::json!({
                    "is_favorite": e.is_favorite,
                })),
            }
        })
        .collect();

    Ok(nodes)
}

/// 根据路径获取题目集
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsExamRepo::get_exam_sheet(vfs_db, resource_id) {
        Ok(Some(exam)) => Ok(Some(exam_to_dstu_node(&exam))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
