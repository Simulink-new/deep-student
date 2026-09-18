//! DSTU 通用处理器 -- 路由函数和共享工具
//!
//! 包含所有 Tauri 命令的路由逻辑，按资源类型分发到各处理器子模块。

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use tauri::{Manager, State, Window};

use super::super::error::{DstuError, DstuResult};
use super::super::handler_utils::{
    delete_resource_by_type, delete_resource_by_type_with_conn, emit_watch_event,
    extract_resource_info, fallback_lookup_uuid_resource, fetch_resource_as_dstu_node,
    get_content_by_type, get_resource_by_type_and_id, infer_resource_type_from_id, is_uuid_format,
    item_type_to_dstu_node_type, list_resources_by_type_with_folder_path, list_unassigned_essays,
    list_unassigned_exams, list_unassigned_files, list_unassigned_images,
    list_unassigned_notes, list_unassigned_textbooks, list_unassigned_translations,
    purge_resource_by_type, restore_resource_by_type,
    restore_resource_by_type_with_conn, update_content_by_type,
};
use super::super::path_parser::build_simple_resource_path;
use super::super::types::{
    BatchMoveRequest, BatchMoveResult, DstuCreateOptions, DstuListOptions, DstuNode, DstuNodeType,
    DstuParsedPath, DstuWatchEvent, FailedMoveItem, ResourceLocation,
};
use super::super::trash_handlers::is_resource_in_trash;
use super::{
    essay_handlers, exam_handlers, file_handlers, image_handlers, mindmap_handlers,
    note_handlers, textbook_handlers, translation_handlers,
};

use crate::vfs::{
    canonical_folder_item_type, repos::VfsMindMapRepo, VfsCreateEssaySessionParams,
    VfsCreateExamSheetParams, VfsCreateMindMapParams, VfsCreateNoteParams, VfsDatabase,
    VfsEssayRepo, VfsExamRepo, VfsFileRepo, VfsFolder, VfsFolderItem, VfsFolderRepo, VfsNoteRepo,
    VfsTextbookRepo, VfsTranslationRepo, VfsUpdateMindMapParams, VfsUpdateNoteParams,
};

use chrono;

// ============================================================================
// 记忆系统隐藏名称检测
// ============================================================================

/// 检测名称是否为记忆系统保留名称（以 `__` 开头且以 `__` 结尾）
pub fn is_memory_system_hidden_name(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.len() > 4 && trimmed.starts_with("__") && trimmed.ends_with("__")
}

/// 记录并跳过迭代中的错误，避免静默丢弃
pub fn log_and_skip_err<T, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("[DstuHandlers] Row parse error (skipped): {}", e);
            None
        }
    }
}

// ============================================================================
// 输入验证常量
// ============================================================================

/// 最大内容大小: 1MB
pub const MAX_CONTENT_SIZE: usize = 1 * 1024 * 1024;
/// 最大元数据大小: 64KB
pub const MAX_METADATA_SIZE: usize = 64 * 1024;
/// 最大名称长度: 256字符
pub const MAX_NAME_LENGTH: usize = 256;
/// 批量操作的最大数量限制
pub const MAX_BATCH_SIZE: usize = 100;

// ============================================================================
// Tauri 命令：列出目录内容
// ============================================================================

