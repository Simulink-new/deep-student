//! DSTU 访达协议层类型定义
//!
//! 本模块定义 DSTU 协议的核心类型，包括：
//! - `DstuNodeType`: 节点类型（`ResourceKind` 类型别名，已收编）
//! - `DstuNode`: 资源节点
//! - `DstuListOptions`: 列表选项
//! - `DstuCreateOptions`: 创建选项
//!
//! ## 路径规范
//! ```text
//! 路径格式：/{folder_path}/{resource_id}
//!
//! 示例：
//! - /高考复习/函数/note_abc123   → 在"高考复习/函数"文件夹下的笔记
//! - /我的教材/tb_xyz789          → 在"我的教材"文件夹下的教材
//! - /exam_sheet_001              → 根目录下的题目集（无文件夹）
//! - /                            → 根目录
//! - /@trash                      → 回收站（虚拟路径）
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

// DstuError 和 DstuResult 由 handlers 模块使用（Prompt 5）
// use super::error::{DstuError, DstuResult};

// ============================================================================
// 节点类型（收编自 ResourceKind）
// ============================================================================

/// DSTU 节点类型（`ResourceKind` 的类型别名）
///
/// `DstuNodeType` 已收编为 `crate::vfs::resource_kind::ResourceKind` 的类型别名
/// （"三个资源枚举统一"的第三步），满足：
/// - **唯一事实源**: `ResourceKind` (vfs/resource_kind.rs)，11 个变体
///   （原 9 变体 + Card/Folder，无变体被删除）
/// - **序列化契约不变**: serde lowercase 11 个小写字符串，由 `ResourceKind` 的
///   `#[serde(rename_all = "lowercase")]` 保证，且新增 Card → "card" / Folder → "folder"
/// - **方法由 `ResourceKind` 提供**: `from_str` / `to_path_segment` /
///   `to_type_string` / `from_id_prefix` / `display_name_key` / `preview_type` /
///   `Display`，均通过别名直接可用
///
/// 协议层差异说明：
/// - `display_name_key()` / `preview_type()` 已原样迁移到 `ResourceKind`，
///   并新增 Card 臂（"dstu:types.card" / "card"）
/// - `from_id_prefix` 行为变化为**设计内变更**：`img_` 前缀由 File 修复为 Image
pub type DstuNodeType = crate::vfs::ResourceKind;

// ============================================================================
// 资源节点
// ============================================================================

/// DSTU 资源节点
///
/// 表示 DSTU 文件系统中的一个节点，可以是文件夹或具体资源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DstuNode {
    /// 节点 ID（资源 ID 或虚拟 ID）
    pub id: String,

    /// 完整路径
    ///
    /// 简单路径格式：/{resource_id}，如 "/note_abc123"
    /// 文件夹节点的 path 为其完整层级路径，如 "/高考复习/函数"
    pub path: String,

    /// 显示名称
    pub name: String,

    /// 节点类型
    #[serde(rename = "type")]
    pub node_type: DstuNodeType,

    /// 内容大小（字节）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// 创建时间（毫秒时间戳）
    pub created_at: i64,

    /// 更新时间（毫秒时间戳）
    pub updated_at: i64,

    /// 子节点（仅文件夹有效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DstuNode>>,

    /// 子节点数量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_count: Option<u32>,

    /// VFS 资源 ID（资源节点有效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,

    /// 稳定的业务 ID（与 id 相同，但语义明确，用于引用模式）
    pub source_id: String,

    /// 资源内容 hash（用于校验引用有效性，资源未同步时可为空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_hash: Option<String>,

    /// 预览类型（markdown | pdf | card | exam | image | docx | xlsx | pptx | text | mindmap | none）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_type: Option<String>,

    /// 扩展元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl DstuNode {
    /// 创建文件夹节点
    pub fn folder(id: impl Into<String>, path: impl Into<String>, name: impl Into<String>) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        let id_str = id.into();
        Self {
            source_id: id_str.clone(),
            id: id_str,
            path: path.into(),
            name: name.into(),
            node_type: DstuNodeType::Folder,
            size: None,
            created_at: now,
            updated_at: now,
            children: None,
            child_count: None,
            resource_id: None,
            resource_hash: None,
            preview_type: Some("none".to_string()),
            metadata: None,
        }
    }

    /// 创建资源节点
    pub fn resource(
        id: impl Into<String>,
        path: impl Into<String>,
        name: impl Into<String>,
        node_type: DstuNodeType,
        resource_id: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        let preview_type = node_type.preview_type().to_string();
        let id_str = id.into();
        Self {
            source_id: id_str.clone(),
            id: id_str,
            path: path.into(),
            name: name.into(),
            node_type,
            size: None,
            created_at: now,
            updated_at: now,
            children: None,
            child_count: None,
            resource_id: Some(resource_id.into()),
            resource_hash: None,
            preview_type: Some(preview_type),
            metadata: None,
        }
    }

    /// 设置大小
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// 设置时间戳
    pub fn with_timestamps(mut self, created_at: i64, updated_at: i64) -> Self {
        self.created_at = created_at;
        self.updated_at = updated_at;
        self
    }

    /// 设置子节点
    pub fn with_children(mut self, children: Vec<DstuNode>) -> Self {
        self.child_count = Some(children.len() as u32);
        self.children = Some(children);
        self
    }

    /// 设置子节点数量（不含实际子节点）
    pub fn with_child_count(mut self, count: u32) -> Self {
        self.child_count = Some(count);
        self
    }

    /// 设置元数据
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// 设置资源 hash（用于引用有效性校验）
    pub fn with_resource_hash(mut self, hash: impl Into<String>) -> Self {
        self.resource_hash = Some(hash.into());
        self
    }

    /// 设置预览类型（覆盖默认值）
    pub fn with_preview_type(mut self, preview_type: impl Into<String>) -> Self {
        self.preview_type = Some(preview_type.into());
        self
    }
}

