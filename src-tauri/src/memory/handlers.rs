use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{info, warn};

use crate::llm_manager::LLMManager;
use crate::vfs::database::VfsDatabase;
use crate::vfs::indexing::VfsFullIndexingService;
use crate::vfs::lance_store::VfsLanceStore;

use super::audit_log::{self, MemoryAuditLogItem};
use super::error::{MemoryError, MemoryResult};
use super::service::{
    LearningSessionContext, MemoryConfigOutput, MemoryListItem, MemorySearchResult, MemoryService,
    MemoryWriteOutput, SmartWriteOutput, WriteMode,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperationResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReadOutput {
    pub note_id: String,
    pub title: String,
    pub content: String,
    pub folder_path: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBatchWriteItemInput {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub folder_path: Option<String>,
    #[serde(default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub memory_purpose: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBatchWriteItemResult {
    pub title: String,
    #[serde(flatten)]
    pub output: SmartWriteOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBatchWriteOutput {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub filtered: usize,
    pub results: Vec<MemoryBatchWriteItemResult>,
}

fn get_memory_service(
    vfs_db: &Arc<VfsDatabase>,
    lance_store: &Arc<VfsLanceStore>,
    llm_manager: &Arc<LLMManager>,
) -> MemoryService {
    MemoryService::new(vfs_db.clone(), lance_store.clone(), llm_manager.clone())
}

fn parse_memory_type(memory_type: Option<&str>) -> Result<super::service::MemoryType, String> {
    match memory_type.map(|s| s.trim().to_lowercase()) {
        Some(s) if s == "fact" => Ok(super::service::MemoryType::Fact),
        Some(s) if s == "study" => Ok(super::service::MemoryType::Study),
        Some(s) if s == "note" => Ok(super::service::MemoryType::Note),
        Some(s) => Err(format!(
            "Invalid memory_type '{}', expected one of: fact, study, note",
            s
        )),
        None => Ok(super::service::MemoryType::Fact),
    }
}

fn parse_memory_purpose(
    memory_purpose: Option<&str>,
) -> Result<Option<super::service::MemoryPurpose>, String> {
    match memory_purpose.map(|s| s.trim().to_lowercase()) {
        Some(s) if s == "internalized" => Ok(Some(super::service::MemoryPurpose::Internalized)),
        Some(s) if s == "memorized" => Ok(Some(super::service::MemoryPurpose::Memorized)),
        Some(s) if s == "supplementary" => Ok(Some(super::service::MemoryPurpose::Supplementary)),
        Some(s) if s == "systemic" => Ok(Some(super::service::MemoryPurpose::Systemic)),
        Some(s) => Err(format!(
            "Invalid memory_purpose '{}', expected one of: internalized, memorized, supplementary, systemic",
            s
        )),
        None => Ok(None),
    }
}

/// 写入后触发单资源索引，保证 write-then-search SLA。
/// 索引成功后标记为 indexed，防止批量 worker 重复处理。
fn trigger_immediate_index(
    vfs_db: Arc<VfsDatabase>,
    llm_manager: Arc<LLMManager>,
    lance_store: Arc<VfsLanceStore>,
    resource_id: String,
) {
    crate::background_tasks::BACKGROUND_TASKS.spawn(async move {
        let db_ref = vfs_db.clone();
        let indexing_service = match VfsFullIndexingService::new(vfs_db, llm_manager, lance_store) {
            Ok(svc) => svc,
            Err(e) => {
                warn!(
                    "[Memory] Failed to create indexing service for immediate index of {}: {}",
                    resource_id, e
                );
                return;
            }
        };

        match indexing_service
            .index_resource(&resource_id, None, None)
            .await
        {
            Ok((chunk_count, dim)) => {
                if let Err(e) = crate::vfs::repos::embedding_repo::VfsIndexStateRepo::mark_indexed(
                    &db_ref,
                    &resource_id,
                    &format!("mem_handler_{}", chrono::Utc::now().timestamp_millis()),
                ) {
                    warn!(
                        "[Memory] Failed to mark indexed after immediate indexing: {}",
                        e
                    );
                }
                info!(
                    "[Memory] Immediate index completed for resource {} ({} chunks, dim={})",
                    resource_id, chunk_count, dim
                );
            }
            Err(e) => {
                warn!(
                    "[Memory] Immediate index failed for resource {} (will retry in next batch): {}",
                    resource_id, e
                );
            }
        }
    });
}

#[tauri::command]
pub async fn memory_get_config(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<MemoryConfigOutput> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.get_config()?)
}

#[tauri::command]
pub async fn memory_set_root_folder(
    folder_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.set_root_folder(&folder_id)?)
}

#[tauri::command]
pub async fn memory_set_privacy_mode(
    enabled: bool,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.set_privacy_mode(enabled)?)
}

