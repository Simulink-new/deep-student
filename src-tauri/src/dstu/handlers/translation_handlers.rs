//! 翻译 (Translation) 类型处理器
//!
//! 处理翻译特有的 DSTU 操作逻辑

use std::sync::Arc;

use crate::vfs::{VfsDatabase, VfsTranslationRepo, VfsCreateTranslationParams, VfsFolderItem, VfsFolderRepo};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{
    parse_timestamp, translation_to_dstu_node,
};
use super::super::types::{DstuCreateOptions, DstuNode, DstuNodeType};

/// 获取翻译
pub async fn handle_get(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTranslationRepo::get_translation(vfs_db, id) {
        Ok(Some(translation)) => Ok(Some(translation_to_dstu_node(&translation))),
        Ok(None) => Ok(None),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - get_translation error, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 创建翻译
pub async fn handle_create(
    vfs_db: &Arc<VfsDatabase>,
    options: &DstuCreateOptions,
    _path: &str,
) -> DstuResult<DstuNode> {
    // 解析元数据获取翻译属性
    let src_lang = options
        .metadata
        .as_ref()
        .and_then(|m| m.get("sourceLanguage"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let tgt_lang = options
        .metadata
        .as_ref()
        .and_then(|m| m.get("targetLanguage"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let translation = match VfsTranslationRepo::create_translation(
        vfs_db,
        VfsCreateTranslationParams {
            title: Some(options.name.clone()),
            source: String::new(),
            translated: options.content.clone().unwrap_or_default(),
            src_lang: src_lang.unwrap_or_else(|| "auto".to_string()),
            tgt_lang: tgt_lang.unwrap_or_else(|| "zh".to_string()),
            engine: None,
            model: None,
        },
    ) {
        Ok(t) => {
            log::info!(
                "[DSTU::handlers] dstu_create: SUCCESS - type=translation, id={}",
                t.id
            );
            t
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_create: FAILED - type=translation, error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 将翻译添加到文件夹
    if let Some(ref folder_id) = options.resolved_folder_id() {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "translation".to_string(),
            translation.id.clone(),
        );
        let _ = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item);
    }

    Ok(translation_to_dstu_node(&translation))
}

/// 删除翻译（软删除）
pub async fn handle_delete(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<()> {
    match VfsTranslationRepo::delete_translation(vfs_db, id) {
        Ok(_) => {
            log::info!(
                "[DSTU::handlers] dstu_delete: SUCCESS - type=translation, id={}",
                id
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_delete: FAILED - type=translation, id={}, error={}",
                id,
                e
            );
            Err(DstuError::from(e.to_string()))
        }
    }
}

/// 复制翻译
pub async fn handle_copy(
    vfs_db: &Arc<VfsDatabase>,
    src_id: &str,
    dest_folder_id: &Option<String>,
) -> DstuResult<DstuNode> {
    let translation = match VfsTranslationRepo::get_translation(vfs_db, src_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - translation not found, id={}",
                src_id
            );
            return Err(DstuError::not_found(src_id));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_translation error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 获取翻译内容
    let content = match VfsTranslationRepo::get_translation_content(vfs_db, src_id) {
        Ok(Some(c)) => c,
        Ok(None) => String::from(r#"{"source":"","translated":""}"#),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - get_translation_content error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    use serde_json::Value;
    let content_json: Value = serde_json::from_str(&content)
        .unwrap_or_else(|_| serde_json::json!({"source": "", "translated": ""}));
    let source = content_json
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let translated = content_json
        .get("translated")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let new_translation = match VfsTranslationRepo::create_translation(
        vfs_db,
        VfsCreateTranslationParams {
            title: translation.title.clone(),
            source,
            translated,
            src_lang: translation.src_lang.clone(),
            tgt_lang: translation.tgt_lang.clone(),
            engine: translation.engine.clone(),
            model: translation.model.clone(),
        },
    ) {
        Ok(t) => {
            log::info!(
                "[DSTU::handlers] dstu_copy: SUCCESS - created translation copy, id={}",
                t.id
            );
            t
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - create_translation error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Some(ref folder_id) = dest_folder_id {
        let folder_item = VfsFolderItem::new(
            Some(folder_id.clone()),
            "translation".to_string(),
            new_translation.id.clone(),
        );
        if let Err(e) = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] dstu_copy: failed to add translation to folder {}: {}",
                folder_id,
                e
            );
        }
    }

    Ok(translation_to_dstu_node(&new_translation))
}

/// 设置翻译收藏状态
pub async fn handle_set_favorite(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
    favorite: bool,
) -> DstuResult<DstuNode> {
    match VfsTranslationRepo::set_favorite(vfs_db, id, favorite) {
        Ok(_) => log::info!(
            "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=translation, id={}, favorite={}",
            id,
            favorite
        ),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - type=translation, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    }

    let translation = match VfsTranslationRepo::get_translation(vfs_db, id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - translation not found after set_favorite, id={}",
                id
            );
            return Err(DstuError::from("操作失败".to_string()));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - get_translation error, id={}, error={}",
                id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    Ok(translation_to_dstu_node(&translation))
}

/// 恢复已删除的翻译
pub async fn handle_restore(
    vfs_db: &Arc<VfsDatabase>,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTranslationRepo::get_translation(vfs_db, id) {
        Ok(Some(t)) => Ok(Some(translation_to_dstu_node(&t))),
        Ok(None) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: translation not found after restore, id={}",
                id
            );
            Ok(None)
        }
        Err(e) => {
            log::warn!(
                "[DSTU::handlers] dstu_restore: get_translation error, id={}, error={}",
                id,
                e
            );
            Ok(None)
        }
    }
}

/// 列出已删除的翻译
pub fn handle_list_deleted(
    vfs_db: &Arc<VfsDatabase>,
    limit: u32,
    offset: u32,
) -> DstuResult<Vec<DstuNode>> {
    let translations = match VfsTranslationRepo::list_deleted_translations(vfs_db, limit, offset) {
        Ok(t) => t,
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_list_deleted: FAILED - list_deleted_translations error={}",
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let nodes: Vec<DstuNode> = translations
        .into_iter()
        .map(|t| {
            let path = format!("/{}", t.id);
            let created_at = parse_timestamp(&t.created_at);
            let updated_at = t.updated_at.as_deref().map(parse_timestamp).unwrap_or(0);

            DstuNode {
                id: t.id.clone(),
                source_id: t.id.clone(),
                name: t.title.unwrap_or_else(|| "未命名翻译".to_string()),
                path,
                node_type: DstuNodeType::Translation,
                size: None,
                created_at,
                updated_at,
                children: None,
                child_count: None,
                resource_id: Some(t.resource_id),
                resource_hash: None,
                preview_type: Some("translation".to_string()),
                metadata: Some(serde_json::json!({
                    "is_favorite": t.is_favorite,
                })),
            }
        })
        .collect();

    Ok(nodes)
}

/// 根据路径获取翻译
pub async fn handle_get_by_path(
    vfs_db: &Arc<VfsDatabase>,
    resource_id: &str,
) -> DstuResult<Option<DstuNode>> {
    match VfsTranslationRepo::get_translation(vfs_db, resource_id) {
        Ok(Some(tr)) => Ok(Some(translation_to_dstu_node(&tr))),
        Ok(None) => Ok(None),
        Err(e) => Err(DstuError::from(e.to_string())),
    }
}