// ============================================================================
// 列表选项
// ============================================================================

/// DSTU 列表选项
///
/// 用于 `dstu_list` 命令的查询参数。
///
/// ## 文件夹优先模型
/// - `folder_id`: 指定文件夹 ID，列出该文件夹下的所有资源（混合类型）
/// - `type_filter`: 按类型筛选（智能文件夹），返回的资源路径仍是文件夹路径
///
/// ## 默认行为
/// - 若 `folder_id` 和 `type_filter` 都未指定，返回根目录内容
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DstuListOptions {
    /// 文件夹 ID（文件夹优先模型）
    /// 指定后列出该文件夹下的所有资源（混合类型：笔记+翻译+图片等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,

    /// 类型筛选（智能文件夹模式）
    /// 按类型筛选资源，但返回的 path 仍是文件夹路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_filter: Option<DstuNodeType>,

    /// ★ 收藏筛选 - 仅返回已收藏的资源
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_favorite: Option<bool>,

    /// 是否递归列出子目录
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,

    /// 过滤类型（旧版，保留兼容）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<DstuNodeType>>,

    /// 搜索关键词
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,

    /// 标签过滤（仅笔记类资源）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// 排序字段（name | createdAt | updatedAt）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,

    /// 排序方向（asc | desc）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<String>,

    /// 分页：返回数量限制
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// 分页：偏移量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,

    /// ★ 2026-08-10 笔记三分域（md 区分规则）：仅配合 type_filter=note 生效。
    /// - `"normal"`: 普通笔记 — 排除 OCR 识别页笔记（tags 含 "ocr"）与记忆子树笔记
    /// - `"ocr"`: 仅 OCR 识别页笔记（tags 含 "ocr"，由 pdf_processing_service 创建）
    /// - `None`/其它: 全部笔记（兼容旧行为）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_scope: Option<String>,
}

impl DstuListOptions {
    /// 创建默认选项
    pub fn new() -> Self {
        Self::default()
    }

    // ========== 文件夹优先模型方法 ==========

    /// 设置文件夹 ID（文件夹导航模式）
    pub fn folder_id(mut self, folder_id: impl Into<String>) -> Self {
        self.folder_id = Some(folder_id.into());
        self
    }

    /// 设置类型筛选（智能文件夹模式）
    pub fn type_filter(mut self, type_filter: DstuNodeType) -> Self {
        self.type_filter = Some(type_filter);
        self
    }

    /// 获取文件夹 ID
    pub fn get_folder_id(&self) -> Option<&str> {
        self.folder_id.as_deref()
    }

    /// 获取类型筛选
    pub fn get_type_filter(&self) -> Option<DstuNodeType> {
        self.type_filter
    }

    /// 获取笔记三分域（"normal" | "ocr" | None=全部）
    pub fn get_note_scope(&self) -> Option<&str> {
        self.note_scope.as_deref()
    }

    /// 是否使用文件夹优先模式
    /// folder_id 或 type_filter 有值时，使用新的文件夹优先模式
    pub fn is_folder_first_mode(&self) -> bool {
        self.folder_id.is_some() || self.type_filter.is_some()
    }

    // ========== 原有方法 ==========