#[tauri::command]
pub async fn memory_create_root_folder(
    title: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<String> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.create_root_folder(&title)?)
}

#[tauri::command]
pub async fn memory_get_or_create_root_folder(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<String> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.get_or_create_root_folder()?)
}

#[tauri::command]
pub async fn memory_search(
    query: String,
    top_k: Option<usize>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemorySearchResult>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let k = top_k.unwrap_or(5).clamp(1, 100);
    Ok(service.search_with_rerank(&query, k, false).await?)
}

#[tauri::command]
pub async fn memory_read(
    note_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Option<MemoryReadOutput>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);

    match service.read(&note_id)? {
        Some((note, content)) => {
            let folder_path = service
                .get_note_folder_path(&note_id)
                ?;

            Ok(Some(MemoryReadOutput {
                note_id: note.id,
                title: note.title,
                content,
                folder_path,
                updated_at: note.updated_at,
            }))
        }
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn memory_write(
    note_id: Option<String>,
    folder_path: Option<String>,
    title: String,
    content: String,
    mode: Option<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<MemoryWriteOutput> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let write_mode = mode
        .map(|m| WriteMode::from_str(&m))
        .unwrap_or(WriteMode::Create);

    let result = if let Some(target_note_id) = note_id {
        match write_mode {
            WriteMode::Append => {
                let current = service
                    .read(&target_note_id)
                    ?
                    .map(|(_, existing)| existing)
                    .unwrap_or_default();
                let final_content = if current.is_empty() {
                    content.clone()
                } else {
                    format!("{}\n\n{}", current, content)
                };
                service
                    .update_by_id(&target_note_id, Some(&title), Some(&final_content))
                    ?
            }
            _ => service
                .update_by_id(&target_note_id, Some(&title), Some(&content))
                ?,
        }
    } else {
        service
            .write(folder_path.as_deref(), &title, &content, write_mode)
            ?
    };

    // ★ P2-2 修复：写入后立即触发索引，保证 write-then-search SLA
    trigger_immediate_index(
        Arc::clone(vfs_db.inner()),
        Arc::clone(llm_manager.inner()),
        Arc::clone(lance_store.inner()),
        result.resource_id.clone(),
    );

    service.spawn_post_write_maintenance();

    Ok(result)
}

#[tauri::command]
pub async fn memory_list(
    folder_path: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryListItem>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let safe_limit = limit.unwrap_or(100).clamp(1, 10000);
    Ok(service.list(folder_path.as_deref(), safe_limit, offset.unwrap_or(0))?)
}

#[tauri::command]
pub async fn memory_get_tree(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Option<crate::vfs::types::FolderTreeNode>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.get_tree()?)
}

/// 添加记忆关联（双向）
#[tauri::command]
pub async fn memory_add_relation(
    note_id_a: String,
    note_id_b: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.add_relation(&note_id_a, &note_id_b)?)
}

/// 移除记忆关联（双向）
#[tauri::command]
pub async fn memory_remove_relation(
    note_id_a: String,
    note_id_b: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.remove_relation(&note_id_a, &note_id_b)?)
}

/// 获取关联记忆 ID 列表
#[tauri::command]
pub async fn memory_get_related(
    note_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<String>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.get_related_ids(&note_id)?)
}

/// 更新记忆标签
#[tauri::command]
pub async fn memory_update_tags(
    note_id: String,
    tags: Vec<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.update_tags(&note_id, tags)?)
}

/// 获取记忆标签
#[tauri::command]
pub async fn memory_get_tags(
    note_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<String>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.get_tags(&note_id)?)
}

/// 批量删除记忆
#[tauri::command]
pub async fn memory_batch_delete(
    note_ids: Vec<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<BatchOperationResult> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let total = note_ids.len();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for note_id in &note_ids {
        match service.delete(note_id).await {
            Ok(()) => succeeded += 1,
            Err(e) => {
                failed += 1;
                if errors.len() < 5 {
                    errors.push(format!("{}: {}", note_id, e));
                }
            }
        }
    }

    if succeeded > 0 {
        service.spawn_post_write_maintenance();
    }

    Ok(BatchOperationResult {
        total,
        succeeded,
        failed,
        errors,
    })
}

