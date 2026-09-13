//! 多模态知识库模块
//!
//! ★ 2026-01 清理说明：
//! - 索引和检索已迁移到 VFS 多模态服务（crate::vfs::multimodal_service）
//! - 本模块仅保留被 VFS 服务依赖的核心组件：
//!   - `types`: 核心类型定义
//!   - `embedding_service`: 多模态嵌入生成
//!   - `page_indexer`: 页面索引元数据解析
//!
//! 已废弃（不再导出）：
//! - `vector_store`: 使用 VFS Lance Store 替代
//! - `retriever`: 使用 VfsMultimodalService.search 替代
//! - `dimension_registry`: 使用 VfsDimensionRepo 替代

// 核心类型定义（仍需保留）
pub mod types;

// 嵌入服务（VFS 多模态服务依赖）
pub mod embedding_chunker;
pub mod embedding_service;

// A9#5: page_indexer 模块已删(1,760 行死代码,仅 AttachmentPreview 类型被 VFS 引用,已迁 types.rs)

// A9#5: reranker_service(307 行)与 vector_store(973 行)已删——唯一依赖方 retriever.rs 未编译,llm_manager 实无引用

// 重新导出常用类型
pub use types::{
    MultimodalImage,
    // 索引相关
    MultimodalIndexingMode,
    // 输入类型
    MultimodalInput,
    MultimodalVideo,
    // 元数据
    PageEmbeddingMetadata,
    // 来源类型
    SourceType,
    // API 类型
    VLEmbeddingInputItem,
    VLRerankerResult, // llm_manager 依赖
};

// 嵌入服务导出
pub use embedding_service::{EmbeddingServiceConfig, MultimodalEmbeddingService};

pub use types::AttachmentPreview; // A9#5: 迁至 types 后的 re-export