    /// 设置递归
    pub fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = Some(recursive);
        self
    }

    /// 设置类型过滤（旧版，保留兼容）
    pub fn types(mut self, types: Vec<DstuNodeType>) -> Self {
        self.types = Some(types);
        self
    }

    /// 设置搜索关键词
    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    /// 设置排序
    pub fn sort(mut self, sort_by: impl Into<String>, sort_order: impl Into<String>) -> Self {
        self.sort_by = Some(sort_by.into());
        self.sort_order = Some(sort_order.into());
        self
    }

    /// 设置分页
    pub fn paginate(mut self, limit: u32, offset: u32) -> Self {
        self.limit = Some(limit);
        self.offset = Some(offset);
        self
    }

    /// 获取 limit，默认 50
    pub fn get_limit(&self) -> u32 {
        self.limit.unwrap_or(50)
    }

    /// 获取 offset，默认 0
    pub fn get_offset(&self) -> u32 {
        self.offset.unwrap_or(0)
    }

    /// 是否递归
    pub fn is_recursive(&self) -> bool {
        self.recursive.unwrap_or(false)
    }

    /// 是否升序
    pub fn is_ascending(&self) -> bool {
        self.sort_order
            .as_deref()
            .map(|s| s == "asc")
            .unwrap_or(true)
    }
}

// ============================================================================
// 创建选项
// ============================================================================

/// DSTU 创建选项
///
/// 用于 `dstu_create` 命令的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DstuCreateOptions {
    /// 节点类型
    #[serde(rename = "type")]
    pub node_type: DstuNodeType,

    /// 显示名称
    pub name: String,

    /// 内容（笔记等文本资源）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// 文件内容（Base64 编码，用于图片/文件附件）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_base64: Option<String>,

    /// 文件数据（Base64 编码，用于教材/文件/图片创建，与 file_base64 同义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,

    /// 目标文件夹 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,

    /// 标签列表（仅笔记类资源）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// 扩展元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl DstuCreateOptions {
    /// 创建笔记创建选项
    pub fn note(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            node_type: DstuNodeType::Note,
            name: name.into(),
            content: Some(content.into()),
            file_base64: None,
            file_data: None,
            folder_id: None,
            tags: None,
            metadata: None,
        }
    }

    /// 创建文件夹创建选项
    pub fn folder(name: impl Into<String>) -> Self {
        Self {
            node_type: DstuNodeType::Folder,
            name: name.into(),
            content: None,
            file_base64: None,
            file_data: None,
            folder_id: None,
            tags: None,
            metadata: None,
        }
    }

    /// 创建文件/图片创建选项
    pub fn file(
        name: impl Into<String>,
        file_base64: impl Into<String>,
        node_type: DstuNodeType,
    ) -> Self {
        Self {
            node_type,
            name: name.into(),
            content: None,
            file_base64: Some(file_base64.into()),
            file_data: None,
            folder_id: None,
            tags: None,
            metadata: None,
        }
    }

    /// 设置元数据
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// 解析文件内容（`file_data` 与 `file_base64` 同义，前端可能只传其中一个）
    pub fn resolved_file_data(&self) -> Option<&str> {
        self.file_data
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| self.file_base64.as_deref().filter(|value| !value.is_empty()))
    }

    /// 解析目标文件夹 ID（顶层 `folder_id` 优先，兼容 metadata.folderId）
    pub fn resolved_folder_id(&self) -> Option<String> {
        if let Some(id) = self.folder_id.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            return Some(id.to_string());
        }
        self.metadata.as_ref().and_then(|metadata| {
            metadata
                .get("folderId")
                .or_else(|| metadata.get("folder_id"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        })
    }
}

// ============================================================================
// 监听事件
// ============================================================================

/// DSTU 监听事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DstuWatchEventType {
    /// 创建
    Created,
    /// 更新
    Updated,
    /// 删除
    Deleted,
    /// 移动
    Moved,
    /// 恢复（从回收站恢复）
    Restored,
    /// 永久删除
    Purged,
}

/// DSTU 监听事件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DstuWatchEvent {
    /// 事件类型
    #[serde(rename = "type")]
    pub event_type: DstuWatchEventType,

    /// 路径
    pub path: String,

    /// 旧路径（移动事件有效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,

    /// 节点（创建/更新事件有效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<DstuNode>,
}

impl DstuWatchEvent {
    /// 创建"已创建"事件
    pub fn created(path: impl Into<String>, node: DstuNode) -> Self {
        Self {
            event_type: DstuWatchEventType::Created,
            path: path.into(),
            old_path: None,
            node: Some(node),
        }
    }