/// 批量移动记忆到指定文件夹
#[tauri::command]
pub async fn memory_batch_move(
    note_ids: Vec<String>,
    target_folder_path: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<BatchOperationResult> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let total = note_ids.len();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for note_id in &note_ids {
        match service.move_to_folder(note_id, &target_folder_path) {
            Ok(()) => succeeded += 1,
            Err(e) => {
                failed += 1;
                if errors.len() < 5 {
                    errors.push(format!("{}: {}", note_id, e));
                }
            }
        }
    }

    Ok(BatchOperationResult {
        total,
        succeeded,
        failed,
        errors,
    })
}

/// 移动记忆到指定文件夹路径
#[tauri::command]
pub async fn memory_move_to_folder(
    note_id: String,
    target_folder_path: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    Ok(service.move_to_folder(&note_id, &target_folder_path)?)
}

// ★ 修复风险2：按 note_id 更新记忆
#[tauri::command]
pub async fn memory_update_by_id(
    note_id: String,
    title: Option<String>,
    content: Option<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<MemoryWriteOutput> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let result = service
        .update_by_id(&note_id, title.as_deref(), content.as_deref())
        ?;

    // ★ P2-2 修复：更新后立即触发索引，保证 write-then-search SLA
    trigger_immediate_index(
        Arc::clone(vfs_db.inner()),
        Arc::clone(llm_manager.inner()),
        Arc::clone(lance_store.inner()),
        result.resource_id.clone(),
    );

    service.spawn_post_write_maintenance();

    Ok(result)
}

// ★ 修复风险3：删除记忆
#[tauri::command]
pub async fn memory_delete(
    note_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.delete(&note_id).await?;
    service.spawn_post_write_maintenance();
    Ok(())
}

#[tauri::command]
pub async fn memory_set_auto_create_subfolders(
    enabled: bool,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let cfg = super::config::MemoryConfig::new(service.storage_ref().clone());
    Ok(cfg.set_auto_create_subfolders(enabled)?)
}

#[tauri::command]
pub async fn memory_set_default_category(
    category: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let cfg = super::config::MemoryConfig::new(service.storage_ref().clone());
    Ok(cfg.set_default_category(&category)?)
}

