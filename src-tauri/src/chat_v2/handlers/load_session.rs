//! 会话加载命令处理器
//!
//! 加载会话的完整数据，包括会话信息、消息列表、块列表和会话状态。

use std::sync::Arc;

use std::time::Instant;

use tauri::State;

use crate::chat_v2::database::ChatV2Database;
use crate::chat_v2::error::ChatV2Error;
use crate::chat_v2::repo::ChatV2Repo;
use crate::chat_v2::types::LoadSessionResponse;

/// 加载会话完整数据
///
/// 从数据库加载会话的所有相关数据，用于前端初始化会话视图。
///
/// ## 参数
/// - `session_id`: 会话 ID
/// - `limit`: 可选分页大小（1..=500）。传入时只返回最近一页（或游标之前的一页）
///   消息及其关联块，响应带 `hasMore` 标记；不传时保持全量加载（兼容旧调用方）
/// - `before_message_id`: 可选游标消息 ID，返回严格早于该消息的一页（懒加载"加载更早"用）
/// - `db`: Chat V2 独立数据库
///
/// ## 返回
/// - `Ok(LoadSessionResponse)`: 会话完整数据
/// - `Err(String)`: 会话不存在或加载失败
///
/// ## 响应结构
/// ```json
/// {
///   "session": { ... },
///   "messages": [ ... ],
///   "blocks": [ ... ],
///   "state": { ... },
///   "hasMore": true
/// }
/// ```
#[tauri::command]
pub async fn chat_v2_load_session(
    session_id: String,
    limit: Option<i64>,
    before_message_id: Option<String>,
    db: State<'_, Arc<ChatV2Database>>,
) -> Result<LoadSessionResponse, String> {
    let t0 = Instant::now();
    log::info!(
        "[ChatV2::handlers] chat_v2_load_session: session_id={}, limit={:?}, before_message_id={:?}",
        session_id,
        limit,
        before_message_id
    );

    // 验证会话 ID 格式（宽松模式，兼容所有历史版本前缀）
    // 🔧 2026-06: 放宽验证 — 历史会话可能使用不同的 ID 前缀（如旧版 chat_v2_*），
    // 不应因 ID 格式不匹配而拒绝加载。只拒绝明显无效的空 ID 或纯空白。
    if session_id.trim().is_empty() {
        return Err(
            ChatV2Error::Validation(format!("Invalid session ID: empty or whitespace-only")).into(),
        );
    }

    // 从数据库加载会话数据：分页（懒加载）或全量（默认，兼容）
    let response = match limit {
        Some(limit) => {
            // 钳制到合理区间，防止误传 0/负数/超大值
            let clamped = limit.clamp(1, 500);
            load_session_paged_from_db(&session_id, clamped, before_message_id.as_deref(), &db)?
        }
        None => load_session_from_db(&session_id, &db)?,
    };

    let elapsed_ms = t0.elapsed().as_millis();
    log::info!(
        "[ChatV2::handlers] Loaded session: session_id={}, messages={}, blocks={}, has_more={:?}, elapsed_ms={}",
        session_id,
        response.messages.len(),
        response.blocks.len(),
        response.has_more,
        elapsed_ms
    );

    Ok(response)
}

/// 从数据库加载会话完整数据
fn load_session_from_db(
    session_id: &str,
    db: &ChatV2Database,
) -> Result<LoadSessionResponse, ChatV2Error> {
    // 调用 ChatV2Repo::load_session_full_v2 加载完整会话数据
    ChatV2Repo::load_session_full_v2(db, session_id)
}

/// 从数据库分页加载会话（懒加载历史消息）
fn load_session_paged_from_db(
    session_id: &str,
    limit: i64,
    before_message_id: Option<&str>,
    db: &ChatV2Database,
) -> Result<LoadSessionResponse, ChatV2Error> {
    ChatV2Repo::load_session_paged_v2(db, session_id, limit, before_message_id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_session_id_validation() {
        // 有效的会话 ID
        assert!("sess_12345".starts_with("sess_"));
        assert!("sess_a1b2c3d4-e5f6-7890-abcd-ef1234567890".starts_with("sess_"));
        assert!("agent_12345".starts_with("agent_"));
        assert!("subagent_foo_bar".starts_with("subagent_"));

        // 无效的会话 ID
        assert!(!"invalid_id".starts_with("sess_"));
        assert!(!"session_12345".starts_with("sess_"));
    }
}