    /// 创建"已更新"事件
    pub fn updated(path: impl Into<String>, node: DstuNode) -> Self {
        Self {
            event_type: DstuWatchEventType::Updated,
            path: path.into(),
            old_path: None,
            node: Some(node),
        }
    }

    /// 创建"已删除"事件
    pub fn deleted(path: impl Into<String>) -> Self {
        Self {
            event_type: DstuWatchEventType::Deleted,
            path: path.into(),
            old_path: None,
            node: None,
        }
    }

    /// 创建"已移动"事件
    pub fn moved(old_path: impl Into<String>, new_path: impl Into<String>, node: DstuNode) -> Self {
        Self {
            event_type: DstuWatchEventType::Moved,
            path: new_path.into(),
            old_path: Some(old_path.into()),
            node: Some(node),
        }
    }

    /// 创建"已恢复"事件（从回收站恢复）
    pub fn restored(path: impl Into<String>, node: Option<DstuNode>) -> Self {
        Self {
            event_type: DstuWatchEventType::Restored,
            path: path.into(),
            old_path: None,
            node,
        }
    }

    /// 创建"已永久删除"事件
    pub fn purged(path: impl Into<String>) -> Self {
        Self {
            event_type: DstuWatchEventType::Purged,
            path: path.into(),
            old_path: None,
            node: None,
        }
    }
}

// ============================================================================
// 契约 C: 真实路径架构类型定义（文档 28）
// ============================================================================

/// C1: 路径解析结果（真实文件夹路径）
///
/// 已迁移至 `path_types::ParsedPath`。此类型别名保留用于向后兼容。
/// 新代码应直接使用 `path_types::ParsedPath`。
pub type DstuParsedPath = crate::dstu::path_types::ParsedPath;

/// C2: 路径缓存条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathCacheEntry {
    /// 资源类型
    pub item_type: String,
    /// 资源 ID
    pub item_id: String,
    /// 完整路径
    pub full_path: String,
    /// 文件夹路径
    pub folder_path: String,
    /// 缓存更新时间
    pub updated_at: String,
}

/// C3: 资源定位信息
///
/// 已收编至 `crate::vfs::types::ResourceLocation`（"资源类型统一"第五步）。
/// 唯一事实源为 vfs 版（vfs/types.rs），此处 re-export 以保持
/// `dstu::types::ResourceLocation` 路径兼容。
pub use crate::vfs::types::ResourceLocation;

/// C4: 批量移动请求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchMoveRequest {
    /// 要移动的资源 ID 列表
    pub item_ids: Vec<String>,
    /// 目标文件夹（None = 根目录）
    pub target_folder_id: Option<String>,
}

/// C4b: 批量移动失败项
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedMoveItem {
    /// 失败的资源 ID
    pub item_id: String,
    /// 失败原因
    pub error: String,
}

/// C4c: 批量移动结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchMoveResult {
    /// 成功移动的资源定位信息
    pub successes: Vec<ResourceLocation>,
    /// 移动失败的项及原因
    pub failed_items: Vec<FailedMoveItem>,
    /// 移动的总数量
    pub total_count: usize,
}

/// C5: Subject 迁移状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectMigrationStatus {
    /// 总资源数
    pub total_resources: usize,
    /// 已迁移数
    pub migrated_count: usize,
    /// 待迁移数
    pub pending_count: usize,
    /// 自动创建的科目文件夹
    pub auto_created_folders: Vec<String>,
}

impl Default for SubjectMigrationStatus {
    fn default() -> Self {
        Self {
            total_resources: 0,
            migrated_count: 0,
            pending_count: 0,
            auto_created_folders: Vec::new(),
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_type_serialization() {
        // 验证 DstuNodeType 序列化为小写字符串
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Folder).unwrap(),
            "\"folder\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Note).unwrap(),
            "\"note\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Textbook).unwrap(),
            "\"textbook\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Exam).unwrap(),
            "\"exam\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Translation).unwrap(),
            "\"translation\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Essay).unwrap(),
            "\"essay\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::Image).unwrap(),
            "\"image\""
        );
        assert_eq!(
            serde_json::to_string(&DstuNodeType::File).unwrap(),
            "\"file\""
        );
    }

    #[test]
    fn test_node_type_deserialization() {
        // 验证从小写字符串反序列化
        assert_eq!(
            serde_json::from_str::<DstuNodeType>("\"folder\"").unwrap(),
            DstuNodeType::Folder
        );
        assert_eq!(
            serde_json::from_str::<DstuNodeType>("\"note\"").unwrap(),
            DstuNodeType::Note
        );
        assert_eq!(
            serde_json::from_str::<DstuNodeType>("\"textbook\"").unwrap(),
            DstuNodeType::Textbook
        );
    }