#[tauri::command]
pub async fn memory_set_auto_extract_frequency(
    frequency: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let freq = match frequency.trim().to_lowercase().as_str() {
        "off" => super::config::AutoExtractFrequency::Off,
        "balanced" => super::config::AutoExtractFrequency::Balanced,
        "aggressive" => super::config::AutoExtractFrequency::Aggressive,
        other => {
            return Err(MemoryError::Other(format!(
                "Invalid auto extract frequency '{}', expected one of: off, balanced, aggressive",
                other
            )));
        }
    };
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let cfg = super::config::MemoryConfig::new(service.storage_ref().clone());
    Ok(cfg.set_auto_extract_frequency(freq)?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryExportItem {
    pub title: String,
    pub content: String,
    pub folder: String,
    pub updated_at: String,
}

#[tauri::command]
pub async fn memory_export_all(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryExportItem>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let items = service.list(None, 500, 0)?;

    let mut results = Vec::with_capacity(items.len());
    for item in &items {
        let content = service
            .read(&item.id)
            ?
            .map(|(_, c)| c)
            .unwrap_or_default();
        results.push(MemoryExportItem {
            title: item.title.clone(),
            content,
            folder: item.folder_path.clone(),
            updated_at: item.updated_at.clone(),
        });
    }
    Ok(results)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryProfileSection {
    pub category: String,
    pub content: String,
}

#[tauri::command]
pub async fn memory_get_profile(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryProfileSection>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let root_id = match service.get_root_folder_id()? {
        Some(id) => id,
        None => return Ok(vec![]),
    };

    let cat_mgr = super::category_manager::MemoryCategoryManager::new(
        service.storage_ref().clone(),
        llm_manager.inner().clone(),
    );

    let categories = cat_mgr
        .load_all_category_summaries(&root_id)
        ?;

    if !categories.is_empty() {
        return Ok(categories
            .into_iter()
            .map(|(cat, content)| MemoryProfileSection {
                category: cat,
                content,
            })
            .collect());
    }

    match service.get_profile_summary()? {
        Some(profile) => Ok(vec![MemoryProfileSection {
            category: "画像".to_string(),
            content: profile,
        }]),
        None => Ok(vec![]),
    }
}

#[tauri::command]
pub async fn memory_write_smart(
    folder_path: Option<String>,
    title: String,
    content: String,
    memory_type: Option<String>,
    memory_purpose: Option<String>,
    idempotency_key: Option<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<SmartWriteOutput> {
    if title.trim().is_empty() {
        return Err(MemoryError::Validation("标题不能为空".to_string()));
    }
    if content.trim().is_empty() {
        return Err(MemoryError::Validation("内容不能为空".to_string()));
    }

    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let mem_type = parse_memory_type(memory_type.as_deref())?;
    let purpose = parse_memory_purpose(memory_purpose.as_deref())?;
    let result = service
        .write_smart_with_source(
            folder_path.as_deref(),
            &title,
            &content,
            super::audit_log::MemoryOpSource::Handler,
            None,
            mem_type,
            purpose,
            idempotency_key.as_deref(),
        )
        .await
        ?;

    if result.event != "NONE" && result.event != "FILTERED" {
        service.spawn_post_write_maintenance();
    }

    Ok(result)
}

/// 写入学习承诺 (L1 层)
#[tauri::command]
pub async fn memory_write_promise(
    folder_path: Option<String>,
    title: String,
    content: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<SmartWriteOutput> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let result = service
        .write_promise(folder_path.as_deref(), &title, &content)
        .await?;
    if result.event != "NONE" && result.event != "FILTERED" {
        service.spawn_post_write_maintenance();
    }
    Ok(result)
}

/// 完成一个学习承诺
#[tauri::command]
pub fn memory_complete_promise(
    note_id: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.complete_promise(&note_id)
}

/// 获取所有未完成的学习承诺
#[tauri::command]
pub fn memory_get_pending_promises(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryListItem>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.get_pending_promises()
}

/// 写入学习偏好 (L2 层，需证据绑定)
#[tauri::command]
pub async fn memory_write_preference(
    folder_path: Option<String>,
    title: String,
    content: String,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<SmartWriteOutput> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let result = service
        .write_preference(folder_path.as_deref(), &title, &content)
        .await?;
    if result.event != "NONE" && result.event != "FILTERED" {
        service.spawn_post_write_maintenance();
    }
    Ok(result)
}

/// 获取所有学习偏好
#[tauri::command]
pub fn memory_get_learning_preferences(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryListItem>> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.get_learning_preferences()
}

/// 获取会话工作上下文 (L4 层)
#[tauri::command]
pub fn memory_get_session_context(
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<LearningSessionContext> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.get_session_context()
}

/// 更新会话工作上下文 (L4 层)
#[tauri::command]
pub fn memory_update_session_context(
    ctx: LearningSessionContext,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<()> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    service.update_session_context(&ctx)
}

#[tauri::command]
pub async fn memory_write_batch(
    items: Vec<MemoryBatchWriteItemInput>,
    default_folder_path: Option<String>,
    default_memory_type: Option<String>,
    default_memory_purpose: Option<String>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<MemoryBatchWriteOutput> {
    if items.is_empty() {
        return Ok(MemoryBatchWriteOutput {
            total: 0,
            succeeded: 0,
            failed: 0,
            added: 0,
            updated: 0,
            skipped: 0,
            filtered: 0,
            results: vec![],
        });
    }

    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let default_type = default_memory_type
        .as_deref()
        .map(|s| parse_memory_type(Some(s)))
        .transpose()?
        .unwrap_or(super::service::MemoryType::Study);
    let default_purpose = parse_memory_purpose(default_memory_purpose.as_deref())?;

    let mut results = Vec::with_capacity(items.len());
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut filtered = 0usize;
    let mut resource_ids = Vec::new();

    for item in items {
        let mem_type = item
            .memory_type
            .as_deref()
            .map(|s| parse_memory_type(Some(s)))
            .transpose()?
            .unwrap_or(default_type);
        let purpose = parse_memory_purpose(item.memory_purpose.as_deref())?.or(default_purpose);
        let output = match mem_type {
            super::service::MemoryType::Fact => service
                .write_smart_with_source(
                    item.folder_path
                        .as_deref()
                        .or(default_folder_path.as_deref()),
                    &item.title,
                    &item.content,
                    super::audit_log::MemoryOpSource::Handler,
                    None,
                    mem_type,
                    purpose,
                    item.idempotency_key.as_deref(),
                )
                .await
                ?,
            _ => service
                .write_explicit_memory(
                    item.folder_path
                        .as_deref()
                        .or(default_folder_path.as_deref()),
                    &item.title,
                    &item.content,
                    mem_type,
                    purpose,
                )
                ?,
        };

        match output.event.as_str() {
            "ADD" => added += 1,
            "UPDATE" | "APPEND" | "DELETE" => updated += 1,
            "FILTERED" => filtered += 1,
            _ => skipped += 1,
        }
        // ★ A8#2 双索引修复: Fact 已在 write_smart 内联 index_immediately,
        // 只有非 Fact(write_explicit_memory 无内联索引)需要 handler 层再触发索引
        if let Some(resource_id) = &output.resource_id {
            if mem_type != super::service::MemoryType::Fact {
                resource_ids.push(resource_id.clone());
            }
        }
        results.push(MemoryBatchWriteItemResult {
            title: item.title,
            output,
        });
    }

    if !resource_ids.is_empty() {
        for resource_id in resource_ids {
            trigger_immediate_index(
                Arc::clone(vfs_db.inner()),
                Arc::clone(llm_manager.inner()),
                Arc::clone(lance_store.inner()),
                resource_id,
            );
        }
        service.spawn_post_write_maintenance();
    }

    let succeeded = added + updated;
    Ok(MemoryBatchWriteOutput {
        total: results.len(),
        succeeded,
        failed: filtered,
        added,
        updated,
        skipped,
        filtered,
        results,
    })
}

#[tauri::command]
pub async fn memory_get_audit_logs(
    limit: Option<u32>,
    offset: Option<u32>,
    source_filter: Option<String>,
    operation_filter: Option<String>,
    success_filter: Option<bool>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<Vec<MemoryAuditLogItem>> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let offset = offset.unwrap_or(0);
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let conn = service.storage_ref().conn()?;
    Ok(audit_log::query_audit_logs(
        &conn,
        limit,
        offset,
        source_filter.as_deref(),
        operation_filter.as_deref(),
        success_filter,
    )?)
}

/// 将记忆导出为 ChatAnki 卡片格式的文档内容
///
/// 筛选记忆后，格式化为结构化文本，返回给前端。
/// 前端可将此文本传入 chatanki_run 工具触发制卡。
#[tauri::command]
pub async fn memory_to_anki_document(
    folder_path: Option<String>,
    purpose_filter: Option<String>,
    limit: Option<u32>,
    vfs_db: State<'_, Arc<VfsDatabase>>,
    lance_store: State<'_, Arc<VfsLanceStore>>,
    llm_manager: State<'_, Arc<LLMManager>>,
) -> MemoryResult<MemoryAnkiDocument> {
    let service = get_memory_service(&vfs_db, &lance_store, &llm_manager);
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let items = service
        .list(folder_path.as_deref(), limit, 0)
        ?;

    let purpose = purpose_filter.as_deref();

    let mut lines = Vec::new();
    let mut count = 0usize;

    for item in &items {
        if item.title.starts_with("__") {
            continue;
        }
        if let Some(p) = purpose {
            if item.memory_purpose != p {
                continue;
            }
        }

        let content = service
            .read(&item.id)
            ?
            .map(|(_, c)| c)
            .unwrap_or_default();

        let text = if content.is_empty() {
            &item.title
        } else {
            &content
        };

        lines.push(format!("## {}\n\n{}\n\n---\n", item.title, text));
        count += 1;
    }

    let document_content = if lines.is_empty() {
        String::new()
    } else {
        format!(
            "# 用户记忆知识卡片\n\n以下是从用户记忆库中提取的 {} 条记忆，请为每条生成对应的 Anki 卡片。\n\n{}",
            count,
            lines.join("\n")
        )
    };

    Ok(MemoryAnkiDocument {
        document_content,
        memory_count: count,
        document_name: format!("记忆卡片_{}", chrono::Local::now().format("%Y%m%d")),
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAnkiDocument {
    pub document_content: String,
    pub memory_count: usize,
    pub document_name: String,
}