#[tauri::command]
pub async fn dstu_list(
    path: String,
    options: Option<DstuListOptions>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Vec<DstuNode>> {
    let options = options.unwrap_or_default();

    log::info!(
        "[DSTU::handlers] dstu_list: folder_id={:?}, type_filter={:?}, path={}",
        options.get_folder_id(),
        options.get_type_filter(),
        path
    );

    dstu_list_folder_first(&options, &vfs_db).await
}

async fn dstu_list_folder_first(
    options: &DstuListOptions,
    vfs_db: &Arc<VfsDatabase>,
) -> DstuResult<Vec<DstuNode>> {
    let mut results = Vec::new();

    let folder_id = options.folder_id.as_ref().map(|s| s.as_str());
    let is_root = folder_id.is_none()
        || folder_id == Some("")
        || folder_id == Some("root")
        || folder_id == Some("null");

    if let Some(ref fid) = options.folder_id {
        log::info!(
            "[DSTU::handlers] dstu_list_folder_first: listing folder {} (is_root={})",
            fid,
            is_root
        );
    }

    // ★ 优先处理收藏模式
    if let Some(true) = options.is_favorite {
        log::info!(
            "[DSTU::handlers] dstu_list_folder_first: favorite-only mode, loading all resources"
        );

        for node_type in &[
            DstuNodeType::Note,
            DstuNodeType::Textbook,
            DstuNodeType::Exam,
            DstuNodeType::Translation,
            DstuNodeType::Essay,
            DstuNodeType::Image,
            DstuNodeType::File,
            DstuNodeType::MindMap,
        ] {
            let type_results =
                list_resources_by_type_with_folder_path(vfs_db, *node_type, options).await?;
            results.extend(type_results);
        }

        results.retain(|node| {
            if let Some(metadata) = &node.metadata {
                metadata
                    .get("isFavorite")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    || metadata
                        .get("favorite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    || metadata
                        .get("is_favorite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
            } else {
                false
            }
        });
        results.retain(|node| !is_memory_system_hidden_name(&node.name));

        let sort_by = options.sort_by.as_deref().unwrap_or("updatedAt");
        let ascending = options
            .sort_order
            .as_deref()
            .map(|s| s == "asc")
            .unwrap_or(false);
        results.sort_by(|a, b| {
            let cmp = match sort_by {
                "name" => a.name.cmp(&b.name),
                "createdAt" => a.created_at.cmp(&b.created_at),
                _ => a.updated_at.cmp(&b.updated_at),
            };
            if ascending {
                cmp
            } else {
                cmp.reverse()
            }
        });
        return Ok(results);
    }

    // ★ 优先处理 typeFilter（智能文件夹模式）
    if let Some(type_filter) = options.get_type_filter() {
        if is_root || options.folder_id.is_none() {
            log::info!(
                "[DSTU::handlers] dstu_list_folder_first: smart folder mode, type_filter={:?}",
                type_filter
            );
            let mut smart_results =
                list_resources_by_type_with_folder_path(vfs_db, type_filter, options).await?;
            smart_results.retain(|node| !is_memory_system_hidden_name(&node.name));
            return Ok(smart_results);
        }
    }

    if is_root {
        let _folder_id = "root";
        let root_folders = match VfsFolderRepo::list_folders_by_parent(vfs_db, None) {
            Ok(folders) => folders,
            Err(e) => return Err(DstuError::from(e.to_string())),
        };
        for folder in root_folders {
            results.push(DstuNode::folder(&folder.id, &folder.title, &folder.title));
        }

        let root_items = match VfsFolderRepo::list_items_by_folder(vfs_db, None) {
            Ok(items) => items,
            Err(e) => return Err(DstuError::from(e.to_string())),
        };

        for item in root_items {
            if let Some(node) = fetch_resource_as_dstu_node(
                vfs_db,
                &item,
                &item
                    .cached_path
                    .clone()
                    .unwrap_or_else(|| item.item_id.clone()),
            )
            .await?
            {
                results.push(node);
            }
        }

        let all_assigned_ids = match VfsFolderRepo::list_all_assigned_item_ids(vfs_db) {
            Ok(ids) => ids,
            Err(e) => return Err(DstuError::from(e.to_string())),
        };

        results.extend(list_unassigned_notes(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_textbooks(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_exams(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_translations(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_essays(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_files(vfs_db, &all_assigned_ids).await?);
        results.extend(list_unassigned_images(vfs_db, &all_assigned_ids).await?);

        return Ok(results);
    } else if let Some(ref actual_folder_id) = options.folder_id {
        let _folder = match VfsFolderRepo::get_folder(vfs_db, actual_folder_id) {
            Ok(Some(f)) => f,
            Ok(None) => {
                log::warn!(
                    "[DSTU::handlers] dstu_list: folder not found: {}",
                    actual_folder_id
                );
                return Err(DstuError::from("文件夹不存在".to_string()));
            }
            Err(e) => return Err(DstuError::from(e.to_string())),
        };

        let folder_path = VfsFolderRepo::build_folder_path(vfs_db, actual_folder_id)
            .map_err(|e| e.to_string())?;

        let sub_folders =
            VfsFolderRepo::list_folders_by_parent(vfs_db, Some(actual_folder_id))
                .map_err(|e| e.to_string())?;
        for sub_folder in sub_folders {
            if is_memory_system_hidden_name(&sub_folder.title) {
                continue;
            }
            let sub_path = format!("{}/{}", folder_path, sub_folder.title);
            results.push(DstuNode::folder(
                &sub_folder.id,
                &sub_path,
                &sub_folder.title,
            ));
        }

        let items = VfsFolderRepo::list_items_by_folder(vfs_db, Some(actual_folder_id))
            .map_err(|e| e.to_string())?;

        for item in items {
            if let Some(type_filter) = options.get_type_filter() {
                if let Some(node_type) = item_type_to_dstu_node_type(&item.item_type) {
                    // ★★ 修复：file 类型物品可能是 PDF/DOCX/图片等，实际 node_type
                    // 由 file_to_dstu_node 根据 MIME 动态决定。对 file 类型跳过类型筛选，
                    // 由后续 fetch_resource_as_dstu_node 确定正确的节点类型
                    if node_type != type_filter && item.item_type != "file" && item.item_type != "image" {
                        continue;
                    }
                }
            }

            let resource_path = item
                .cached_path
                .clone()
                .unwrap_or_else(|| format!("{}/{}", folder_path, &item.item_id));

            if let Some(node) = fetch_resource_as_dstu_node(vfs_db, &item, &resource_path).await? {
                if is_memory_system_hidden_name(&node.name) {
                    continue;
                }
                results.push(node);
            }
        }
    } else if let Some(type_filter) = options.get_type_filter() {
        results = list_resources_by_type_with_folder_path(vfs_db, type_filter, options).await?;
    }

    results.retain(|node| !is_memory_system_hidden_name(&node.name));

    let sort_by = options.sort_by.as_deref().unwrap_or("updatedAt");
    let ascending = options
        .sort_order
        .as_deref()
        .map(|s| s == "asc")
        .unwrap_or(false);

    results.sort_by(|a, b| {
        let cmp = match sort_by {
            "name" => a.name.cmp(&b.name),
            "createdAt" => a.created_at.cmp(&b.created_at),
            _ => a.updated_at.cmp(&b.updated_at),
        };
        if ascending {
            cmp
        } else {
            cmp.reverse()
        }
    });

    let offset = options.get_offset() as usize;
    let limit = options.get_limit() as usize;
    if offset > 0 {
        results = results.into_iter().skip(offset).collect();
    }
    if results.len() > limit {
        results.truncate(limit);
    }

    Ok(results)
}

// ============================================================================
// Tauri 命令：资源获取
// ============================================================================

#[tauri::command]
pub async fn dstu_get(
    path: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Option<DstuNode>> {
    log::info!("[DSTU::handlers] dstu_get: path={}", path);

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let node = get_node_by_type_and_id(&vfs_db, &resource_type, &id).await?;

    if node.is_some() {
        log::info!(
            "[DSTU::handlers] dstu_get: SUCCESS - type={}, id={}",
            resource_type,
            id
        );
    } else {
        log::warn!(
            "[DSTU::handlers] dstu_get: NOT FOUND - type={}, id={}",
            resource_type,
            id
        );
    }

    Ok(node)
}

/// dstu_get 的类型分发核心（dstu_get 与 dstu_get_batch 共用）
async fn get_node_by_type_and_id(
    vfs_db: &Arc<VfsDatabase>,
    resource_type: &str,
    id: &str,
) -> DstuResult<Option<DstuNode>> {
    let node = match resource_type {
        "notes" => note_handlers::handle_get(vfs_db, id).await?,
        "textbooks" => textbook_handlers::handle_get(vfs_db, id).await?,
        "exams" => exam_handlers::handle_get(vfs_db, id).await?,
        "translations" => translation_handlers::handle_get(vfs_db, id).await?,
        "essays" => essay_handlers::handle_get(vfs_db, id).await?,
        "folders" => {
            match crate::vfs::VfsFolderRepo::get_folder(vfs_db, id) {
                Ok(Some(folder)) => {
                    let folder_path = build_simple_resource_path(&folder.id);
                    Some(DstuNode::folder(&folder.id, &folder_path, &folder.title))
                }
                Ok(None) => {
                    if is_uuid_format(id) {
                        log::info!("[DSTU::handlers] dstu_get: folder not found for UUID, trying fallback lookup, id={}", id);
                        fallback_lookup_uuid_resource(vfs_db, id)
                    } else {
                        None
                    }
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_get: FAILED - get_folder error, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            }
        }
        "mindmaps" => mindmap_handlers::handle_get(vfs_db, id).await?,
        "files" | "images" => file_handlers::handle_get(vfs_db, id).await?,
        _ => {
            log::warn!(
                "[DSTU::handlers] dstu_get: unsupported type={}",
                resource_type
            );
            None
        }
    };
    Ok(node)
}

/// 批量获取（perf-audit task-029/B2）: 消灭最近视图 N+1——
/// 旧路径最近 50 项×含重试最坏 ~100 次 dstu_get invoke,现 1 次批量 + 可选 1 次重试批
#[tauri::command]
pub async fn dstu_get_batch(
    paths: Vec<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Vec<Option<DstuNode>>> {
    log::debug!("[DSTU::handlers] dstu_get_batch: {} paths", paths.len());
    let mut out = Vec::with_capacity(paths.len());
    for path in &paths {
        let node = match extract_resource_info(path) {
            Ok((resource_type, id)) => match get_node_by_type_and_id(&vfs_db, &resource_type, &id).await {
                Ok(n) => n,
                Err(e) => {
                    // 单项失败不拖垮整批（镜像 dstu_get 单路径失败语义: 调用方按 None 处理）
                    log::warn!(
                        "[DSTU::handlers] dstu_get_batch: item failed - path={}, error={}",
                        path,
                        e
                    );
                    None
                }
            },
            Err(e) => {
                log::warn!(
                    "[DSTU::handlers] dstu_get_batch: invalid path - path={}, error={}",
                    path,
                    e
                );
                None
            }
        };
        out.push(node);
    }
    Ok(out)
}

// ============================================================================
// Tauri 命令：创建资源
// ============================================================================

#[tauri::command]
pub async fn dstu_create(
    app: tauri::AppHandle,
    path: String,
    options: DstuCreateOptions,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_create: path={}, type={:?}, name={}",
        path,
        options.node_type,
        options.name
    );

    // 输入验证
    if options.name.len() > MAX_NAME_LENGTH {
        let error_msg = format!(
            "名称长度超出限制: {} 字符 (最大允许: {} 字符)",
            options.name.len(),
            MAX_NAME_LENGTH
        );
        log::error!("[DSTU::handlers] dstu_create: FAILED - {}", error_msg);
        return Err(DstuError::from(error_msg));
    }

    if let Some(ref content) = options.content {
        let content_bytes = content.len();
        if content_bytes > MAX_CONTENT_SIZE {
            let error_msg = format!(
                "内容大小超出限制: {} 字节 ({:.2} MB) (最大允许: {} 字节 ({} MB))",
                content_bytes,
                content_bytes as f64 / (1024.0 * 1024.0),
                MAX_CONTENT_SIZE,
                MAX_CONTENT_SIZE / (1024 * 1024)
            );
            log::error!("[DSTU::handlers] dstu_create: FAILED - {}", error_msg);
            return Err(DstuError::from(error_msg));
        }
    }

    if let Some(ref metadata) = options.metadata {
        let metadata_json = match serde_json::to_string(metadata) {
            Ok(json) => json,
            Err(e) => {
                let error_msg = format!("元数据序列化失败: {}", e);
                log::error!("[DSTU::handlers] dstu_create: FAILED - {}", error_msg);
                return Err(DstuError::from(error_msg));
            }
        };
        let metadata_bytes = metadata_json.len();
        if metadata_bytes > MAX_METADATA_SIZE {
            let error_msg = format!(
                "元数据大小超出限制: {} 字节 ({:.2} KB) (最大允许: {} 字节 ({} KB))",
                metadata_bytes,
                metadata_bytes as f64 / 1024.0,
                MAX_METADATA_SIZE,
                MAX_METADATA_SIZE / 1024
            );
            log::error!("[DSTU::handlers] dstu_create: FAILED - {}", error_msg);
            return Err(DstuError::from(error_msg));
        }
    }

    let resource_type = options.node_type.to_path_segment();

    let node = match resource_type {
        "notes" => {
            note_handlers::handle_create(&vfs_db, &window, &options, &path, "notes").await?
        }
        "textbooks" => {
            textbook_handlers::handle_create(&vfs_db, &options, options.resolved_folder_id()).await?
        }
        "exams" => exam_handlers::handle_create(&vfs_db, &options, &path).await?,
        "translations" => {
            translation_handlers::handle_create(&vfs_db, &options, &path).await?
        }
        "essays" => essay_handlers::handle_create(&vfs_db, &options, &path).await?,
        "mindmaps" => mindmap_handlers::handle_create(&vfs_db, &options, &path).await?,
        "images" => {
            image_handlers::handle_create(&vfs_db, &options, &path, options.resolved_folder_id()).await?
        }
        "files" => {
            file_handlers::handle_create(&vfs_db, &options, &path, options.resolved_folder_id()).await?
        }
        "cards" => {
            // ★ 2026-08-07 迁移补充：DstuNodeType 收编为 ResourceKind 后新增 Card 变体。
            // 卡片不支持通过 DSTU 创建（由卡组/复习模块管理），显式返回 NotSupported
            return Err(DstuError::NotSupported(
                "卡片资源不支持通过 DSTU 创建".to_string(),
            ));
        }
        "folders" => {
            // 创建文件夹
            let new_folder = VfsFolder::new(
                options.name.clone(),
                None,
                None,
                None,
            );
            match VfsFolderRepo::create_folder(&vfs_db, &new_folder) {
                Ok(_) => {
                    log::info!(
                        "[DSTU::handlers] dstu_create: SUCCESS - type=folder, id={}",
                        new_folder.id
                    );
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_create: FAILED - type=folder, error={}",
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            DstuNode::folder(&new_folder.id, &format!("/{}", new_folder.id), &new_folder.title)
        }
        _ => {
            return Err(DstuError::invalid_node_type(resource_type));
        }
    };

    emit_watch_event(&window, DstuWatchEvent::created(&node.path, node.clone()));
    log::info!("[DSTU::handlers] dstu_create: created {}", node.path);
    Ok(node)
}

// ============================================================================
// Tauri 命令：更新资源内容
// ============================================================================

#[tauri::command]
pub async fn dstu_update(
    app: tauri::AppHandle,
    path: String,
    content: String,
    resource_type: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_update: path={}, type={}, content_len={}",
        path,
        resource_type,
        content.len()
    );

    let content_bytes = content.len();
    if content_bytes > MAX_CONTENT_SIZE {
        let error_msg = format!(
            "内容大小超出限制: {} 字节 ({:.2} MB) (最大允许: {} 字节 ({} MB))",
            content_bytes,
            content_bytes as f64 / (1024.0 * 1024.0),
            MAX_CONTENT_SIZE,
            MAX_CONTENT_SIZE / (1024 * 1024)
        );
        log::error!("[DSTU::handlers] dstu_update: FAILED - {}", error_msg);
        return Err(DstuError::from(error_msg));
    }

    log::info!(
        "[DSTU::handlers] dstu_update: 输入验证通过 - content_size={}",
        content_bytes
    );

    let id = path.trim_start_matches('/').to_string();
    if id.is_empty() {
        log::error!("[DSTU::handlers] dstu_update: FAILED - empty path");
        return Err(DstuError::invalid_path("Update path must contain resource ID"));
    }

    let node = match resource_type.as_str() {
        "notes" | "note" => note_handlers::handle_update(&vfs_db, &id, &content).await?,
        "exams" | "exam" => {
            // 使用 update_content_by_type 更新题目集内容（支持 JSON preview）
            if let Err(e) = update_content_by_type(&vfs_db, "exam", &id, &content) {
                log::error!(
                    "[DSTU::handlers] dstu_update: FAILED - type=exam, id={}, error={}",
                    id,
                    e
                );
                return Err(DstuError::from(e));
            }
            log::info!(
                "[DSTU::handlers] dstu_update: SUCCESS - type=exam, id={}",
                id
            );
            let updated_exam = match VfsExamRepo::get_exam_sheet(&vfs_db, &id) {
                Ok(Some(e)) => e,
                Ok(None) => return Err(DstuError::not_found(&id)),
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_update: FAILED - type=exam, id={}, get_exam_sheet error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            crate::dstu::handler_utils::exam_to_dstu_node(&updated_exam)
        }
        "translations" | "translation" => {
            // 使用 update_content_by_type 更新翻译内容（JSON source/translated）
            if let Err(e) = update_content_by_type(&vfs_db, "translation", &id, &content) {
                log::error!(
                    "[DSTU::handlers] dstu_update: FAILED - type=translation, id={}, error={}",
                    id,
                    e
                );
                return Err(DstuError::from(e));
            }
            log::info!(
                "[DSTU::handlers] dstu_update: SUCCESS - type=translation, id={}",
                id
            );
            let updated_translation = match VfsTranslationRepo::get_translation(&vfs_db, &id) {
                Ok(Some(t)) => t,
                Ok(None) => return Err(DstuError::not_found(&id)),
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_update: FAILED - type=translation, id={}, get_translation error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            crate::dstu::handler_utils::translation_to_dstu_node(&updated_translation)
        }
        "essays" | "essay" => {
            // update_session 返回 VfsResult<()>, 需要再查询获取更新后的会话
            if let Err(e) = VfsEssayRepo::update_session(
                &vfs_db,
                &id,
                None,
                None,
                Some(&content),
                None,
                None,
            ) {
                log::error!(
                    "[DSTU::handlers] dstu_update: FAILED - type=essay, id={}, error={}",
                    id,
                    e
                );
                return Err(DstuError::from(e.to_string()));
            }
            log::info!(
                "[DSTU::handlers] dstu_update: SUCCESS - type=essay, id={}",
                id
            );
            let updated_session = match VfsEssayRepo::get_session(&vfs_db, &id) {
                Ok(Some(s)) => s,
                Ok(None) => return Err(DstuError::not_found(&id)),
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_update: FAILED - type=essay, id={}, get_session error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            crate::dstu::handler_utils::session_to_dstu_node(&updated_session)
        }
        "mindmaps" | "mindmap" => {
            let updated_mindmap = match VfsMindMapRepo::update_mindmap(
                &vfs_db,
                &id,
                VfsUpdateMindMapParams {
                    title: None,
                    description: None,
                    content: Some(content),
                    default_view: None,
                    theme: None,
                    settings: None,
                    expected_updated_at: None,
                    version_source: Some("manual".to_string()),
                },
            ) {
                Ok(m) => {
                    log::info!(
                        "[DSTU::handlers] dstu_update: SUCCESS - type=mindmap, id={}",
                        id
                    );
                    m
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_update: FAILED - type=mindmap, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            crate::dstu::handler_utils::mindmap_to_dstu_node(&updated_mindmap)
        }
        _ => {
            log::error!(
                "[DSTU::handlers] dstu_update: FAILED - unsupported type={}",
                resource_type
            );
            return Err(DstuError::invalid_node_type(&resource_type));
        }
    };

    emit_watch_event(&window, DstuWatchEvent::updated(&path, node.clone()));
    log::info!("[DSTU::handlers] dstu_update: updated {}", path);
    Ok(node)
}

// ============================================================================
// Tauri 命令：删除资源
// ============================================================================

#[tauri::command]
pub async fn dstu_delete(
    app: tauri::AppHandle,
    path: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_delete: path={}", path);

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_delete: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    // 检查资源是否在回收站中
    let in_trash = is_resource_in_trash(&vfs_db, &resource_type, &id);
    if in_trash {
        log::error!(
            "[DSTU::handlers] dstu_delete: FAILED - resource already in trash, type={}, id={}",
            resource_type,
            id
        );
        return Err(DstuError::from("Resource already in trash".to_string()));
    }

    delete_resource_by_type(&vfs_db, &resource_type, &id)?;

    emit_watch_event(&window, DstuWatchEvent::deleted(&path));

    log::info!(
        "[DSTU::handlers] dstu_delete: deleted {} -> trash",
        path
    );
    Ok(())
}

// ============================================================================
// Tauri 命令：移动资源
// ============================================================================

#[tauri::command]
pub async fn dstu_move(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_move: src={}, dst={}", src, dst);

    let (src_type, src_id) = match extract_resource_info(&src) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_move: FAILED - src={}, error={}",
                src,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };
    let resource_type = src_type;

    let item_type = match resource_type.as_str() {
        "notes" => "note",
        "textbooks" => "textbook",
        "exams" => "exam",
        "translations" => "translation",
        "essays" => "essay",
        "folders" => "folder",
        "mindmaps" => "mindmap",
        "files" | "images" | "attachments" => "file",
        _ => {
            return Err(DstuError::invalid_node_type(resource_type));
        }
    };

    let dest_folder_id = if dst.trim().is_empty() || dst.trim() == "/" {
        None
    } else {
        let (dst_type, dst_id) = match extract_resource_info(&dst) {
            Ok((rt, rid)) => (rt, rid),
            Err(e) => {
                log::error!(
                    "[DSTU::handlers] dstu_move: FAILED - dst={}, error={}",
                    dst,
                    e
                );
                return Err(DstuError::from(e.to_string()));
            }
        };
        if dst_type != "folders" {
            return Err(DstuError::from("Destination must be a folder".to_string()));
        }
        Some(dst_id)
    };

    if let Err(e) =
        VfsFolderRepo::move_item_to_folder(&vfs_db, item_type, &src_id, dest_folder_id.as_deref())
    {
        log::error!(
            "[DSTU::handlers] dstu_move: FAILED - type={}, id={}, error={}",
            item_type,
            src_id,
            e
        );
        return Err(DstuError::from(e.to_string()));
    }

    let node = match get_resource_by_type_and_id(&vfs_db, &resource_type, &src_id).await {
        Ok(Some(n)) => n,
        Ok(None) => {
            log::error!(
                "[DSTU::handlers] dstu_move: FAILED - resource not found after move, id={}",
                src_id
            );
            return Err(DstuError::not_found(&src));
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_move: FAILED - get_resource error, id={}, error={}",
                src_id,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    emit_watch_event(
        &window,
        DstuWatchEvent::moved(&src, &node.path, node.clone()),
    );

    log::info!("[DSTU::handlers] dstu_move: moved {} to {}", src, node.path);
    Ok(node)
}

// ============================================================================
// Tauri 命令：重命名资源
// ============================================================================

#[tauri::command]
pub async fn dstu_rename(
    app: tauri::AppHandle,
    path: String,
    new_name: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_rename: path={}, new_name={}",
        path,
        new_name
    );

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_rename: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let node = match resource_type.as_str() {
        "notes" => note_handlers::handle_rename(&vfs_db, &id, &new_name).await?,
        "exams" => exam_handlers::handle_rename(&vfs_db, &id, &new_name).await?,
        "essays" => {
            VfsEssayRepo::update_session(
                &vfs_db,
                &id,
                Some(&new_name),
                None,
                None,
                None,
                None,
            )
            .map_err(|e| {
                log::error!(
                    "[DSTU::handlers] dstu_rename: FAILED - type=essay, id={}, error={}",
                    id,
                    e
                );
                DstuError::from(e.to_string())
            })?;
            log::info!(
                "[DSTU::handlers] dstu_rename: SUCCESS - type=essay, id={}",
                id
            );
            let session = VfsEssayRepo::get_session(&vfs_db, &id)
                .map_err(|e| DstuError::from(e.to_string()))?
                .ok_or_else(|| DstuError::from(format!("Essay session not found after rename: {}", id)))?;
            crate::dstu::handler_utils::session_to_dstu_node(&session)
        }
        "textbooks" => textbook_handlers::handle_rename(&vfs_db, &id, &new_name).await?,
        "translations" => {
            match VfsTranslationRepo::update_title(&vfs_db, &id, &new_name) {
                Ok(t) => {
                    log::info!(
                        "[DSTU::handlers] dstu_rename: SUCCESS - type=translation, id={}",
                        id
                    );
                    crate::dstu::handler_utils::translation_to_dstu_node(&t)
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=translation, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            }
        }
        "mindmaps" => {
            let updated_mindmap = match VfsMindMapRepo::update_mindmap(
                &vfs_db,
                &id,
                VfsUpdateMindMapParams {
                    title: Some(new_name.clone()),
                    description: None,
                    content: None,
                    default_view: None,
                    theme: None,
                    settings: None,
                    expected_updated_at: None,
                    version_source: Some("manual".to_string()),
                },
            ) {
                Ok(m) => {
                    log::info!(
                        "[DSTU::handlers] dstu_rename: SUCCESS - type=mindmap, id={}",
                        id
                    );
                    m
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=mindmap, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            crate::dstu::handler_utils::mindmap_to_dstu_node(&updated_mindmap)
        }
        "folders" => {
            // 获取文件夹
            let mut folder = match VfsFolderRepo::get_folder(&vfs_db, &id) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=folder, id={}, not found",
                        id
                    );
                    return Err(DstuError::not_found(&id));
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=folder, id={}, get_folder error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            // 更新标题
            folder.title = new_name.clone();
            // 更新数据库
            match VfsFolderRepo::update_folder(&vfs_db, &folder) {
                Ok(_) => {
                    log::info!(
                        "[DSTU::handlers] dstu_rename: SUCCESS - type=folder, id={}",
                        id
                    );
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=folder, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            let folder_path = format!("/{}", folder.id);
            DstuNode::folder(&folder.id, &folder_path, &folder.title)
        }
        "files" | "images" => {
            match VfsFileRepo::update_file_name(&vfs_db, &id, &new_name) {
                Ok(f) => {
                    log::info!(
                        "[DSTU::handlers] dstu_rename: SUCCESS - type=file, id={}",
                        id
                    );
                    crate::dstu::handler_utils::file_to_dstu_node(&f)
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_rename: FAILED - type=file, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            }
        }
        _ => {
            log::error!(
                "[DSTU::handlers] dstu_rename: FAILED - unsupported type={}",
                resource_type
            );
            return Err(DstuError::invalid_node_type(&resource_type));
        }
    };

    // 清除缓存的路径（直接更新 folder_items 表）
    if let Ok(conn) = vfs_db.get_conn_safe() {
        let canonical_type = crate::vfs::canonical_folder_item_type(&resource_type);
        let _ = conn.execute(
            "UPDATE folder_items SET cached_path = NULL WHERE item_type = ?1 AND item_id = ?2",
            rusqlite::params![canonical_type, &id],
        );
    }

    emit_watch_event(&window, DstuWatchEvent::updated(&path, node.clone()));

    log::info!(
        "[DSTU::handlers] dstu_rename: renamed {} to {} (cached_path cleared)",
        path,
        new_name
    );
    Ok(node)
}

// ============================================================================
// Tauri 命令：复制资源
// ============================================================================

#[tauri::command]
pub async fn dstu_copy(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_copy: src={}, dst={}", src, dst);

    let (src_resource_type, src_id) = match extract_resource_info(&src) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_copy: FAILED - src={}, error={}",
                src,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let dest_folder_id: Option<String> = if dst.trim().is_empty() || dst.trim() == "/" {
        None
    } else {
        let (dst_type, dst_id) = match extract_resource_info(&dst) {
            Ok((rt, rid)) => (rt, rid),
            Err(e) => {
                log::error!(
                    "[DSTU::handlers] dstu_copy: FAILED - invalid dst path, error={}",
                    e
                );
                return Err(DstuError::from(format!("Invalid destination path: {}", e)));
            }
        };
        if dst_type != "folders" {
            return Err(DstuError::from("Destination must be a folder".to_string()));
        }
        Some(dst_id)
    };

    let node = match src_resource_type.as_str() {
        "notes" => note_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?,
        "textbooks" => textbook_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?,
        "exams" => exam_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?,
        "translations" => {
            translation_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?
        }
        "essays" => essay_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?,
        "files" | "images" => {
            // files 和 images 共享 VfsFileRepo
            let file = match VfsFileRepo::get_file(&vfs_db, &src_id) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    log::error!(
                        "[DSTU::handlers] dstu_copy: FAILED - file not found, id={}",
                        src_id
                    );
                    return Err(DstuError::not_found(&src));
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_copy: FAILED - get_file error, id={}, error={}",
                        src_id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };

            let new_file_name = format!("{} (副本)", file.file_name);
            let new_sha256 = format!("{}_{}", file.sha256, chrono::Utc::now().timestamp_millis());

            let new_file = match VfsFileRepo::create_file(
                &vfs_db,
                &new_sha256,
                &new_file_name,
                file.size,
                &file.file_type,
                file.mime_type.as_deref(),
                file.blob_hash.as_deref(),
                file.original_path.as_deref(),
            ) {
                Ok(f) => {
                    log::info!(
                        "[DSTU::handlers] dstu_copy: SUCCESS - created file copy, id={}",
                        f.id
                    );
                    f
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_copy: FAILED - create_file error={}",
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };

            if let Some(ref folder_id) = dest_folder_id {
                let folder_item = VfsFolderItem::new(
                    Some(folder_id.clone()),
                    "file".to_string(),
                    new_file.id.clone(),
                );
                if let Err(e) = VfsFolderRepo::add_item_to_folder(&vfs_db, &folder_item) {
                    log::warn!(
                        "[DSTU::handlers] dstu_copy: failed to add file to folder {}: {}",
                        folder_id,
                        e
                    );
                }
            }

            crate::dstu::handler_utils::file_to_dstu_node(&new_file)
        }
        "mindmaps" => mindmap_handlers::handle_copy(&vfs_db, &src_id, &dest_folder_id).await?,
        "folders" => {
            if let Some(ref dest_id) = dest_folder_id {
                if is_subfolder_of(&vfs_db, dest_id, &src_id)? {
                    log::error!(
                        "[DSTU::handlers] dstu_copy: FAILED - circular reference detected, src={}, dest={}",
                        src_id, dest_id
                    );
                    return Err(DstuError::from(
                        "Cannot copy a folder into itself or its subfolder".to_string(),
                    ));
                }
            }
            copy_folder_recursive(&vfs_db, &src_id, dest_folder_id.clone(), 0)?
        }
        _ => {
            return Err(DstuError::invalid_node_type(src_resource_type));
        }
    };

    emit_watch_event(&window, DstuWatchEvent::created(&node.path, node.clone()));

    log::info!(
        "[DSTU::handlers] dstu_copy: copied {} to {}",
        src,
        node.path
    );
    Ok(node)
}

// ============================================================================
// 辅助函数：复制相关
// ============================================================================

fn is_subfolder_of(
    vfs_db: &Arc<VfsDatabase>,
    folder_id: &str,
    potential_parent: &str,
) -> DstuResult<bool> {
    // 递归检查 folder_id 是否是 potential_parent 的子文件夹
    let mut current = folder_id.to_string();
    loop {
        let parent = match VfsFolderRepo::get_folder(vfs_db, &current) {
            Ok(Some(f)) => f.parent_id,
            _ => return Ok(false),
        };
        match parent {
            Some(pid) if pid == potential_parent => return Ok(true),
            Some(pid) => current = pid,
            None => return Ok(false),
        }
    }
}

fn copy_folder_recursive(
    vfs_db: &Arc<VfsDatabase>,
    source_folder_id: &str,
    dest_parent_folder_id: Option<String>,
    depth: u32,
) -> DstuResult<DstuNode> {
    const MAX_COPY_DEPTH: u32 = 20;
    if depth > MAX_COPY_DEPTH {
        return Err(DstuError::from(
            "Copy depth exceeds maximum (20)".to_string(),
        ));
    }

    let folder = match VfsFolderRepo::get_folder(vfs_db, source_folder_id) {
        Ok(Some(f)) => f,
        Ok(None) => return Err(DstuError::not_found(source_folder_id)),
        Err(e) => return Err(DstuError::from(e.to_string())),
    };

    let new_folder_name = format!("{} (副本)", folder.title);
    let new_folder = VfsFolder::new(new_folder_name, None, None, None);
    match VfsFolderRepo::create_folder(vfs_db, &new_folder) {
        Ok(_) => {}
        Err(e) => return Err(DstuError::from(e.to_string())),
    }

    if let Some(ref parent_id) = dest_parent_folder_id {
        let folder_item = VfsFolderItem::new(
            Some(parent_id.clone()),
            "folder".to_string(),
            new_folder.id.clone(),
        );
        if let Err(e) = VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item) {
            log::warn!(
                "[DSTU::handlers] copy_folder_recursive: failed to add folder to parent {}: {}",
                parent_id,
                e
            );
        }
    }

    let items = match VfsFolderRepo::list_items_by_folder(vfs_db, Some(source_folder_id)) {
        Ok(i) => i,
        Err(e) => return Err(DstuError::from(e.to_string())),
    };

    for item in items {
        let dest_folder = Some(new_folder.id.clone());
        if let Err(e) = copy_resource_to_folder(vfs_db, &item, &dest_folder) {
            log::warn!(
                "[DSTU::handlers] copy_folder_recursive: failed to copy item {}: {}",
                item.item_id,
                e
            );
        }
    }

    // 递归复制子文件夹
    let sub_folders = match VfsFolderRepo::list_folders_by_parent(vfs_db, Some(source_folder_id))
    {
        Ok(f) => f,
        Err(e) => return Err(DstuError::from(e.to_string())),
    };

    for sub_folder in sub_folders {
        let _ = copy_folder_recursive(
            vfs_db,
            &sub_folder.id,
            Some(new_folder.id.clone()),
            depth + 1,
        );
    }

    let new_path = format!("/{}", new_folder.id);
    Ok(DstuNode::folder(&new_folder.id, &new_path, &new_folder.title))
}

fn copy_resource_to_folder(
    vfs_db: &Arc<VfsDatabase>,
    item: &crate::vfs::VfsFolderItem,
    dest_folder_id: &Option<String>,
) -> DstuResult<()> {
    let dest_folder_id = match dest_folder_id {
        Some(id) => id.clone(),
        None => return Ok(()),
    };

    match item.item_type.as_str() {
        "note" => {
            let note = match VfsNoteRepo::get_note(vfs_db, &item.item_id) {
                Ok(Some(n)) => n,
                Ok(None) => return Err(DstuError::from(format!("笔记不存在: {}", item.item_id))),
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let content = match VfsNoteRepo::get_note_content(vfs_db, &item.item_id) {
                Ok(Some(c)) => c,
                Ok(None) => String::new(),
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_note = match VfsNoteRepo::create_note(
                vfs_db,
                VfsCreateNoteParams {
                    title: note.title.clone(),
                    content,
                    tags: note.tags.clone(),
                },
            ) {
                Ok(n) => n,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "note".to_string(),
                new_note.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "textbook" => {
            let textbook = match VfsTextbookRepo::get_textbook(vfs_db, &item.item_id) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    return Err(DstuError::from(format!("教材不存在: {}", item.item_id)))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_sha256 = format!(
                "{}_{}",
                textbook.sha256,
                chrono::Utc::now().timestamp_millis()
            );

            let new_textbook = match VfsTextbookRepo::create_textbook(
                vfs_db,
                &new_sha256,
                &textbook.file_name,
                textbook.size,
                textbook.blob_hash.as_deref(),
                textbook.original_path.as_deref(),
            ) {
                Ok(t) => t,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "textbook".to_string(),
                new_textbook.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "translation" => {
            let translation = match VfsTranslationRepo::get_translation(vfs_db, &item.item_id) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    return Err(DstuError::from(format!("翻译不存在: {}", item.item_id)))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let content = match VfsTranslationRepo::get_translation_content(vfs_db, &item.item_id) {
                Ok(Some(c)) => c,
                Ok(None) => String::from(r#"{"source":"","translated":""}"#),
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

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
                crate::vfs::types::VfsCreateTranslationParams {
                    title: translation.title.clone(),
                    source,
                    translated,
                    src_lang: translation.src_lang.clone(),
                    tgt_lang: translation.tgt_lang.clone(),
                    engine: translation.engine.clone(),
                    model: translation.model.clone(),
                },
            ) {
                Ok(t) => t,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "translation".to_string(),
                new_translation.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "exam" => {
            let exam = match VfsExamRepo::get_exam_sheet(vfs_db, &item.item_id) {
                Ok(Some(e)) => e,
                Ok(None) => {
                    return Err(DstuError::from(format!("题目集不存在: {}", item.item_id)))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_temp_id = format!("copy_{}", nanoid::nanoid!(10));

            let new_exam = match VfsExamRepo::create_exam_sheet(
                vfs_db,
                VfsCreateExamSheetParams {
                    exam_name: exam.exam_name.clone(),
                    temp_id: new_temp_id,
                    metadata_json: exam.metadata_json.clone(),
                    preview_json: exam.preview_json.clone(),
                    status: exam.status.clone(),
                    folder_id: Some(dest_folder_id.clone()),
                },
            ) {
                Ok(e) => e,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "exam".to_string(),
                new_exam.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "essay" => {
            let session = match VfsEssayRepo::get_session(vfs_db, &item.item_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return Err(DstuError::from(format!(
                        "作文会话不存在: {}",
                        item.item_id
                    )))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_session = match VfsEssayRepo::create_session(
                vfs_db,
                VfsCreateEssaySessionParams {
                    title: session.title.clone(),
                    essay_type: session.essay_type.clone(),
                    grade_level: session.grade_level.clone(),
                    custom_prompt: session.custom_prompt.clone(),
                },
            ) {
                Ok(s) => s,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "essay".to_string(),
                new_session.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "file" | "image" => {
            let file = match VfsFileRepo::get_file(vfs_db, &item.item_id) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    return Err(DstuError::from(format!("文件不存在: {}", item.item_id)))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_sha256 = format!("{}_{}", file.sha256, chrono::Utc::now().timestamp_millis());

            let new_file = match VfsFileRepo::create_file(
                vfs_db,
                &new_sha256,
                &file.file_name,
                file.size,
                &file.file_type,
                file.mime_type.as_deref(),
                file.blob_hash.as_deref(),
                file.original_path.as_deref(),
            ) {
                Ok(f) => f,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "file".to_string(),
                new_file.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        "mindmap" => {
            let mindmap = match VfsMindMapRepo::get_mindmap(vfs_db, &item.item_id) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    return Err(DstuError::from(format!("知识导图不存在: {}", item.item_id)))
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let content = match VfsMindMapRepo::get_mindmap_content(vfs_db, &item.item_id) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    r#"{"version":"1.0","root":{"id":"root","text":"根节点","children":[]}}"#
                        .to_string()
                }
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let new_mindmap = match VfsMindMapRepo::create_mindmap(
                vfs_db,
                VfsCreateMindMapParams {
                    title: mindmap.title.clone(),
                    description: mindmap.description.clone(),
                    content,
                    default_view: mindmap.default_view.clone(),
                    theme: mindmap.theme.clone(),
                },
            ) {
                Ok(m) => m,
                Err(e) => return Err(DstuError::from(e.to_string())),
            };

            let folder_item = VfsFolderItem::new(
                Some(dest_folder_id),
                "mindmap".to_string(),
                new_mindmap.id.clone(),
            );
            VfsFolderRepo::add_item_to_folder(vfs_db, &folder_item).map_err(|e| e.to_string())?;
        }
        _ => {
            log::warn!(
                "[DSTU::handlers] copy_resource_to_folder: unsupported item type: {}",
                item.item_type
            );
        }
    }

    Ok(())
}

// ============================================================================
// Tauri 命令：获取内容
// ============================================================================

#[tauri::command]
pub async fn dstu_get_content(
    path: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<String> {
    log::info!("[DSTU::handlers] dstu_get_content: path={}", path);

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_get_content: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    get_content_by_type(&vfs_db, &resource_type, &id).map_err(DstuError::from)
}

#[tauri::command]
pub async fn dstu_get_exam_content(
    exam_id: String,
    is_multimodal: bool,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Vec<crate::chat_v2::resource_types::ContentBlock>> {
    log::info!(
        "[DSTU::handlers] dstu_get_exam_content: exam_id={}, is_multimodal={}",
        exam_id,
        is_multimodal
    );

    super::super::exam_formatter::format_exam_for_context(&vfs_db.inner().clone(), &exam_id, is_multimodal)
        .await
        .map_err(DstuError::from)
}

// ============================================================================
// Tauri 命令：设置元数据
// ============================================================================

#[tauri::command]
pub async fn dstu_set_metadata(
    app: tauri::AppHandle,
    path: String,
    metadata: Value,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_set_metadata: path={}", path);

    let normalized_path = if path.starts_with('/') {
        path.clone()
    } else {
        format!("/{}", path)
    };

    let (resource_type, id) = match VfsFolderRepo::get_folder_item_by_cached_path(
        &vfs_db,
        &normalized_path,
    ) {
        Ok(Some(folder_item)) => {
            log::info!(
                "[DSTU::handlers] dstu_set_metadata: found by cached_path, item_type={}, item_id={}",
                folder_item.item_type,
                folder_item.item_id
            );
            let resource_type = match folder_item.item_type.as_str() {
                "note" => "notes",
                "textbook" => "textbooks",
                "exam" => "exams",
                "translation" => "translations",
                "essay" => "essays",
                "image" => "images",
                "file" => "files",
                "folder" => "folders",
                other => {
                    log::warn!(
                        "[DSTU::handlers] dstu_set_metadata: unsupported item_type: {}",
                        other
                    );
                    return Err(DstuError::invalid_node_type(other));
                }
            };
            (resource_type.to_string(), folder_item.item_id.clone())
        }
        Ok(None) => {
            let segments: Vec<&str> = normalized_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            if segments.len() == 1 {
                let id = segments[0].to_string();
                let resource_type = infer_resource_type_from_id(&id);

                if resource_type == "unknown" {
                    log::warn!(
                        "[DSTU::handlers] dstu_set_metadata: FAILED - cannot infer type from id={}",
                        id
                    );
                    return Err(DstuError::from("资源不存在".to_string()));
                }

                log::info!(
                    "[DSTU::handlers] dstu_set_metadata: fallback to simple path, type={}, id={}",
                    resource_type,
                    id
                );
                (resource_type.to_string(), id)
            } else {
                log::warn!(
                    "[DSTU::handlers] dstu_set_metadata: FAILED - resource not found by cached_path, path={}",
                    normalized_path
                );
                return Err(DstuError::from("资源不存在".to_string()));
            }
        }
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_metadata: FAILED - get_folder_item_by_cached_path error, path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let node = match resource_type.as_str() {
        "notes" => {
            let title = metadata
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tags = metadata.get("tags").and_then(|v| {
                v.as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            });
            let favorite = metadata.get("isFavorite").and_then(|v| v.as_bool());

            let mut updated_note = match VfsNoteRepo::update_note(
                &vfs_db,
                &id,
                VfsUpdateNoteParams {
                    content: None,
                    title,
                    tags,
                    expected_updated_at: None,
                },
            ) {
                Ok(n) => {
                    log::info!(
                        "[DSTU::handlers] dstu_set_metadata: SUCCESS - type=note, id={}",
                        id
                    );
                    n
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_set_metadata: FAILED - type=note, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };

            if let Some(favorite) = favorite {
                if let Err(e) = VfsNoteRepo::set_favorite(&vfs_db, &id, favorite) {
                    log::error!(
                        "[DSTU::handlers] dstu_set_metadata: FAILED - set note favorite id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
                updated_note.is_favorite = favorite;
            }

            crate::dstu::handler_utils::note_to_dstu_node(&updated_note)
        }
        // ★ task-053 修复:翻译保存此前落入 `_` 兜底分支——metadata 序列化后从未写库,
        // sourceText/translatedText/srcLang/tgtLang/qualityRating/isFavorite 全部静默丢失。
        "translations" => {
            let conn = vfs_db
                .get_conn_safe()
                .map_err(|e| DstuError::from(e.to_string()))?;
            let row: Option<(String, Option<String>)> = conn
                .query_row(
                    "SELECT resource_id, metadata_json FROM translations WHERE id = ?1 AND deleted_at IS NULL",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| DstuError::from(e.to_string()))?;
            let (resource_id, old_metadata_json) =
                row.ok_or_else(|| DstuError::not_found(&path))?;

            let now = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();

            if let Some(v) = metadata.get("title").and_then(|v| v.as_str()) {
                conn.execute(
                    "UPDATE translations SET title = ?1, updated_at = ?2 WHERE id = ?3",
                    params![v, now, id],
                )
                .map_err(|e| DstuError::from(e.to_string()))?;
            }
            if let Some(v) = metadata.get("srcLang").and_then(|v| v.as_str()) {
                conn.execute(
                    "UPDATE translations SET src_lang = ?1, updated_at = ?2 WHERE id = ?3",
                    params![v, now, id],
                )
                .map_err(|e| DstuError::from(e.to_string()))?;
            }
            if let Some(v) = metadata.get("tgtLang").and_then(|v| v.as_str()) {
                conn.execute(
                    "UPDATE translations SET tgt_lang = ?1, updated_at = ?2 WHERE id = ?3",
                    params![v, now, id],
                )
                .map_err(|e| DstuError::from(e.to_string()))?;
            }
            if let Some(v) = metadata.get("isFavorite").and_then(|v| v.as_bool()) {
                VfsTranslationRepo::set_favorite_with_conn(&conn, &id, v)
                    .map_err(DstuError::from)?;
            }
            if let Some(v) = metadata.get("qualityRating").and_then(|v| v.as_i64()) {
                if (1..=5).contains(&v) {
                    VfsTranslationRepo::set_quality_rating_with_conn(&conn, &id, v as i32)
                        .map_err(DstuError::from)?;
                }
            }
            // formality/customPrompt 无独立列 → 并入 metadata_json(读侧经 converter 合并回 metadata)
            let mut extra: serde_json::Map<String, Value> = old_metadata_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let mut extra_changed = false;
            for key in ["formality", "customPrompt"] {
                if let Some(v) = metadata.get(key) {
                    if !v.is_null() {
                        extra.insert(key.to_string(), v.clone());
                        extra_changed = true;
                    }
                }
            }
            if extra_changed {
                let s = serde_json::to_string(&Value::Object(extra))
                    .map_err(|e| DstuError::from(e.to_string()))?;
                conn.execute(
                    "UPDATE translations SET metadata_json = ?1, updated_at = ?2 WHERE id = ?3",
                    params![s, now, id],
                )
                .map_err(|e| DstuError::from(e.to_string()))?;
            }
            // 源文/译文 → resources.data {"source","translated"}(repo 维护 hash/index_state)
            let source = metadata.get("sourceText").and_then(|v| v.as_str());
            let translated = metadata.get("translatedText").and_then(|v| v.as_str());
            if source.is_some() || translated.is_some() {
                let old_data: Option<String> = conn
                    .query_row(
                        "SELECT data FROM resources WHERE id = ?1",
                        params![resource_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| DstuError::from(e.to_string()))?;
                let mut data_json: Value = old_data
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Some(v) = source {
                    data_json["source"] = Value::String(v.to_string());
                }
                if let Some(v) = translated {
                    data_json["translated"] = Value::String(v.to_string());
                }
                let data_str = serde_json::to_string(&data_json)
                    .map_err(|e| DstuError::from(e.to_string()))?;
                crate::vfs::repos::VfsResourceRepo::update_resource_data_with_conn(
                    &conn,
                    &resource_id,
                    &data_str,
                )
                .map_err(DstuError::from)?;
            }

            log::info!(
                "[DSTU::handlers] dstu_set_metadata: SUCCESS - type=translation, id={}",
                id
            );
            get_resource_by_type_and_id(&vfs_db, &resource_type, &id)
                .await
                .map_err(DstuError::from)?
                .ok_or_else(|| DstuError::not_found(&path))?
        }
        // ★ task-053 修复:作文会话元数据(essayType/gradeLevel/customPrompt/isFavorite)
        // 此前同样被 `_` 分支丢弃。modeId 无 essay_sessions 列,暂不持久化(见文档)。
        "essays" => {
            let title = metadata.get("title").and_then(|v| v.as_str());
            let essay_type = metadata.get("essayType").and_then(|v| v.as_str());
            let grade_level = metadata.get("gradeLevel").and_then(|v| v.as_str());
            let custom_prompt = metadata.get("customPrompt").and_then(|v| v.as_str());
            let is_favorite = metadata.get("isFavorite").and_then(|v| v.as_bool());
            if metadata.get("modeId").is_some() {
                log::warn!(
                    "[DSTU::handlers] dstu_set_metadata: essays modeId has no storage column (essay_sessions), value NOT persisted"
                );
            }
            VfsEssayRepo::update_session(
                &vfs_db,
                &id,
                title,
                is_favorite,
                essay_type,
                grade_level,
                custom_prompt,
            )
            .map_err(DstuError::from)?;
            log::info!(
                "[DSTU::handlers] dstu_set_metadata: SUCCESS - type=essay, id={}",
                id
            );
            get_resource_by_type_and_id(&vfs_db, &resource_type, &id)
                .await
                .map_err(DstuError::from)?
                .ok_or_else(|| DstuError::not_found(&path))?
        }
        // ★ task-053 修复:教材/文件阅读进度(readingProgress.page → files.last_page)
        // 与收藏此前也被 `_` 丢弃——阅读进度跨会话恢复因此从未生效(只剩 sessionStorage)。
        "textbooks" | "files" | "images" => {
            let last_page = metadata
                .get("readingProgress")
                .and_then(|v| v.get("page"))
                .and_then(|v| v.as_i64())
                .map(|p| p as i32);
            let favorite = metadata.get("isFavorite").and_then(|v| v.as_bool());
            if last_page.is_some() || favorite.is_some() {
                crate::textbooks_db::TextbooksDb::update_vfs(
                    &vfs_db,
                    &id,
                    crate::textbooks_db::VfsUpdateTextbookParams {
                        last_page,
                        favorite,
                        ..Default::default()
                    },
                )
                .map_err(|e| DstuError::from(e.to_string()))?;
                log::info!(
                    "[DSTU::handlers] dstu_set_metadata: SUCCESS - type={}, id={}, last_page={:?}",
                    resource_type,
                    id,
                    last_page
                );
            }
            get_resource_by_type_and_id(&vfs_db, &resource_type, &id)
                .await
                .map_err(DstuError::from)?
                .ok_or_else(|| DstuError::not_found(&path))?
        }
        _ => {
            log::warn!(
                "[DSTU::handlers] dstu_set_metadata: unsupported type {}, NO metadata written (keys: {:?})",
                resource_type,
                metadata.as_object().map(|o| o.keys().collect::<Vec<_>>())
            );
            // 未覆盖类型:返回节点但不写任何数据(此前版本注释声称"直接存储",实际从未写库)
            get_resource_by_type_and_id(&vfs_db, &resource_type, &id)
                .await
                .map_err(|e| DstuError::from(e))?
                .ok_or_else(|| DstuError::not_found(&path))?
        }
    };

    emit_watch_event(&window, DstuWatchEvent::updated(&path, node));
    Ok(())
}

// ============================================================================
// Tauri 命令：恢复资源
// ============================================================================

#[tauri::command]
pub async fn dstu_restore(
    app: tauri::AppHandle,
    path: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<DstuNode> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_restore: path={}", path);

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_restore: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    if let Err(e) = restore_resource_by_type(&vfs_db, &resource_type, &id) {
        log::error!(
            "[DSTU::handlers] dstu_restore: FAILED - type={}, id={}, error={}",
            resource_type,
            id,
            e
        );
        return Err(DstuError::from(e.to_string()));
    }

    let node = match resource_type.as_str() {
        "notes" | "note" => note_handlers::handle_restore(&vfs_db, &id).await?,
        "textbooks" | "textbook" => textbook_handlers::handle_restore(&vfs_db, &id).await?,
        "translations" | "translation" => translation_handlers::handle_restore(&vfs_db, &id).await?,
        "exams" | "exam" => exam_handlers::handle_restore(&vfs_db, &id).await?,
        "essays" | "essay" => essay_handlers::handle_restore(&vfs_db, &id).await?,
        "folders" | "folder" => match VfsFolderRepo::get_folder(&vfs_db, &id) {
            Ok(Some(f)) => Some(DstuNode::folder(&f.id, &path, &f.title)),
            Ok(None) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: folder not found after restore, id={}",
                    id
                );
                None
            }
            Err(e) => {
                log::warn!(
                    "[DSTU::handlers] dstu_restore: get_folder error, id={}, error={}",
                    id,
                    e
                );
                None
            }
        },
        "images" | "files" | "attachments" | "image" | "file" | "attachment" => {
            image_handlers::handle_restore(&vfs_db, &id).await?
        }
        "mindmaps" | "mindmap" => mindmap_handlers::handle_restore(&vfs_db, &id).await?,
        _ => None,
    };

    emit_watch_event(&window, DstuWatchEvent::restored(&path, node.clone()));

    log::info!("[DSTU::handlers] dstu_restore: restored {}", path);

    match node {
        Some(n) => Ok(n),
        None => Err(DstuError::from(format!(
            "Resource restored but failed to retrieve node info: {}",
            path
        ))),
    }
}

// ============================================================================
// Tauri 命令：永久删除
// ============================================================================

#[tauri::command]
pub async fn dstu_purge(
    app: tauri::AppHandle,
    path: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<crate::vfs::lance_store::VfsLanceStore>>,
) -> DstuResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_purge: path={}", path);

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_purge: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let resource_id = {
        if let Ok(conn) = vfs_db.get_conn_safe() {
            let mut stmt = conn
                .prepare("SELECT resource_id FROM files WHERE id = ?1")
                .map_err(|e| e.to_string())?;
            stmt.query_row(rusqlite::params![&id], |row| row.get::<_, Option<String>>(0))
                .ok()
                .flatten()
        } else {
            None
        }
    };

    purge_resource_by_type(&vfs_db, &resource_type, &id)?;

    if let Some(rid) = resource_id {
        let lance_store = Arc::clone(lance_store.inner());
        crate::background_tasks::BACKGROUND_TASKS.spawn(async move {
            let _ = lance_store.delete_by_resource("text", &rid).await;
            let _ = lance_store.delete_by_resource("multimodal", &rid).await;
        });
    }

    emit_watch_event(&window, DstuWatchEvent::purged(&path));
    log::info!("[DSTU::handlers] dstu_purge: purged {}", path);
    Ok(())
}

// ============================================================================
// Tauri 命令：设置收藏
// ============================================================================

#[tauri::command]
pub async fn dstu_set_favorite(
    app: tauri::AppHandle,
    path: String,
    favorite: bool,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<()> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_set_favorite: path={}, favorite={}",
        path,
        favorite
    );

    let (resource_type, id) = match extract_resource_info(&path) {
        Ok((rt, rid)) => (rt, rid),
        Err(e) => {
            log::error!(
                "[DSTU::handlers] dstu_set_favorite: FAILED - path={}, error={}",
                path,
                e
            );
            return Err(DstuError::from(e.to_string()));
        }
    };

    let node = match resource_type.as_str() {
        "notes" => note_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?,
        "textbooks" => textbook_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?,
        "exams" => exam_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?,
        "folders" => {
            match VfsFolderRepo::set_favorite(&vfs_db, &id, favorite) {
                Ok(_) => log::info!(
                    "[DSTU::handlers] dstu_set_favorite: SUCCESS - type=folder, id={}, favorite={}",
                    id,
                    favorite
                ),
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_set_favorite: FAILED - type=folder, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            }
            let folder = match VfsFolderRepo::get_folder(&vfs_db, &id) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    log::warn!(
                        "[DSTU::handlers] dstu_set_favorite: FAILED - folder not found after set_favorite, id={}",
                        id
                    );
                    return Err(DstuError::from("操作失败".to_string()));
                }
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_set_favorite: FAILED - get_folder error, id={}, error={}",
                        id,
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };
            let folder_path = format!("/{}", folder.id);
            DstuNode::folder(&folder.id, &folder_path, &folder.title)
                .with_timestamps(folder.created_at, folder.updated_at)
                .with_metadata(serde_json::json!({
                    "isExpanded": folder.is_expanded,
                    "icon": folder.icon,
                    "color": folder.color,
                }))
        }
        "images" | "files" => {
            image_handlers::handle_set_favorite(&vfs_db, &id, favorite, &resource_type).await?
        }
        "translations" => {
            translation_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?
        }
        "essays" => essay_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?,
        "mindmaps" => mindmap_handlers::handle_set_favorite(&vfs_db, &id, favorite).await?,
        _ => {
            return Err(DstuError::from(format!(
                "Resource type '{}' does not support favorite operation",
                resource_type
            )));
        }
    };

    emit_watch_event(&window, DstuWatchEvent::updated(&path, node));

    log::info!(
        "[DSTU::handlers] dstu_set_favorite: set {} to favorite={}",
        path,
        favorite
    );
    Ok(())
}

// ============================================================================
// Tauri 命令：列出已删除资源
// ============================================================================

#[tauri::command]
pub async fn dstu_list_deleted(
    resource_type: String,
    limit: Option<u32>,
    offset: Option<u32>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Vec<DstuNode>> {
    log::info!("[DSTU::handlers] dstu_list_deleted: type={}", resource_type);

    let limit = limit.unwrap_or(100);
    let offset = offset.unwrap_or(0);

    match resource_type.as_str() {
        "notes" => note_handlers::handle_list_deleted(&vfs_db, limit, offset),
        "textbooks" => textbook_handlers::handle_list_deleted(&vfs_db, limit, offset),
        "exams" => exam_handlers::handle_list_deleted(&vfs_db, limit, offset),
        "translations" => translation_handlers::handle_list_deleted(&vfs_db, limit, offset),
        "essays" => {
            let sessions = match VfsEssayRepo::list_deleted_sessions(&vfs_db, limit, offset) {
                Ok(s) => s,
                Err(e) => {
                    log::error!(
                        "[DSTU::handlers] dstu_list_deleted: FAILED - list_deleted_sessions error={}",
                        e
                    );
                    return Err(DstuError::from(e.to_string()));
                }
            };

            let nodes: Vec<DstuNode> = sessions
                .into_iter()
                .map(|s| {
                    let path = format!("/{}", s.id);
                    let created_at = chrono::DateTime::parse_from_rfc3339(&s.created_at)
                        .map(|dt| dt.timestamp_millis())
                        .unwrap_or(0);
                    let updated_at = chrono::DateTime::parse_from_rfc3339(&s.updated_at)
                        .map(|dt| dt.timestamp_millis())
                        .unwrap_or(0);
                    DstuNode {
                        id: s.id.clone(),
                        source_id: s.id.clone(),
                        name: s.title.clone(),
                        path,
                        node_type: DstuNodeType::Essay,
                        size: None,
                        created_at,
                        updated_at,
                        children: None,
                        child_count: None,
                        resource_id: None,
                        resource_hash: None,
                        preview_type: Some("essay".to_string()),
                        metadata: Some(serde_json::json!({
                            "is_favorite": s.is_favorite,
                            "deleted_at": s.deleted_at,
                        })),
                    }
                })
                .collect();

            Ok(nodes)
        }
        _ => {
            return Err(DstuError::from(format!(
                "Resource type '{}' does not support listing deleted items",
                resource_type
            )));
        }
    }
}

// ============================================================================
// Tauri 命令：清空回收站
// ============================================================================

#[tauri::command]
pub async fn dstu_purge_all(
    app: tauri::AppHandle,
    resource_type: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<crate::vfs::lance_store::VfsLanceStore>>,
) -> DstuResult<usize> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_purge_all: type={}", resource_type);

    let resource_ids_to_cleanup: Vec<String> = {
        if let Ok(conn) = vfs_db.get_conn_safe() {
            let sql = match resource_type.as_str() {
                "notes" => Some(
                    "SELECT resource_id FROM notes WHERE deleted_at IS NOT NULL AND resource_id IS NOT NULL",
                ),
                "textbooks" => Some(
                    "SELECT resource_id FROM files WHERE status = 'deleted' AND resource_id IS NOT NULL",
                ),
                _ => None,
            };
            if let Some(sql) = sql {
                if let Ok(mut stmt) = conn.prepare(sql) {
                    stmt.query_map([], |row| row.get::<_, String>(0))
                        .map(|rows| rows.flatten().collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    };

    let count = match resource_type.as_str() {
        "notes" => match VfsNoteRepo::purge_deleted_notes(&vfs_db) {
            Ok(c) => {
                log::info!(
                    "[DSTU::handlers] dstu_purge_all: SUCCESS - type=notes, count={}",
                    c
                );
                c
            }
            Err(e) => {
                log::error!(
                    "[DSTU::handlers] dstu_purge_all: FAILED - type=notes, error={}",
                    e
                );
                return Err(DstuError::from(e.to_string()));
            }
        },
        "textbooks" => match VfsTextbookRepo::purge_deleted_textbooks(&vfs_db) {
            Ok(c) => {
                log::info!(
                    "[DSTU::handlers] dstu_purge_all: SUCCESS - type=textbooks, count={}",
                    c
                );
                c
            }
            Err(e) => {
                log::error!(
                    "[DSTU::handlers] dstu_purge_all: FAILED - type=textbooks, error={}",
                    e
                );
                return Err(DstuError::from(e.to_string()));
            }
        },
        _ => {
            return Err(DstuError::from(format!(
                "Resource type '{}' does not support purge_all operation",
                resource_type
            )));
        }
    };

    let path = format!("/{}/_trash", resource_type);
    emit_watch_event(&window, DstuWatchEvent::purged(&path));

    if !resource_ids_to_cleanup.is_empty() {
        let lance_for_cleanup = Arc::clone(lance_store.inner());
        crate::background_tasks::BACKGROUND_TASKS.spawn(async move {
            for rid in &resource_ids_to_cleanup {
                let _ = lance_for_cleanup.delete_by_resource("text", rid).await;
                let _ = lance_for_cleanup
                    .delete_by_resource("multimodal", rid)
                    .await;
            }
            log::info!(
                "[DSTU::handlers] dstu_purge_all: cleaned up vectors for {} resources",
                resource_ids_to_cleanup.len()
            );
        });
    }

    log::info!(
        "[DSTU::handlers] dstu_purge_all: purged {} {} resources",
        count,
        resource_type
    );
    Ok(count)
}

// ============================================================================
// Tauri 命令：批量删除
// ============================================================================

#[tauri::command]
pub async fn dstu_delete_many(
    app: tauri::AppHandle,
    paths: Vec<String>,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<crate::vfs::lance_store::VfsLanceStore>>,
) -> DstuResult<usize> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_delete_many: {} paths", paths.len());

    if paths.len() > MAX_BATCH_SIZE {
        return Err(DstuError::from(format!(
            "批量操作数量超出限制：最多允许 {} 个，实际 {} 个",
            MAX_BATCH_SIZE,
            paths.len()
        )));
    }

    if paths.is_empty() {
        return Ok(0);
    }

    let mut parsed_items: Vec<(String, String, String)> = Vec::with_capacity(paths.len());
    for path in &paths {
        let (resource_type, id) = match extract_resource_info(path) {
            Ok((rt, rid)) => (rt, rid),
            Err(e) => {
                log::warn!("[DSTU::handlers] Invalid path {}: {}", path, e);
                return Err(DstuError::from(format!("无效的资源路径 '{}': {}", path, e)));
            }
        };
        parsed_items.push((path.clone(), resource_type, id));
    }

    let resource_ids_to_cleanup: Vec<String> = {
        if let Ok(conn) = vfs_db.get_conn_safe() {
            parsed_items
                .iter()
                .filter_map(|(_, resource_type, id)| {
                    let sql = match resource_type.as_str() {
                        "notes" | "note" => Some("SELECT resource_id FROM notes WHERE id = ?1"),
                        "textbooks" | "textbook" | "images" | "image" | "files" | "file"
                        | "attachments" | "attachment" => {
                            Some("SELECT resource_id FROM files WHERE id = ?1")
                        }
                        "exams" | "exam" => {
                            Some("SELECT resource_id FROM exam_sheets WHERE id = ?1")
                        }
                        "translations" | "translation" => {
                            Some("SELECT resource_id FROM translations WHERE id = ?1")
                        }
                        "mindmaps" | "mindmap" => {
                            Some("SELECT resource_id FROM mindmaps WHERE id = ?1")
                        }
                        _ => None,
                    };
                    sql.and_then(|s| {
                        conn.query_row(s, rusqlite::params![id], |row| {
                            row.get::<_, Option<String>>(0)
                        })
                        .ok()
                        .flatten()
                    })
                })
                .collect()
        } else {
            Vec::new()
        }
    };

    let vfs_db_clone = vfs_db.inner().clone();
    let items_for_delete = parsed_items.clone();

    let transaction_result = (|| -> DstuResult<Vec<String>> {
        let conn = vfs_db_clone.get_conn_safe().map_err(|e| e.to_string())?;

        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;

        for (_path, resource_type, id) in &items_for_delete {
            delete_resource_by_type_with_conn(&tx, resource_type, id)?;
        }

        tx.commit().map_err(|e| e.to_string())?;
        log::info!(
            "[DSTU::handlers] dstu_delete_many: 事务提交成功 - {} items",
            items_for_delete.len()
        );

        Ok(items_for_delete.iter().map(|(p, _, _)| p.clone()).collect())
    })()?;

    for p in &transaction_result {
        emit_watch_event(&window, DstuWatchEvent::deleted(p));
    }

    if !resource_ids_to_cleanup.is_empty() {
        let lance_for_cleanup = Arc::clone(lance_store.inner());
        crate::background_tasks::BACKGROUND_TASKS.spawn(async move {
            for rid in &resource_ids_to_cleanup {
                let _ = lance_for_cleanup.delete_by_resource("text", rid).await;
                let _ = lance_for_cleanup
                    .delete_by_resource("multimodal", rid)
                    .await;
            }
            log::info!(
                "[DSTU::handlers] dstu_delete_many: cleaned up vectors for {} resources",
                resource_ids_to_cleanup.len()
            );
        });
    }

    let count = transaction_result.len();
    log::info!(
        "[DSTU::handlers] dstu_delete_many: deleted {} items",
        count
    );
    Ok(count)
}

// ============================================================================
// Tauri 命令：批量恢复
// ============================================================================

#[tauri::command]
pub async fn dstu_restore_many(
    app: tauri::AppHandle,
    paths: Vec<String>,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<usize> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!("[DSTU::handlers] dstu_restore_many: {} paths", paths.len());

    if paths.len() > MAX_BATCH_SIZE {
        return Err(DstuError::from(format!(
            "批量操作数量超出限制：最多允许 {} 个，实际 {} 个",
            MAX_BATCH_SIZE,
            paths.len()
        )));
    }

    if paths.is_empty() {
        return Ok(0);
    }

    let mut parsed_items: Vec<(String, String, String)> = Vec::with_capacity(paths.len());
    for path in &paths {
        let (resource_type, id) = match extract_resource_info(path) {
            Ok((rt, rid)) => (rt, rid),
            Err(e) => {
                log::warn!("[DSTU::handlers] Invalid path {}: {}", path, e);
                return Err(DstuError::from(format!("无效的资源路径 '{}': {}", path, e)));
            }
        };
        parsed_items.push((path.clone(), resource_type, id));
    }

    let vfs_db_clone = vfs_db.inner().clone();
    let items_for_restore = parsed_items.clone();

    let transaction_result = (|| -> DstuResult<Vec<String>> {
        let conn = vfs_db_clone.get_conn_safe().map_err(|e| e.to_string())?;

        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;

        for (_path, resource_type, id) in &items_for_restore {
            restore_resource_by_type_with_conn(&tx, resource_type, id)?;
        }

        tx.commit().map_err(|e| e.to_string())?;
        log::info!(
            "[DSTU::handlers] dstu_restore_many: 事务提交成功 - {} items",
            items_for_restore.len()
        );

        Ok(items_for_restore.iter().map(|(p, _, _)| p.clone()).collect())
    })()?;

    for p in &transaction_result {
        emit_watch_event(&window, DstuWatchEvent::restored(p, Some(crate::dstu::types::DstuNode::folder("", "", ""))));
    }

    let count = transaction_result.len();
    log::info!(
        "[DSTU::handlers] dstu_restore_many: restored {} items",
        count
    );
    Ok(count)
}

// ============================================================================
// Tauri 命令：批量移动
// ============================================================================

#[tauri::command]
pub async fn dstu_move_many(
    app: tauri::AppHandle,
    paths: Vec<String>,
    dest_folder: String,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<usize> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_move_many: {} paths to {}",
        paths.len(),
        dest_folder
    );

    if paths.len() > MAX_BATCH_SIZE {
        return Err(DstuError::from(format!(
            "批量操作数量超出限制：最多允许 {} 个，实际 {} 个",
            MAX_BATCH_SIZE,
            paths.len()
        )));
    }

    let dest_folder_id = if dest_folder.trim().is_empty() || dest_folder.trim() == "/" {
        None
    } else {
        let (dst_type, dst_id) = match extract_resource_info(&dest_folder) {
            Ok((rt, rid)) => (rt, rid),
            Err(e) => {
                log::error!(
                    "[DSTU::handlers] dstu_move_many: FAILED - dest={}, error={}",
                    dest_folder,
                    e
                );
                return Err(DstuError::from(e.to_string()));
            }
        };
        if dst_type != "folders" {
            return Err(DstuError::from("Destination must be a folder".to_string()));
        }
        Some(dst_id)
    };

    let mut success_count = 0;

    for path in &paths {
        let (resource_type, id) = match extract_resource_info(path) {
            Ok((rt, rid)) => (rt, rid),
            Err(_) => continue,
        };

        let item_type = match resource_type.as_str() {
            "notes" => "note",
            "textbooks" => "textbook",
            "exams" => "exam",
            "translations" => "translation",
            "essays" => "essay",
            "folders" => "folder",
            "mindmaps" => "mindmap",
            "files" | "images" | "attachments" => "file",
            _ => continue,
        };

        let result =
            VfsFolderRepo::move_item_to_folder(&vfs_db, item_type, &id, dest_folder_id.as_deref());
        if result.is_ok() {
            success_count += 1;

            if let Ok(Some(node)) = get_resource_by_type_and_id(&vfs_db, &resource_type, &id).await
            {
                let new_path = node.path.clone();
                emit_watch_event(&window, DstuWatchEvent::moved(path, &new_path, node));
            }
        } else if let Err(e) = result {
            log::warn!(
                "[DSTU::handlers] dstu_move_many: FAILED - type={}, id={}, error={}",
                item_type,
                id,
                e
            );
        }
    }

    log::info!(
        "[DSTU::handlers] dstu_move_many: moved {} of {} items",
        success_count,
        paths.len()
    );
    Ok(success_count)
}

// ============================================================================
// Tauri 命令：资源变化监听
// ============================================================================

#[tauri::command]
pub async fn dstu_watch(path: String) -> DstuResult<()> {
    log::info!("[DSTU::handlers] dstu_watch: path={}", path);
    Ok(())
}

#[tauri::command]
pub async fn dstu_unwatch(path: String) -> DstuResult<()> {
    log::info!("[DSTU::handlers] dstu_unwatch: path={}", path);
    Ok(())
}

// ============================================================================
// Tauri 命令：路径解析
// ============================================================================

#[tauri::command]
pub async fn dstu_parse_path(path: String) -> DstuResult<DstuParsedPath> {
    log::info!("[DSTU::handlers] dstu_parse_path: path={}", path);

    if path.is_empty() || path == "/" {
        return Ok(DstuParsedPath::root());
    }

    let normalized = if path.starts_with('/') {
        path.clone()
    } else {
        format!("/{}", path)
    };
    let normalized = normalized.trim_end_matches('/');

    if normalized.starts_with("/@") {
        let virtual_name = &normalized[2..];
        return Ok(DstuParsedPath::virtual_path(virtual_name));
    }

    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

    if segments.is_empty() {
        return Ok(DstuParsedPath::root());
    }

    let last_segment = match segments.last() {
        Some(s) => *s,
        None => return Ok(DstuParsedPath::root()),
    };
    let resource_type = DstuNodeType::from_id_prefix(last_segment)
        .map(|t| t.to_type_string().to_string());

    if resource_type.is_some() {
        let resource_id = last_segment.to_string();
        let folder_path = if segments.len() > 1 {
            Some(format!("/{}", segments[..segments.len() - 1].join("/")))
        } else {
            None
        };

        Ok(DstuParsedPath {
            full_path: normalized.to_string(),
            folder_path,
            resource_id: Some(resource_id),
            resource_type,
            is_root: false,
            is_virtual: false,
            virtual_type: None,
        })
    } else {
        Ok(DstuParsedPath {
            full_path: normalized.to_string(),
            folder_path: Some(normalized.to_string()),
            resource_id: None,
            resource_type: None,
            is_root: false,
            is_virtual: false,
            virtual_type: None,
        })
    }
}

#[tauri::command]
pub async fn dstu_build_path(
    folder_id: Option<String>,
    resource_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<String> {
    log::info!(
        "[DSTU::handlers] dstu_build_path: folder_id={:?}, resource_id={}",
        folder_id,
        resource_id
    );

    let folder_path = match folder_id {
        Some(ref fid) => {
            VfsFolderRepo::build_folder_path(&vfs_db, fid).map_err(|e| e.to_string())?
        }
        None => String::new(),
    };

    let full_path = if folder_path.is_empty() {
        format!("/{}", resource_id)
    } else {
        format!("{}/{}", folder_path, resource_id)
    };

    log::info!("[DSTU::handlers] dstu_build_path: result={}", full_path);
    Ok(full_path)
}

// ============================================================================
// Tauri 命令：资源定位
// ============================================================================

#[tauri::command]
pub async fn dstu_get_resource_location(
    resource_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<ResourceLocation> {
    log::info!(
        "[DSTU::handlers] dstu_get_resource_location: resource_id={}",
        resource_id
    );

    let resource_type =
        DstuNodeType::from_id_prefix(&resource_id).map(|t| t.to_type_string().to_string()).unwrap_or_else(|| "unknown".to_string());

    let conn = vfs_db.get_conn_safe().map_err(|e| e.to_string())?;

    let folder_item: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT folder_id, cached_path FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![resource_type.as_str(), &resource_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let (folder_id, cached_path) = folder_item.unwrap_or((None, None));

    let folder_path = match &folder_id {
        Some(fid) => VfsFolderRepo::build_folder_path(&vfs_db, fid)
            .unwrap_or_else(|_| String::new()),
        None => String::new(),
    };

    let full_path = cached_path.unwrap_or_else(|| {
        if folder_path.is_empty() {
            format!("/{}", resource_id)
        } else {
            format!("{}/{}", folder_path, resource_id)
        }
    });

    log::info!(
        "[DSTU::handlers] dstu_get_resource_location: SUCCESS - folder_id={:?}, path={}",
        folder_id,
        full_path
    );

    Ok(ResourceLocation {
        id: resource_id,
        resource_type,
        folder_id,
        folder_path,
        full_path,
        hash: None,
    })
}

#[tauri::command]
pub async fn dstu_get_resource_by_path(
    path: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<Option<DstuNode>> {
    log::info!("[DSTU::handlers] dstu_get_resource_by_path: path={}", path);

    let parsed = dstu_parse_path(path.clone()).await?;

    if parsed.is_root {
        return Ok(Some(DstuNode::folder("root", "/", "根目录")));
    }

    if parsed.is_virtual {
        let name = parsed.full_path.trim_start_matches("/@");
        return Ok(Some(DstuNode::folder(
            &format!("@{}", name),
            &parsed.full_path,
            name,
        )));
    }

    if let Some(ref resource_id) = parsed.resource_id {
        let resource_type = parsed.resource_type.as_deref().unwrap_or("unknown");

        match resource_type {
            "note" => note_handlers::handle_get_by_path(&vfs_db, resource_id).await,
            "textbook" => textbook_handlers::handle_get_by_path(&vfs_db, resource_id).await,
            "exam" => exam_handlers::handle_get_by_path(&vfs_db, resource_id).await,
            "translation" => translation_handlers::handle_get_by_path(&vfs_db, resource_id).await,
            "essay" => essay_handlers::handle_get_by_path(&vfs_db, resource_id).await,
            "folder" => match VfsFolderRepo::get_folder(&vfs_db, resource_id) {
                Ok(Some(folder)) => Ok(Some(DstuNode::folder(
                    &folder.id,
                    &parsed.full_path,
                    &folder.title,
                ))),
                Ok(None) => Ok(None),
                Err(e) => Err(DstuError::from(e.to_string())),
            },
            _ => {
                log::warn!(
                    "[DSTU::handlers] dstu_get_resource_by_path: unknown resource type: {}",
                    resource_type
                );
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

// ============================================================================
// Tauri 命令：移动到文件夹
// ============================================================================

#[tauri::command]
pub async fn dstu_move_to_folder(
    app: tauri::AppHandle,
    resource_id: String,
    target_folder_id: Option<String>,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<ResourceLocation> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_move_to_folder: resource_id={}, target_folder_id={:?}",
        resource_id,
        target_folder_id
    );

    let resource_type =
        DstuNodeType::from_id_prefix(&resource_id).map(|t| t.to_type_string().to_string()).unwrap_or_else(|| "unknown".to_string());

    let vfs_db_clone = vfs_db.inner().clone();
    let resource_id_for_blocking = resource_id.clone();
    let resource_type_for_blocking = resource_type.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = vfs_db_clone.get_conn_safe().map_err(|e| e.to_string())?;
        let canonical_resource_type = canonical_folder_item_type(&resource_type_for_blocking);

        let old_path: String = conn
            .query_row(
                "SELECT cached_path FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![canonical_resource_type, &resource_id_for_blocking],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| format!("/{}", resource_id_for_blocking));

        let folder_path = match &target_folder_id {
            Some(fid) => {
                VfsFolderRepo::build_folder_path_with_conn(&conn, fid)
                    .unwrap_or_else(|_| String::new())
            }
            None => String::new(),
        };

        let full_path = if folder_path.is_empty() {
            format!("/{}", resource_id_for_blocking)
        } else {
            format!("{}/{}", folder_path, resource_id_for_blocking)
        };
        let now_ms = chrono::Utc::now().timestamp_millis();

        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![canonical_resource_type, &resource_id_for_blocking],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        if existing.is_some() {
            conn.execute(
                "UPDATE folder_items SET folder_id = ?1, cached_path = ?2, updated_at = ?3 WHERE item_id = ?4 AND deleted_at IS NULL",
                rusqlite::params![&target_folder_id, &full_path, now_ms, &resource_id_for_blocking],
            )
            .map_err(|e| e.to_string())?;
        } else {
            let item_id = format!("fi_{}", nanoid::nanoid!(10));
            conn.execute(
                r#"
                INSERT INTO folder_items (id, folder_id, item_type, item_id, sort_order, cached_path, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)
                "#,
                rusqlite::params![
                    &item_id, &target_folder_id, canonical_resource_type,
                    &resource_id_for_blocking, &full_path, now_ms, now_ms
                ],
            )
            .map_err(|e| e.to_string())?;
        }

        Ok::<(ResourceLocation, String), String>((ResourceLocation {
            id: resource_id_for_blocking,
            resource_type: resource_type_for_blocking,
            folder_id: target_folder_id,
            folder_path,
            full_path,
            hash: None,
        }, old_path))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    let (location, old_path) = result?;

    let node_type = DstuNodeType::from_str(&resource_type).unwrap_or(DstuNodeType::File);
    let now = chrono::Utc::now().timestamp_millis();
    let node = DstuNode {
        id: location.id.clone(),
        source_id: location.id.clone(),
        name: location.id.clone(),
        path: location.full_path.clone(),
        node_type,
        size: None,
        created_at: now,
        updated_at: now,
        children: None,
        child_count: None,
        resource_id: None,
        resource_hash: None,
        preview_type: None,
        metadata: None,
    };

    emit_watch_event(
        &window,
        DstuWatchEvent::moved(&old_path, &location.full_path, node),
    );

    Ok(location)
}

// ============================================================================
// Tauri 命令：批量移动
// ============================================================================

#[tauri::command]
pub async fn dstu_batch_move(
    app: tauri::AppHandle,
    request: BatchMoveRequest,
    window: Window,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<BatchMoveResult> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_batch_move: item_ids={:?}, target_folder_id={:?}",
        request.item_ids,
        request.target_folder_id
    );

    let total_count = request.item_ids.len();
    if total_count == 0 {
        return Ok(BatchMoveResult {
            successes: Vec::new(),
            failed_items: Vec::new(),
            total_count: 0,
        });
    }

    let vfs_db_clone = vfs_db.inner().clone();
    let item_ids = request.item_ids.clone();
    let target_folder_id = request.target_folder_id.clone();

    let (move_results, failed_items): (
        Vec<(ResourceLocation, String, String)>,
        Vec<FailedMoveItem>,
    ) = tokio::task::spawn_blocking(move || {
        let conn = vfs_db_clone.get_conn_safe().map_err(|e| e.to_string())?;

        let folder_path = match &target_folder_id {
            Some(fid) => VfsFolderRepo::build_folder_path_with_conn(&conn, fid)
                .unwrap_or_else(|_| String::new()),
            None => String::new(),
        };

        let mut successes = Vec::with_capacity(item_ids.len());
        let mut failures: Vec<FailedMoveItem> = Vec::new();

        for resource_id in &item_ids {
            match move_single_item(&conn, resource_id, &target_folder_id, &folder_path) {
                Ok((location, old_path, resource_type)) => {
                    successes.push((location, old_path, resource_type));
                }
                Err(err_msg) => {
                    log::warn!(
                        "[DSTU::handlers] dstu_batch_move: 移动失败 item_id={}, error={}",
                        resource_id,
                        err_msg
                    );
                    failures.push(FailedMoveItem {
                        item_id: resource_id.clone(),
                        error: err_msg.to_string(),
                    });
                }
            }
        }

        Ok::<_, String>((successes, failures))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    let successes: Vec<ResourceLocation> = move_results
        .into_iter()
        .map(|(location, old_path, resource_type)| {
            let node_type = DstuNodeType::from_str(&resource_type).unwrap_or(DstuNodeType::File);
            let now = chrono::Utc::now().timestamp_millis();
            let node = DstuNode {
                id: location.id.clone(),
                source_id: location.id.clone(),
                name: location.id.clone(),
                path: location.full_path.clone(),
                node_type,
                size: None,
                created_at: now,
                updated_at: now,
                children: None,
                child_count: None,
                resource_id: None,
                resource_hash: None,
                preview_type: None,
                metadata: None,
            };

            emit_watch_event(
                &window,
                DstuWatchEvent::moved(&old_path, &location.full_path, node),
            );

            location
        })
        .collect();

    if failed_items.is_empty() {
        log::info!(
            "[DSTU::handlers] dstu_batch_move: SUCCESS - 移动 {} 项资源",
            successes.len()
        );
    } else {
        log::warn!(
            "[DSTU::handlers] dstu_batch_move: 部分完成 - 成功 {}, 失败 {}",
            successes.len(),
            failed_items.len(),
        );
    }

    Ok(BatchMoveResult {
        successes,
        failed_items,
        total_count,
    })
}

fn move_single_item(
    conn: &rusqlite::Connection,
    resource_id: &str,
    target_folder_id: &Option<String>,
    folder_path: &str,
) -> std::result::Result<(ResourceLocation, String, String), String> {
    let resource_type =
        DstuNodeType::from_id_prefix(resource_id).map(|t| t.to_type_string().to_string()).unwrap_or_else(|| "unknown".to_string());
    let canonical_resource_type = canonical_folder_item_type(&resource_type);

    let old_path: String = conn
        .query_row(
            "SELECT cached_path FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![canonical_resource_type, resource_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询旧路径失败 ({}): {}", resource_id, e))?
        .unwrap_or_else(|| format!("/{}", resource_id));

    let full_path = if folder_path.is_empty() {
        format!("/{}", resource_id)
    } else {
        format!("{}/{}", folder_path, resource_id)
    };
    let now_ms = chrono::Utc::now().timestamp_millis();

    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![canonical_resource_type, resource_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询现有记录失败 ({}): {}", resource_id, e))?;

    if existing.is_some() {
        conn.execute(
            "UPDATE folder_items SET folder_id = ?1, cached_path = ?2, updated_at = ?3 WHERE item_type = ?4 AND item_id = ?5 AND deleted_at IS NULL",
            rusqlite::params![target_folder_id, &full_path, now_ms, canonical_resource_type, resource_id],
        )
        .map_err(|e| format!("更新 folder_items 失败 ({}): {}", resource_id, e))?;
    } else {
        let item_id = format!("fi_{}", nanoid::nanoid!(10));
        conn.execute(
            r#"
            INSERT INTO folder_items (id, folder_id, item_type, item_id, sort_order, cached_path, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)
            "#,
            rusqlite::params![&item_id, target_folder_id, canonical_resource_type, resource_id, &full_path, now_ms, now_ms],
        )
        .map_err(|e| format!("插入 folder_items 失败 ({}): {}", resource_id, e))?;
    }

    let location = ResourceLocation {
        id: resource_id.to_string(),
        resource_type: resource_type.clone(),
        folder_id: target_folder_id.clone(),
        folder_path: folder_path.to_string(),
        full_path,
        hash: None,
    };

    Ok((location, old_path, resource_type))
}

// ============================================================================
// Tauri 命令：路径缓存
// ============================================================================

#[tauri::command]
pub async fn dstu_refresh_path_cache(
    app: tauri::AppHandle,
    resource_id: Option<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<usize> {
    // [写门-接线] 同步写门检查: 同步 apply 期间 (写门被占) → SyncInProgress (可重试)。
    crate::dstu::write_gate::check_dstu_write_gate(&app.state::<crate::commands::AppState>())?;
    log::info!(
        "[DSTU::handlers] dstu_refresh_path_cache: resource_id={:?}",
        resource_id
    );

    let vfs_db_clone = vfs_db.inner().clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = vfs_db_clone.get_conn_safe().map_err(|e| e.to_string())?;

        let items: Vec<(String, Option<String>, String)> = if let Some(ref rid) = resource_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id, folder_id, item_id FROM folder_items WHERE item_type = ?1 AND item_id = ?2 AND deleted_at IS NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows: Vec<_> = stmt
                .query_map(
                    rusqlite::params![
                        canonical_folder_item_type(
                            &DstuNodeType::from_id_prefix(rid)
                                .map(|t| t.to_type_string().to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ),
                        rid,
                    ],
                    |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                    },
                )
                .map_err(|e| e.to_string())?
                .filter_map(log_and_skip_err)
                .collect();
            rows
        } else {
            let mut stmt = conn
                .prepare("SELECT id, folder_id, item_id FROM folder_items WHERE deleted_at IS NULL")
                .map_err(|e| e.to_string())?;
            let rows: Vec<_> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .filter_map(log_and_skip_err)
                .collect();
            rows
        };

        let mut updated_count = 0;

        for (item_row_id, folder_id, item_id) in items {
            let folder_path = match &folder_id {
                Some(fid) => VfsFolderRepo::build_folder_path_with_conn(&conn, fid)
                    .unwrap_or_else(|_| String::new()),
                None => String::new(),
            };

            let full_path = if folder_path.is_empty() {
                format!("/{}", item_id)
            } else {
                format!("{}/{}", folder_path, item_id)
            };

            conn.execute(
                "UPDATE folder_items SET cached_path = ?1 WHERE id = ?2",
                rusqlite::params![&full_path, &item_row_id],
            )
            .map_err(|e| e.to_string())?;

            updated_count += 1;
        }

        log::info!(
            "[DSTU::handlers] dstu_refresh_path_cache: SUCCESS - updated {} entries",
            updated_count
        );
        Ok::<usize, DstuError>(updated_count)
    })
    .await
    .map_err(|e| DstuError::from(format!("Task join error: {}", e)))?;

    result.map_err(DstuError::from)
}

// ============================================================================
// Tauri 命令：根据 ID 获取路径
// ============================================================================

#[tauri::command]
pub async fn dstu_get_path_by_id(
    resource_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
) -> DstuResult<String> {
    log::info!(
        "[DSTU::handlers] dstu_get_path_by_id: resource_id={}",
        resource_id
    );

    let path = crate::vfs::ref_handlers::get_resource_path_internal(&vfs_db, &resource_id)
        .map_err(|e| e.to_string())?;

    log::info!(
        "[DSTU::handlers] dstu_get_path_by_id: SUCCESS - path={}",
        path
    );
    Ok(path)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dstu::handler_utils::{create_type_folder, generate_resource_id};

    #[test]
    fn test_generate_resource_id() {
        let id = generate_resource_id(&DstuNodeType::Note);
        assert!(id.starts_with("note_"));
        assert_eq!(id.len(), 15);

        let id = generate_resource_id(&DstuNodeType::Textbook);
        assert!(id.starts_with("tb_"));

        let id = generate_resource_id(&DstuNodeType::Translation);
        assert!(id.starts_with("tr_"));
    }

    #[test]
    fn test_create_type_folder() {
        let folder = create_type_folder(DstuNodeType::Note);
        assert_eq!(folder.node_type, DstuNodeType::Folder);
        assert_eq!(folder.name, "笔记");
        assert_eq!(folder.path, "/notes");

        let folder = create_type_folder(DstuNodeType::Translation);
        assert_eq!(folder.path, "/translations");
    }

    #[test]
    fn test_simple_path_format() {
        let resource_type = "notes";
        let id = "note_abc123";

        let simple_path = format!("/{}", id);
        assert_eq!(simple_path, "/note_abc123");
    }

    #[test]
    fn test_build_simple_resource_path() {
        let path = build_simple_resource_path("note_123");
        assert_eq!(path, "/note_123");

        let path2 = build_simple_resource_path("tr_456");
        assert_eq!(path2, "/tr_456");
    }
}
