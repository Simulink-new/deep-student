use crate::canonical_tools::{
    build_openai_function_tool_schema, prepare_external_tool, ApiNameSource, CanonicalExternalTool,
    CanonicalExternalToolConfig,
};
use crate::models::{AppError, ChatMessage};
use crate::providers::ProviderAdapter;
use futures_util::StreamExt;
use log::{debug, info, warn};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tauri::{Emitter, Listener, Window};
use tokio::sync::watch;

use super::config_types::{FrontendMcpTool, McpToolCache, MergedChatMessage};

type Result<T> = std::result::Result<T, AppError>;

// ==================== IncrementalJsonArrayParser ====================

/// 增量 JSON 数组解析器 - 用于流式解析 LLM 输出的 JSON 数组
pub(crate) struct IncrementalJsonArrayParser {
    buffer: String,
    in_array: bool,
    brace_depth: i32,
    in_string: bool,
    escape_next: bool,
}

impl IncrementalJsonArrayParser {
    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
            in_array: false,
            brace_depth: 0,
            in_string: false,
            escape_next: false,
        }
    }

    /// 输入新的文本块，返回解析出的完整 JSON 对象列表
    pub(crate) fn feed(&mut self, chunk: &str) -> Option<Vec<Value>> {
        let mut results = Vec::new();

        for ch in chunk.chars() {
            if self.escape_next {
                self.escape_next = false;
                if self.brace_depth > 0 {
                    self.buffer.push(ch);
                }
                continue;
            }

            if ch == '\\' && self.in_string {
                self.escape_next = true;
                if self.brace_depth > 0 {
                    self.buffer.push(ch);
                }
                continue;
            }

            if ch == '"' && !self.escape_next {
                self.in_string = !self.in_string;
                if self.brace_depth > 0 {
                    self.buffer.push(ch);
                }
                continue;
            }

            if self.in_string {
                if self.brace_depth > 0 {
                    self.buffer.push(ch);
                }
                continue;
            }

            if ch == '[' && !self.in_array && self.brace_depth == 0 {
                self.in_array = true;
                continue;
            }

            if ch == ']' && self.in_array && self.brace_depth == 0 {
                self.in_array = false;
                continue;
            }

            if ch == '{' {
                if self.brace_depth == 0 {
                    self.buffer.clear();
                }
                self.brace_depth += 1;
                self.buffer.push(ch);
                continue;
            }

            if ch == '}' {
                self.brace_depth -= 1;
                self.buffer.push(ch);

                if self.brace_depth == 0 && !self.buffer.is_empty() {
                    if let Ok(obj) = serde_json::from_str::<Value>(&self.buffer) {
                        results.push(obj);
                    }
                    self.buffer.clear();
                }
                continue;
            }

            if self.brace_depth > 0 {
                self.buffer.push(ch);
            }
        }

        if results.is_empty() {
            None
        } else {
            Some(results)
        }
    }

    /// 处理剩余缓冲区内容
    pub(crate) fn finalize(&mut self) -> Option<Vec<Value>> {
        if self.buffer.trim().is_empty() {
            return None;
        }

        if let Ok(obj) = serde_json::from_str::<Value>(&self.buffer) {
            self.buffer.clear();
            return Some(vec![obj]);
        }

        None
    }
}

// ==================== LLMStreamHooks ====================

pub trait LLMStreamHooks: Send + Sync {
    fn on_content_chunk(&self, _text: &str) {}
    fn on_reasoning_chunk(&self, _text: &str) {}
    fn on_thought_signature(&self, _signature: &str) {}
    fn on_tool_call_start(&self, _tool_call_id: &str, _tool_name: &str) {}
    fn on_tool_call_args_delta(&self, _tool_call_id: &str, _delta: &str) {}
    fn on_tool_call(&self, _msg: &ChatMessage) {}
    fn on_tool_result(&self, _msg: &ChatMessage) {}
    fn on_usage(&self, _usage: &serde_json::Value) {}
    fn on_complete(&self, _final_text: &str, _reasoning: Option<&str>) {}
}

impl super::LLMManager {
    // ==================== Cancellation ====================