    #[test]
    fn test_node_type_from_str() {
        // 支持单数和复数形式
        assert_eq!(DstuNodeType::from_str("note"), Some(DstuNodeType::Note));
        assert_eq!(DstuNodeType::from_str("notes"), Some(DstuNodeType::Note));
        assert_eq!(
            DstuNodeType::from_str("textbook"),
            Some(DstuNodeType::Textbook)
        );
        assert_eq!(
            DstuNodeType::from_str("textbooks"),
            Some(DstuNodeType::Textbook)
        );
        assert_eq!(DstuNodeType::from_str("unknown"), None);
    }

    #[test]
    fn test_node_type_to_path_segment() {
        assert_eq!(DstuNodeType::Note.to_path_segment(), "notes");
        assert_eq!(DstuNodeType::Textbook.to_path_segment(), "textbooks");
        assert_eq!(DstuNodeType::Folder.to_path_segment(), "folders");
    }

    #[test]
    fn test_dstu_node_serialization_camel_case() {
        // 验证 DstuNode 序列化为 camelCase
        let node = DstuNode::folder("folder_1", "/高考复习", "高考复习");
        let json = serde_json::to_string(&node).unwrap();

        assert!(json.contains("\"nodeType\"")); // 字段名应该是 nodeType，但 serde 用 type
        assert!(json.contains("\"createdAt\""));
        assert!(json.contains("\"updatedAt\""));
        assert!(!json.contains("\"created_at\""));
        assert!(!json.contains("\"updated_at\""));
    }

    #[test]
    fn test_dstu_node_skip_none() {
        // 验证 Option 字段为 None 时不输出
        let node = DstuNode::folder("folder_1", "/高考复习", "高考复习");
        let json = serde_json::to_string(&node).unwrap();

        assert!(!json.contains("\"children\""));
        assert!(!json.contains("\"childCount\""));
        assert!(!json.contains("\"resourceId\""));
        assert!(!json.contains("\"size\""));
    }

    #[test]
    fn test_dstu_node_resource() {
        let node = DstuNode::resource(
            "note_123",
            "/高考复习/函数/note_123",
            "期末复习",
            DstuNodeType::Note,
            "res_abc123",
        );

        assert_eq!(node.node_type, DstuNodeType::Note);
        assert_eq!(node.resource_id, Some("res_abc123".to_string()));
        assert_eq!(node.preview_type, Some("markdown".to_string()));
    }

    #[test]
    fn test_list_options_defaults() {
        let options = DstuListOptions::new();
        assert_eq!(options.get_limit(), 50);
        assert_eq!(options.get_offset(), 0);
        assert!(!options.is_recursive());
        assert!(options.is_ascending());
    }

    #[test]
    fn test_list_options_builder() {
        let options = DstuListOptions::new()
            .recursive(true)
            .types(vec![DstuNodeType::Note, DstuNodeType::Essay])
            .search("期末")
            .sort("updatedAt", "desc")
            .paginate(20, 10);

        assert!(options.is_recursive());
        assert_eq!(options.types.as_ref().unwrap().len(), 2);
        assert_eq!(options.search, Some("期末".to_string()));
        assert_eq!(options.get_limit(), 20);
        assert_eq!(options.get_offset(), 10);
        assert!(!options.is_ascending());
    }

    #[test]
    fn test_create_options_note() {
        let options = DstuCreateOptions::note("期末复习笔记", "# 期末复习\n\n...");

        assert_eq!(options.node_type, DstuNodeType::Note);
        assert_eq!(options.name, "期末复习笔记");
        assert_eq!(options.content, Some("# 期末复习\n\n...".to_string()));
    }

    #[test]
    fn test_watch_event_serialization() {
        let node = DstuNode::folder("folder_1", "/高考复习", "高考复习");
        let event = DstuWatchEvent::created("/高考复习", node);
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains("\"type\":\"created\""));
        assert!(json.contains("\"path\":\"/高考复习\""));
        assert!(json.contains("\"node\""));
    }

    #[test]
    fn test_watch_event_moved() {
        let node = DstuNode::resource(
            "note_123",
            "/物理复习/note_123",
            "物理笔记",
            DstuNodeType::Note,
            "res_123",
        );
        let event = DstuWatchEvent::moved("/数学复习/note_123", "/物理复习/note_123", node);

        assert_eq!(event.event_type, DstuWatchEventType::Moved);
        assert_eq!(event.old_path, Some("/数学复习/note_123".to_string()));
        assert_eq!(event.path, "/物理复习/note_123");
    }
}