    pub async fn subscribe_cancel_stream(&self, stream_event: &str) -> watch::Receiver<bool> {
        self.register_cancel_channel(stream_event).await
    }

    pub async fn clear_cancel_stream(&self, stream_event: &str) {
        self.clear_cancel_channel(stream_event).await;
    }

    pub async fn consume_pending_cancel(&self, stream_event: &str) -> bool {
        self.take_cancellation_if_any(stream_event).await
    }

    pub async fn request_cancel_stream(&self, stream_event: &str) {
        info!(
            "[LLM Manager] request_cancel_stream 开始处理: {}",
            stream_event
        );

        debug!("[LLM Manager] 检查 cancel_channels...");
        if let Some(sender) = self.cancel_channels.lock().await.get(stream_event).cloned() {
            debug!("[LLM Manager] 找到 cancel_channel，发送取消信号...");
            if sender.send(true).is_ok() {
                info!(
                    "[LLM Manager] 取消信号已成功发送到 channel: {}",
                    stream_event
                );
            } else {
                warn!(
                    "[LLM Manager] 取消信号发送失败（channel 已关闭）: {}",
                    stream_event
                );
            }
        } else {
            debug!(
                "[LLM Manager] 未找到对应的 cancel_channel: {}",
                stream_event
            );
        }

        debug!("[LLM Manager] 写入 cancel_registry 作为备用...");
        let mut guard = self.cancel_registry.lock().await;
        guard.insert(stream_event.to_string());
        debug!("[LLM Manager] 已将取消标记写入 registry: {}", stream_event);

        debug!("[LLM Manager] request_cancel_stream 完成");
    }

    pub(crate) async fn take_cancellation_if_any(&self, stream_event: &str) -> bool {
        let mut guard = self.cancel_registry.lock().await;
        if guard.remove(stream_event) {
            debug!(
                "[Cancel] Acknowledged and cleared cancel flag for stream: {}",
                stream_event
            );
            true
        } else {
            false
        }
    }

    pub(crate) async fn register_cancel_channel(&self, stream_event: &str) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        self.cancel_channels
            .lock()
            .await
            .insert(stream_event.to_string(), tx);
        rx
    }

    pub(crate) async fn clear_cancel_channel(&self, stream_event: &str) {
        self.cancel_channels.lock().await.remove(stream_event);
    }

    pub async fn cancel_streams_by_prefix(&self, prefix: &str) {
        let keys: Vec<String> = self.cancel_channels.lock().await.keys().cloned().collect();
        for key in keys {
            if key.starts_with(prefix) {
                self.request_cancel_stream(&key).await;
            }
        }
        let guard = self.cancel_registry.lock().await;
        for _key in guard.clone().iter() {}
    }

    // ==================== Streaming Hooks ====================

    pub async fn register_stream_hooks(
        &self,
        stream_event: &str,
        hooks: std::sync::Arc<dyn LLMStreamHooks>,
    ) {
        let key = stream_event.to_string();
        debug!("[Hook] 注册 hook: key={}", key);
        self.hooks_registry.lock().await.insert(key, hooks);
        let count = self.hooks_registry.lock().await.len();
        debug!("[Hook] 注册后 registry 大小: {}", count);
    }

    pub async fn unregister_stream_hooks(&self, stream_event: &str) {
        let key = stream_event.to_string();
        debug!("[Hook] 注销 hook: key={}", key);
        self.hooks_registry.lock().await.remove(&key);
    }

    pub(crate) async fn get_hook(&self, stream_event: &str) -> Option<std::sync::Arc<dyn LLMStreamHooks>> {
        let registry = self.hooks_registry.lock().await;
        registry.get(stream_event).cloned()
    }

    // ==================== Message merging ====================

    /// 🔧 P1修复：合并连续的工具调用消息
    pub(crate) fn merge_consecutive_tool_calls(history: &[ChatMessage]) -> Vec<MergedChatMessage> {
        let mut result = Vec::new();
        let mut pending_tool_calls: Vec<crate::models::ToolCall> = Vec::new();
        let mut pending_tool_results: Vec<ChatMessage> = Vec::new();
        let mut current_thinking_content: Option<String> = None;
        let mut current_thought_signature: Option<String> = None;

        for msg in history {
            if msg.role == "assistant" && msg.tool_call.is_some() {
                if let Some(tc) = &msg.tool_call {
                    let has_new_reasoning = msg.thinking_content.is_some();

                    if has_new_reasoning && !pending_tool_calls.is_empty() {
                        result.push(MergedChatMessage::MergedToolCalls {
                            tool_calls: std::mem::take(&mut pending_tool_calls),
                            content: String::new(),
                            thinking_content: std::mem::take(&mut current_thinking_content),
                            thought_signature: std::mem::take(&mut current_thought_signature),
                        });
                        for tr in std::mem::take(&mut pending_tool_results) {
                            result.push(MergedChatMessage::Regular(tr));
                        }
                    }

                    pending_tool_calls.push(tc.clone());

                    if current_thinking_content.is_none() && has_new_reasoning {
                        current_thinking_content = msg.thinking_content.clone();
                    }
                    if current_thought_signature.is_none() {
                        current_thought_signature = msg.thought_signature.clone();
                    }
                }
            } else if msg.role == "tool" {
                pending_tool_results.push(msg.clone());
            } else {
                if !pending_tool_calls.is_empty() {
                    result.push(MergedChatMessage::MergedToolCalls {
                        tool_calls: std::mem::take(&mut pending_tool_calls),
                        content: String::new(),
                        thinking_content: std::mem::take(&mut current_thinking_content),
                        thought_signature: std::mem::take(&mut current_thought_signature),
                    });
                    for tr in std::mem::take(&mut pending_tool_results) {
                        result.push(MergedChatMessage::Regular(tr));
                    }
                }
                result.push(MergedChatMessage::Regular(msg.clone()));
            }
        }

        if !pending_tool_calls.is_empty() {
            result.push(MergedChatMessage::MergedToolCalls {
                tool_calls: pending_tool_calls,
                content: String::new(),
                thinking_content: current_thinking_content,
                thought_signature: current_thought_signature,
            });
            for tr in pending_tool_results {
                result.push(MergedChatMessage::Regular(tr));
            }
        }

        result
    }

    /// 🔧 合并连续同角色的用户消息（防御性措施）
    pub(crate) fn merge_consecutive_user_messages(messages: &mut Vec<serde_json::Value>) {
        if messages.len() < 2 {
            return;
        }

        let mut merged: Vec<serde_json::Value> = Vec::with_capacity(messages.len());

        for msg in messages.drain(..) {
            let is_user = msg.get("role").and_then(|r| r.as_str()) == Some("user");

            if !is_user {
                merged.push(msg);
                continue;
            }

            let prev_is_user = merged
                .last()
                .and_then(|m| m.get("role"))
                .and_then(|r| r.as_str())
                == Some("user");

            if !prev_is_user {
                merged.push(msg);
                continue;
            }

            let prev = merged.last_mut().unwrap();
            let prev_content = prev.get("content").cloned();
            let curr_content = msg.get("content").cloned();

            match (prev_content, curr_content) {
                (
                    Some(serde_json::Value::String(ref prev_text)),
                    Some(serde_json::Value::String(ref curr_text)),
                ) => {
                    let merged_text = format!("{}\n\n{}", prev_text, curr_text);
                    let combined_len = merged_text.len();
                    prev["content"] = serde_json::Value::String(merged_text);
                    log::warn!(
                        "[LLMManager] Merged 2 consecutive user messages (text+text, combined_len={})",
                        combined_len
                    );
                }
                (
                    Some(serde_json::Value::Array(ref _prev_arr)),
                    Some(serde_json::Value::Array(ref curr_arr)),
                ) => {
                    let curr_len = curr_arr.len();
                    if let Some(arr) = prev.get_mut("content").and_then(|c| c.as_array_mut()) {
                        arr.extend(curr_arr.clone());
                        log::warn!(
                            "[LLMManager] Merged 2 consecutive user messages (array+array, appended {} parts)",
                            curr_len
                        );
                    }
                }
                (
                    Some(serde_json::Value::String(prev_text)),
                    Some(serde_json::Value::Array(curr_arr)),
                ) => {
                    let mut new_content = vec![json!({"type": "text", "text": prev_text})];
                    let curr_len = curr_arr.len();
                    new_content.extend(curr_arr);
                    prev["content"] = serde_json::Value::Array(new_content);
                    log::warn!(
                        "[LLMManager] Merged 2 consecutive user messages (text+array, appended {} parts)",
                        curr_len
                    );
                }
                (
                    Some(serde_json::Value::Array(ref _prev_arr)),
                    Some(serde_json::Value::String(ref curr_text)),
                ) => {
                    if let Some(arr) = prev.get_mut("content").and_then(|c| c.as_array_mut()) {
                        arr.push(json!({"type": "text", "text": curr_text}));
                        log::warn!(
                            "[LLMManager] Merged 2 consecutive user messages (array+text, text_len={})",
                            curr_text.len()
                        );
                    }
                }
                _ => {
                    merged.push(msg);
                }
            }
        }

        *messages = merged;
    }

    /// 🔧 C2修复：合并序列化后的连续 assistant tool_calls 消息
    pub(crate) fn merge_consecutive_assistant_tool_calls(messages: &mut Vec<serde_json::Value>) {
        if messages.len() < 2 {
            return;
        }

        let mut merged: Vec<serde_json::Value> = Vec::with_capacity(messages.len());

        for msg in messages.drain(..) {
            let is_assistant_with_tools = msg.get("role").and_then(|r| r.as_str())
                == Some("assistant")
                && msg
                    .get("tool_calls")
                    .and_then(|tc| tc.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);

            if !is_assistant_with_tools {
                merged.push(msg);
                continue;
            }

            let prev_is_assistant_with_tools = merged
                .last()
                .map(|m| {
                    m.get("role").and_then(|r| r.as_str()) == Some("assistant")
                        && m.get("tool_calls")
                            .and_then(|tc| tc.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false)
                })
                .unwrap_or(false);

            if !prev_is_assistant_with_tools {
                merged.push(msg);
                continue;
            }

            let prev = merged.last_mut().unwrap();
            if let Some(curr_tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                let curr_len = curr_tool_calls.len();
                if let Some(prev_arr) = prev.get_mut("tool_calls").and_then(|tc| tc.as_array_mut()) {
                    prev_arr.extend(curr_tool_calls.clone());
                    log::debug!(
                        "[LLMManager] C2fix: Merged consecutive assistant tool_calls (+{} calls, total={})",
                        curr_len,
                        prev_arr.len()
                    );
                }
            }
        }

        *messages = merged;
    }

    // ==================== Streaming events ====================
    // (旧 emit_specialized_source_events 已删除: {stream_event}_web_search/_rag_sources/
    //  _memory_sources 前端零监听且本函数零调用, RAG 引用数据实际经消息字段与块事件送达 — A11#2)

    // ==================== MCP Tools ====================

    /// 构建工具列表，包含本地工具和 MCP 工具
    pub(crate) async fn build_tools_with_mcp(&self, window: &Window) -> Value {
        let mut tools_array = Vec::new();

        let selected_engines_list = self
            .db
            .get_setting("session.selected_search_engines")
            .ok()
            .flatten()
            .unwrap_or_default();
        let has_selected_engines = !selected_engines_list.trim().is_empty();

        debug!(
            "[搜索引擎] 配置: {:?}, 工具可用: {}",
            selected_engines_list, has_selected_engines
        );

        if has_selected_engines {
            let selected_engines: Vec<String> = selected_engines_list
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            let mut properties = json!({
                "query": { "type": "string", "description": "The web search query" },
                "top_k": { "type": "integer", "description": "Max results to return", "default": 5 },
                "site": { "type": "string", "description": "Optional site restriction (e.g., example.com)" },
                "time_range": { "type": "string", "description": "Optional time range: 1d|7d|30d" }
            });

            if selected_engines.len() > 1 {
                properties["engine"] = json!({
                    "type": "string",
                    "enum": selected_engines,
                    "description": format!("Search engine to use. Available: {}", selected_engines.join(", "))
                });
            } else if selected_engines.len() == 1 {
                debug!(
                    "Single search engine selected: {}, engine parameter hidden",
                    selected_engines[0]
                );
            }

            tools_array.push(build_openai_function_tool_schema(
                "web_search",
                Some("Search the INTERNET/WEB for current information, news, people, events, or any information not available in local knowledge base."),
                Some(json!({
                    "type": "object",
                    "properties": properties,
                    "required": ["query"]
                })),
            ));

            debug!(
                "[工具] web_search工具已成功添加到工具列表，选中引擎: {:?}",
                selected_engines
            );
        } else {
            debug!("[工具] web_search工具未添加：没有选中的搜索引擎");
        }

        // ===== MCP 工具广告 =====
        let cache_ttl_ms: u64 = self
            .db
            .get_setting("mcp.performance.cache_ttl_ms")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300_000);
        let cache_ttl = Duration::from_millis(cache_ttl_ms);
        let namespace_prefix = self
            .db
            .get_setting("mcp.tools.namespace_prefix")
            .ok()
            .flatten()
            .unwrap_or_default();
        let advertise_all = self
            .db
            .get_setting("mcp.tools.advertise_all_tools")
            .ok()
            .flatten()
            .map(|v| v.to_lowercase())
            .map(|v| v != "0" && v != "false")
            .unwrap_or(false);
        let whitelist: Vec<String> = self
            .db
            .get_setting("mcp.tools.whitelist")
            .ok()
            .flatten()
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| Vec::new());
        let blacklist: Vec<String> = self
            .db
            .get_setting("mcp.tools.blacklist")
            .ok()
            .flatten()
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| Vec::new());
        let selected: Vec<String> = self
            .db
            .get_setting("session.selected_mcp_tools")
            .ok()
            .flatten()
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| Vec::new());

        let mcp_tools = self.get_frontend_mcp_tools_cached(window, cache_ttl).await;
        let mut included_count = 0usize;
        let namespace_prefix = namespace_prefix.trim();
        let api_name_prefix = (!namespace_prefix.is_empty()).then_some(namespace_prefix);

        for t in mcp_tools {
            let name = t.name.trim().to_string();
            if name.is_empty() {
                warn!("[MCP] 跳过空名称工具: {:?}", t);
                continue;
            }
            let mut allowed = if !selected.is_empty() {
                selected.iter().any(|s| s == &name)
            } else if advertise_all {
                true
            } else if !whitelist.is_empty() {
                whitelist.iter().any(|s| s == &name)
            } else {
                false
            };
            if !blacklist.is_empty() && blacklist.iter().any(|s| s == &name) {
                allowed = false;
            }
            if !allowed {
                continue;
            }

            let Some(prepared) =
                Self::prepare_frontend_mcp_tool_for_legacy_api(&t, api_name_prefix)
            else {
                warn!("[MCP] 跳过无法规范化广告的工具: {}", name);
                continue;
            };
            tools_array.push(prepared.schema);
            included_count += 1;
        }
        debug!("[MCP] 已广告前端MCP工具 {} 个", included_count);

        debug!("[工具] 工具列表构建完成，总计 {} 个工具", tools_array.len());

        Value::Array(tools_array)
    }

    /// 获取 MCP 工具列表（使用 LLMManager 内部共享缓存）
    async fn get_frontend_mcp_tools_cached(
        &self,
        window: &Window,
        cache_ttl: Duration,
    ) -> Vec<FrontendMcpTool> {
        if let Some(cache) = self.mcp_tool_cache.read().await.as_ref() {
            if !cache.is_expired() {
                if !cache.tools.is_empty() {
                    return cache.tools.clone();
                }
            }
        }
        let tools = match self
            .request_frontend_mcp_tools(window, Duration::from_millis(15_000))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("MCP tools bridge failed: {}", e);
                vec![]
            }
        };
        let mut guard = self.mcp_tool_cache.write().await;
        *guard = Some(McpToolCache::new(tools.clone(), cache_ttl));
        tools
    }

    /// 公开：预热前端 MCP 工具清单缓存
    pub async fn preheat_mcp_tools_public(&self, window: &Window) -> usize {
        let ttl_ms: u64 = self
            .db
            .get_setting("mcp.performance.cache_ttl_ms")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300_000);
        let tools = self
            .get_frontend_mcp_tools_cached(window, Duration::from_millis(ttl_ms))
            .await;
        tools.len()
    }

    async fn request_frontend_mcp_tools(
        &self,
        window: &Window,
        timeout: Duration,
    ) -> anyhow::Result<Vec<FrontendMcpTool>> {
        use tokio::sync::oneshot;
        use tokio::time::timeout as tokio_timeout;
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let event_name = format!("mcp-bridge-tools-response:{}", correlation_id);
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        let w = window.clone();
        let tx_guard = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let tx_guard_clone = tx_guard.clone();
        let id = w.listen(event_name.clone(), move |e| {
            let payload_str = e.payload();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(payload_str) {
                if let Some(tx) = tx_guard_clone
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                {
                    let _ = tx.send(val);
                }
            }
        });
        window
            .emit(
                "mcp-bridge-tools-request",
                json!({"correlationId": correlation_id}),
            )
            .map_err(|e| anyhow::anyhow!("emit failed: {}", e))?;

        let waited = tokio_timeout(timeout, rx).await;
        let _ = window.unlisten(id);
        let val = match waited {
            Err(_) => return Err(anyhow::anyhow!("timeout waiting tools response")),
            Ok(Err(_)) => return Err(anyhow::anyhow!("bridge channel closed")),
            Ok(Ok(v)) => v,
        };
        let arr = val.get("tools").cloned().unwrap_or(json!([]));
        let tools: Vec<FrontendMcpTool> =
            serde_json::from_value(arr).unwrap_or_else(|_| Vec::new());
        Ok(tools)
    }

    /// 清除 MCP 工具缓存
    pub async fn clear_mcp_tool_cache(&self) {
        let mut cache_guard = self.mcp_tool_cache.write().await;
        *cache_guard = None;
        info!("MCP tool cache cleared");
    }

    pub(crate) fn prepare_frontend_mcp_tool_for_legacy_api(
        mcp_tool: &FrontendMcpTool,
        api_name_prefix: Option<&str>,
    ) -> Option<CanonicalExternalTool> {
        prepare_external_tool(
            &mcp_tool.name,
            None,
            mcp_tool.description.as_deref(),
            Some(&mcp_tool.input_schema),
            CanonicalExternalToolConfig {
                internal_prefix: None,
                preserve_prefix: None,
                api_name_prefix,
                include_server_suffix: false,
                api_name_source: ApiNameSource::BridgeName,
            },
        )
    }

    // ==================== Text injection ====================

    pub(crate) fn coalesce_injection_texts(texts: &[String]) -> Option<String> {
        if texts.is_empty() {
            return None;
        }
        let per_item_max = 1600usize;
        let total_max = 20_000usize;
        let mut acc = String::new();
        for (idx, text) in texts.iter().enumerate() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut chunk = trimmed.to_string();
            let original_len = chunk.chars().count();
            if original_len > per_item_max {
                chunk = chunk.chars().take(per_item_max).collect();
                debug!(
                    "  [{}] 注入段超限，截断 {} -> {} 字符",
                    idx,
                    original_len,
                    chunk.chars().count()
                );
            }
            if acc.chars().count() + chunk.chars().count() > total_max {
                debug!(
                    "  [{}] 注入总量已达上限，停止继续追加（当前 {} 字符）",
                    idx,
                    acc.chars().count()
                );
                break;
            }
            debug!(
                "  [{}] 收录注入段，长度 {} 字符",
                idx,
                chunk.chars().count()
            );
            acc.push_str(&chunk);
        }
        if acc.is_empty() {
            None
        } else {
            debug!("[Inject] 合并注入文本总长度: {} 字符", acc.chars().count());
            debug!(
                "[Inject] 注入预览: {}",
                &acc.chars().take(200).collect::<String>()
            );
            Some(acc)
        }
    }

    pub(crate) fn append_injection_to_system_message(messages: &mut Vec<Value>, inject_content: &str) {
        if inject_content.trim().is_empty() {
            warn!("[Inject] 注入内容为空，跳过");
            return;
        }
        if let Some(first_msg) = messages.get_mut(0) {
            if first_msg["role"] == "system" {
                match &first_msg["content"] {
                    Value::String(s) => {
                        let new_content = format!("{}\n\n{}", s, inject_content.trim());
                        first_msg["content"] = json!(new_content);
                    }
                    Value::Array(arr) => {
                        let mut new_arr = arr.clone();
                        new_arr.push(json!({
                            "type": "text",
                            "text": inject_content.trim()
                        }));
                        first_msg["content"] = json!(new_arr);
                    }
                    _ => {
                        first_msg["content"] = json!(inject_content.trim());
                    }
                }
                debug!("[Inject] 已将注入文本追加到现有系统消息");
                return;
            }
        }
        messages.insert(
            0,
            json!({
                "role": "system",
                "content": inject_content.trim()
            }),
        );
        debug!("[Inject] 未找到系统消息，已创建新的系统消息承载注入内容");
    }

    /// 构建图谱检索结果的注入文本
    pub(crate) fn build_prefetched_graph_injection(context: &HashMap<String, Value>) -> Option<String> {
        let prefetched = context
            .get("prefetched_graph_sources")
            .and_then(|v| v.as_array())?;
        if prefetched.is_empty() {
            return None;
        }
        let mut rows = Vec::new();
        for (idx, item) in prefetched.iter().enumerate() {
            let title = item
                .get("file_name")
                .or_else(|| item.get("title"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Graph Insight");
            let snippet = item
                .get("chunk_text")
                .or_else(|| item.get("snippet"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if snippet.trim().is_empty() {
                continue;
            }
            rows.push(format!("({}) {}\n{}", idx + 1, title, snippet));
            if rows.len() >= 5 {
                break;
            }
        }
        if rows.is_empty() {
            None
        } else {
            Some(format!("【个人图谱】\n{}\n\n", rows.join("\n\n")))
        }
    }

    // ==================== Question parsing ====================

    /// P2-3: 调用 LLM 解析文档内容为题目
    pub async fn call_llm_for_question_parsing(&self, prompt: &str) -> Result<String> {
        let api_config = self.get_model2_config().await?;
        let api_key = self.decrypt_api_key_if_needed(&api_config.api_key)?;
        let model_id = api_config.model.clone();

        let messages = vec![
            json!({
                "role": "system",
                "content": "你是一个专业的题目解析助手。请准确识别文档中的题目，并按指定格式输出。"
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];

        let mut request_body = json!({
            "model": model_id,
            "messages": messages,
            "temperature": 0.3,
            "max_tokens": 4096
        });

        super::LLMManager::apply_reasoning_config(&mut request_body, &api_config, None);

        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                api_config.base_url.trim_end_matches('/')
            ))
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| AppError::network(format!("LLM 请求失败: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(AppError::network(format!(
                "LLM 响应错误 {}: {}",
                status, error_text
            )));
        }

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| AppError::validation(format!("解析 LLM 响应失败: {}", e)))?;

        let content = response_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| AppError::validation("LLM 响应格式错误"))?;

        Ok(content.to_string())
    }

    /// 流式调用 LLM 解析题目
    pub async fn call_llm_for_question_parsing_streaming<F>(
        &self,
        prompt: &str,
        model_config_id: Option<&str>,
        mut on_question: F,
    ) -> Result<Vec<Value>>
    where
        F: FnMut(Value) -> bool + Send,
    {
        let api_config = if let Some(config_id) = model_config_id {
            let configs = self.get_api_configs().await?;
            configs
                .into_iter()
                .find(|c| c.id == config_id)
                .ok_or_else(|| {
                    AppError::configuration(format!("找不到指定的模型配置: {}", config_id))
                })?
        } else {
            self.get_model2_config().await?
        };

        let api_key = self.decrypt_api_key_if_needed(&api_config.api_key)?;
        let model_id = api_config.model.clone();

        let messages = vec![
            json!({
                "role": "system",
                "content": "你是一个专业的题目解析助手。请准确识别文档中的题目，并按指定格式输出。"
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];

        let mut request_body = json!({
            "model": model_id,
            "messages": messages,
            "temperature": 0.3,
            "max_tokens": 8192,
            "stream": true
        });

        super::LLMManager::apply_reasoning_config(&mut request_body, &api_config, None);

        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                api_config.base_url.trim_end_matches('/')
            ))
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| AppError::network(format!("LLM 流式请求失败: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(AppError::network(format!(
                "LLM 响应错误 {}: {}",
                status, error_text
            )));
        }

        let mut stream = response.bytes_stream();
        let mut sse_buffer = crate::utils::sse_buffer::SseLineBuffer::new();

        let provider = api_config.provider_type.as_deref().unwrap_or("openai");
        let adapter: Box<dyn ProviderAdapter> =
            match provider.to_lowercase().as_str() {
                "google" | "gemini" => Box::new(crate::providers::GeminiAdapter::new()),
                "anthropic" | "claude" => Box::new(crate::providers::AnthropicAdapter::new()),
                _ => Box::new(crate::providers::OpenAIAdapter),
            };

        let mut full_content = String::new();
        let mut all_questions: Vec<Value> = Vec::new();
        let mut json_parser = IncrementalJsonArrayParser::new();
        let mut stream_ended = false;
        let mut aborted = false;

        while !stream_ended && !aborted {
            let next_item = stream.next().await;
            let Some(next) = next_item else { break };

            let chunk = match next {
                Ok(b) => b,
                Err(e) => return Err(AppError::llm(format!("读取流式响应失败: {}", e))),
            };

            let text = String::from_utf8_lossy(&chunk);
            let lines = sse_buffer.process_chunk(&text);

            for line in lines {
                if crate::utils::sse_buffer::SseLineBuffer::check_done_marker(&line) {
                    stream_ended = true;
                    break;
                }
                let events = adapter.parse_stream(&line);
                for ev in events {
                    match ev {
                        crate::providers::StreamEvent::ContentChunk(s) => {
                            full_content.push_str(&s);
                            if let Some(questions) = json_parser.feed(&s) {
                                for q in questions {
                                    if !on_question(q.clone()) {
                                        aborted = true;
                                        break;
                                    }
                                    all_questions.push(q);
                                }
                            }
                        }
                        crate::providers::StreamEvent::Done => {
                            stream_ended = true;
                            break;
                        }
                        _ => {}
                    }
                    if aborted {
                        break;
                    }
                }
                if stream_ended || aborted {
                    break;
                }
            }
        }

        if !aborted {
            if let Some(questions) = json_parser.finalize() {
                for q in questions {
                    if on_question(q.clone()) {
                        all_questions.push(q);
                    }
                }
            }
        }

        Ok(all_questions)
    }

    pub async fn call_llm_for_question_parsing_with_model(
        &self,
        prompt: &str,
        model_config_id: Option<&str>,
    ) -> Result<String> {
        let api_config = if let Some(config_id) = model_config_id {
            let configs = self.get_api_configs().await?;
            configs
                .into_iter()
                .find(|c| c.id == config_id)
                .ok_or_else(|| {
                    AppError::configuration(format!("找不到指定的模型配置: {}", config_id))
                })?
        } else {
            self.get_model2_config().await?
        };

        let api_key = self.decrypt_api_key_if_needed(&api_config.api_key)?;
        let model_id = api_config.model.clone();

        let messages = vec![
            json!({
                "role": "system",
                "content": "你是一个专业的题目解析助手。请准确识别文档中的题目，并按指定格式输出。"
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];

        let mut request_body = json!({
            "model": model_id,
            "messages": messages,
            "temperature": 0.3,
            "max_tokens": 4096
        });

        super::LLMManager::apply_reasoning_config(&mut request_body, &api_config, None);

        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                api_config.base_url.trim_end_matches('/')
            ))
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| AppError::network(format!("LLM 请求失败: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(AppError::network(format!(
                "LLM 响应错误 {}: {}",
                status, error_text
            )));
        }

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| AppError::validation(format!("解析 LLM 响应失败: {}", e)))?;

        let content = response_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| AppError::validation("LLM 响应格式错误"))?;

        Ok(content.to_string())
    }
}
