pub mod adapters;
mod builtin_vendors;
mod config_types;
mod exam_engine;
mod image_processing;
mod model2_pipeline;
mod model_profile_service;
pub(crate) mod parser;
mod rag_extension;
mod streaming;
mod tool_call;
mod vendor_config_service;

// Re-exports for public API
pub use config_types::{
    ApiConfig, ExamSegmentationCard, ExamSegmentationOutput, ExamSegmentationPage, ModelProfile,
    OcrModelConfig, ResolvedModelConfig, VendorConfig,
};
pub use streaming::LLMStreamHooks;

pub(crate) use config_types::{
    build_provider_adapter, effective_max_tokens, normalize_nonstream_response_to_openai,
    provider_supports_openai_responses, resolve_preferred_protocol_for_provider,
    should_use_openai_responses_for_config, EXAM_SEGMENT_MAX_DIMENSION,
    EXAM_SEGMENT_MAX_IMAGE_BYTES, ImagePayload, MergedChatMessage,
};
pub(crate) use streaming::IncrementalJsonArrayParser;

use crate::crypto::CryptoService;
use crate::database::Database;
use crate::file_manager::FileManager;
use crate::models::AppError;
use log::{debug, info, warn};
use reqwest::{header::HeaderMap, Client, ClientBuilder};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;

use self::config_types::{
    build_user_preference_prompt_from_profile, StoredUserPreferenceProfile,
    USER_PREFERENCES_SETTING_KEY,
};
// ==================== LLMManager ====================

pub struct LLMManager {
    client: Client,
    db: Arc<Database>,
    file_manager: Arc<FileManager>,
    crypto_service: CryptoService,
    cancel_registry: Arc<TokioMutex<HashSet<String>>>,
    cancel_channels: Arc<TokioMutex<std::collections::HashMap<String, watch::Sender<bool>>>>,
    mcp_tool_cache: Arc<RwLock<Option<config_types::McpToolCache>>>,
    hooks_registry:
        Arc<TokioMutex<std::collections::HashMap<String, std::sync::Arc<dyn LLMStreamHooks>>>>,
    /// ★ perf-audit A6#1: get_api_configs 结果缓存——全模块 20 个调用点原本每请求
    /// (含工具循环每 turn) 6-8 次 settings 读 + 3-5 次全量 JSON 解析; 写路径主动失效 + 60s TTL 兜底
    api_configs_cache:
        Arc<TokioMutex<Option<(std::time::Instant, Arc<Vec<ApiConfig>>)>>>,
}

// ==================== Core impl ====================

impl LLMManager {
    pub fn new(db: Arc<Database>, file_manager: Arc<FileManager>) -> Result<Self> {
        let client = Self::create_http_client_with_fallback();

        let app_data_dir_path = file_manager.get_app_data_dir();
        let crypto_service = CryptoService::new(&app_data_dir_path.to_path_buf())
            .map_err(|e| AppError::configuration(format!("加密服务初始化失败: {e}")))?;

        Ok(Self {
            client,
            db,
            file_manager,
            crypto_service,
            cancel_registry: Arc::new(TokioMutex::new(HashSet::new())),
            cancel_channels: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            mcp_tool_cache: Arc::new(RwLock::new(None)),
            hooks_registry: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            api_configs_cache: Arc::new(TokioMutex::new(None)),
        })
    }

    // 对外暴露 HTTP 客户端
    pub fn get_http_client(&self) -> Client {
        self.client.clone()
    }

    fn log_request_body(&self, tag: &str, body: &serde_json::Value) {
        match serde_json::to_string_pretty(body) {
            Ok(pretty) => debug!("[{}] 请求体如下:\n{}", tag, pretty),
            Err(e) => warn!("[{}] 请求体序列化失败: {}", tag, e),
        }
    }

    fn provider_error(context: &str, err: crate::providers::ProviderError) -> AppError {
        AppError::llm(format!("{}: {}", context, err))
    }

    /// 应用推理相关配置到请求体
    pub(crate) fn apply_reasoning_config(
        body: &mut serde_json::Value,
        config: &config_types::ApiConfig,
        enable_thinking: Option<bool>,
    ) {
        let Value::Object(map) = body else {
            return;
        };

        let adapter = crate::llm_manager::adapters::get_adapter(
            config.provider_type.as_deref(),
            config.provider_scope.as_deref(),
            &config.model_adapter,
        );

        if adapter.should_remove_sampling_params(config) {
            map.remove("temperature");
            map.remove("top_p");
            map.remove("presence_penalty");
            map.remove("frequency_penalty");
            map.remove("logprobs");
        }

        let early_return = adapter.apply_reasoning_config(map, config, enable_thinking);

        if early_return {
            return;
        }

        adapter.apply_common_params(map, config);
    }

    pub fn user_preference_prompt(&self) -> Option<String> {
        let stored = match self.db.get_setting(USER_PREFERENCES_SETTING_KEY) {
            Ok(value) => value?,
            Err(err) => {
                warn!("[UserPreferences] 读取失败: {}", err);
                return None;
            }
        };

        let trimmed = stored.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut profile = match serde_json::from_str::<StoredUserPreferenceProfile>(trimmed) {
            Ok(parsed) => parsed,
            Err(_) => StoredUserPreferenceProfile {
                enabled: true,
                notes: trimmed.to_string(),
                ..Default::default()
            },
        };

        if !trimmed.contains("\"enabled\"") {
            let has_any_content = !profile.background.trim().is_empty()
                || !profile.goals.trim().is_empty()
                || !profile.communication.trim().is_empty()
                || !profile.notes.trim().is_empty();
            if has_any_content {
                profile.enabled = true;
            }
        }

        if !profile.enabled {
            return None;
        }

        build_user_preference_prompt_from_profile(&profile)
    }

    /// 创建HTTP客户端，使用渐进式回退策略
    fn create_http_client_with_fallback() -> Client {
        let mut headers = HeaderMap::new();
        headers.insert("Accept-Encoding", "identity".parse().unwrap());

        let client_builder = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(false)
            .default_headers(headers.clone());

        if let Ok(client) = client_builder.build() {
            info!("HTTP客户端创建成功: 完整配置（超时120s，连接15s，rustls TLS）");
            return client;
        }

        let client_builder_2 = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(false)
            .default_headers(headers.clone());

        if let Ok(client) = client_builder_2.build() {
            info!("HTTP客户端创建成功: 简化TLS配置（超时120s，连接15s，系统TLS）");
            return client;
        }

        if let Ok(client) = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(300))
            .default_headers(headers.clone())
            .build()
        {
            info!("HTTP客户端创建成功: 仅超时配置（超时120s）");
            return client;
        }

        if let Ok(client) = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(180))
            .default_headers(headers.clone())
            .build()
        {
            info!("HTTP客户端创建成功: 最小配置（超时60s）");
            return client;
        }

        warn!("所有配置均失败，使用默认HTTP客户端（无超时配置）");
        warn!("这可能导致网络请求挂起，建议检查系统网络和TLS配置");
        Client::new()
    }

    /// 检测 Gemini 非流式响应中的安全阻断
    pub(crate) fn extract_gemini_safety_error(resp: &serde_json::Value) -> Option<String> {
        config_types::extract_gemini_safety_error(resp)
    }

    /// Get global singleton
    pub async fn global() -> anyhow::Result<Arc<LLMManager>> {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<Arc<LLMManager>> = OnceLock::new();

        if let Some(mgr) = INSTANCE.get() {
            return Ok(mgr.clone());
        }

        Err(anyhow::anyhow!(
            "LLMManager::global is not yet implemented in this build"
        ))
    }
}

type Result<T> = std::result::Result<T, AppError>;

use serde_json::Value;

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::config_types::{
        self, normalize_provider_protocol_registry_value, provider_supports_openai_responses,
        resolve_preferred_protocol_for_provider, CapabilityOverrides, FrontendMcpTool,
        ModelProfile, RegistryDocument, RegistryModelRecord, RegistrySeriesRecord, VendorConfig,
        BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY, HIDDEN_BUILTIN_MODEL_PROFILES_KEY,
        MergedChatMessage, ProviderProtocolRegistryDocument,
    };
    use super::*;
    use crate::database::Database;
    use crate::file_manager::FileManager;
    use crate::llm_manager::LLMManager;
    use crate::models::{ChatMessage, ToolCall};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn deepseek_sampling_body(model: &str) -> serde_json::Value {
        json!({
            "model": model,
            "messages": [],
            "temperature": 0.7,
            "top_p": 0.9,
            "presence_penalty": 0.3,
            "frequency_penalty": 0.4,
            "logprobs": true
        })
    }

    fn profile(
        id: &str,
        label: &str,
        model: &str,
        supports_tools: bool,
        is_builtin: bool,
    ) -> ModelProfile {
        ModelProfile {
            id: id.to_string(),
            vendor_id: "builtin-deepseek".to_string(),
            label: label.to_string(),
            model: model.to_string(),
            supports_tools,
            is_builtin,
            ..ModelProfile::default()
        }
    }

    fn create_test_llm_manager(temp_dir: &TempDir) -> LLMManager {
        let db_path = temp_dir.path().join("test.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open test db");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .expect("create settings table");
        let db = Arc::new(Database::new(&db_path).expect("create test database"));
        let file_manager =
            Arc::new(FileManager::new(temp_dir.path().to_path_buf()).expect("create file manager"));
        LLMManager::new(db, file_manager).expect("create llm manager")
    }

    #[test]
    fn prepare_frontend_mcp_tool_for_legacy_api_encodes_invalid_names_with_namespace_prefix() {
        let tool = FrontendMcpTool {
            name: "fetch:url".to_string(),
            description: Some("Fetch URL".to_string()),
            input_schema: json!({ "type": "object" }),
        };

        let prepared =
            LLMManager::prepare_frontend_mcp_tool_for_legacy_api(&tool, Some("mcp.tools."))
                .expect("tool should prepare");

        assert_eq!(prepared.bridge_name, "fetch:url");
        assert_eq!(prepared.internal_tool_name, "fetch:url");
        assert_eq!(
            crate::canonical_tools::decode_tool_name_from_api(&prepared.api_name),
            Some("mcp.tools.fetch:url".to_string())
        );
        assert_eq!(
            prepared.schema["function"]["name"],
            json!(prepared.api_name)
        );
    }

    fn builtin_vendor_config(api_key: &str) -> VendorConfig {
        VendorConfig {
            id: "builtin-openai".to_string(),
            name: "OpenAI".to_string(),
            provider_type: "openai".to_string(),
            api_protocol: Some(resolve_preferred_protocol_for_provider(
                Some("openai"),
                Some("openai"),
                "https://api.openai.com/v1",
                None,
            )),
            supports_openai_responses: Some(provider_supports_openai_responses(
                Some("openai"),
                "https://api.openai.com/v1",
                None,
            )),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: api_key.to_string(),
            headers: std::collections::HashMap::new(),
            rate_limit_per_minute: None,
            default_timeout_ms: None,
            notes: None,
            is_builtin: true,
            is_read_only: false,
            sort_order: None,
            max_tokens_limit: None,
            website_url: None,
        }
    }

    fn tool_call_message(id: &str, reasoning: Option<&str>) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: chrono::Utc::now(),
            thinking_content: reasoning.map(str::to_string),
            thought_signature: None,
            rag_sources: None,
            memory_sources: None,
            graph_sources: None,
            web_search_sources: None,
            image_paths: None,
            image_base64: None,
            doc_attachments: None,
            multimodal_content: None,
            tool_call: Some(ToolCall {
                id: id.to_string(),
                tool_name: "builtin_test".to_string(),
                args_json: json!({}),
            }),
            tool_result: None,
            overrides: None,
            relations: None,
            persistent_stable_id: None,
            metadata: None,
        }
    }

    #[test]
    fn vendor_config_default_uses_registry_backed_openai_defaults() {
        let vendor = VendorConfig::default();
        assert_eq!(vendor.provider_type, "openai");
        assert_eq!(vendor.api_protocol.as_deref(), Some("openai_chat_completions"));
        assert_eq!(vendor.supports_openai_responses, Some(false));
    }

    #[test]
    fn resolve_preferred_protocol_for_provider_uses_registry_defaults_for_native_vendors() {
        assert_eq!(
            resolve_preferred_protocol_for_provider(
                Some("anthropic"),
                Some("anthropic"),
                "https://api.anthropic.com/v1",
                None,
            ),
            "anthropic_messages"
        );
        assert_eq!(
            resolve_preferred_protocol_for_provider(
                Some("gemini"),
                Some("google"),
                "https://generativelanguage.googleapis.com",
                None,
            ),
            "google_generate_content"
        );
        assert_eq!(
            resolve_preferred_protocol_for_provider(
                Some("siliconflow"),
                Some("general"),
                "https://api.siliconflow.cn/v1",
                None,
            ),
            "openai_chat_completions"
        );
    }

    #[test]
    fn provider_protocol_registry_contract_is_well_formed() {
        let raw = include_str!("../../../scripts/provider-protocol-registry.json");
        let registry: ProviderProtocolRegistryDocument =
            serde_json::from_str(raw).expect("provider protocol registry should parse");

        assert!(!registry.providers.is_empty());

        for provider in registry.providers {
            assert!(!provider.provider_type.is_empty());
            assert!(!provider.allowed_protocols.is_empty());
            assert!(!provider.default_protocol.is_empty());
            assert!(provider
                .allowed_protocols
                .iter()
                .any(|protocol| protocol == &provider.default_protocol));
        }
    }

    #[test]
    fn provider_protocol_registry_contract_preserves_shared_routing_expectations() {
        let openai = config_types::get_provider_protocol_record(Some("openai")).expect("openai provider");
        assert_eq!(openai.default_protocol, "openai_responses");
        assert!(openai.supports_openai_responses);
        assert!(openai
            .allowed_protocols
            .iter()
            .any(|protocol| protocol == "openai_responses"));

        let anthropic =
            config_types::get_provider_protocol_record(Some("anthropic")).expect("anthropic provider");
        assert_eq!(anthropic.allowed_protocols, vec!["anthropic_messages"]);

        let gemini = config_types::get_provider_protocol_record(Some("gemini")).expect("gemini provider");
        assert_eq!(gemini.allowed_protocols, vec!["google_generate_content"]);

        let custom = config_types::get_provider_protocol_record(Some("custom")).expect("custom provider");
        assert_eq!(custom.default_protocol, "openai_chat_completions");
        assert!(!provider_supports_openai_responses(
            Some("custom"),
            "https://proxy.example.com/v1",
            None,
        ));
    }

    #[test]
    fn third_party_openai_compatible_base_url_does_not_imply_official_responses_support() {
        assert!(!provider_supports_openai_responses(
            Some("openai"),
            "https://api.qsl.fan/v1",
            None,
        ));
    }

    #[tokio::test]
    async fn bootstrap_vendor_model_config_repairs_invalid_openai_responses_overrides() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = create_test_llm_manager(&temp_dir);
        let vendor = VendorConfig {
            id: "vendor-qsl".to_string(),
            name: "QSL".to_string(),
            provider_type: "openai".to_string(),
            api_protocol: Some("openai_responses".to_string()),
            supports_openai_responses: None,
            base_url: "https://api.qsl.fan/v1".to_string(),
            api_key: String::new(),
            headers: std::collections::HashMap::new(),
            rate_limit_per_minute: None,
            default_timeout_ms: None,
            notes: None,
            is_builtin: false,
            is_read_only: false,
            sort_order: None,
            max_tokens_limit: None,
            website_url: None,
        };
        let profile = ModelProfile {
            id: "profile-qsl".to_string(),
            vendor_id: "vendor-qsl".to_string(),
            label: "DeepSeek V4".to_string(),
            model: "deepseek-v4-pro".to_string(),
            api_protocol: Some("openai_responses".to_string()),
            model_adapter: "general".to_string(),
            supports_tools: true,
            supports_reasoning: true,
            is_reasoning: true,
            ..ModelProfile::default()
        };

        manager
            .db
            .save_setting(
                "vendor_configs",
                &serde_json::to_string(&vec![vendor]).unwrap(),
            )
            .expect("save vendors");
        manager
            .db
            .save_setting(
                "model_profiles",
                &serde_json::to_string(&vec![profile]).unwrap(),
            )
            .expect("save profiles");

        manager
            .bootstrap_vendor_model_config()
            .await
            .expect("bootstrap should repair stored protocols");

        let vendors = manager
            .read_user_vendor_configs()
            .await
            .expect("read vendors after repair");
        assert_eq!(
            vendors[0].api_protocol.as_deref(),
            Some("openai_chat_completions")
        );
        assert_eq!(vendors[0].supports_openai_responses, Some(false));

        let profiles = manager
            .read_user_model_profiles()
            .await
            .expect("read profiles after repair");
        assert_eq!(
            profiles[0].api_protocol.as_deref(),
            Some("openai_chat_completions")
        );
    }

    #[test]
    fn merge_consecutive_tool_calls_preserves_empty_reasoning_content() {
        let history = vec![tool_call_message("call_empty_reasoning", Some(""))];

        let merged = LLMManager::merge_consecutive_tool_calls(&history);

        match merged.first() {
            Some(MergedChatMessage::MergedToolCalls {
                thinking_content, ..
            }) => assert_eq!(thinking_content.as_deref(), Some("")),
            _ => panic!("expected merged tool call message"),
        }
    }

    #[test]
    fn merge_builtin_profile_user_aware_preserves_user_modified_fields() {
        let mut profiles = vec![profile(
            "builtin-deepseek-reasoner",
            "My Custom Label",
            "deepseek-reasoner-custom",
            false,
            false,
        )];
        let builtin = profile(
            "builtin-deepseek-reasoner",
            "DeepSeek Reasoner (深度推理)",
            "deepseek-reasoner",
            true,
            true,
        );
        let previous_builtin = profile(
            "builtin-deepseek-reasoner",
            "DeepSeek Reasoner (旧标签)",
            "deepseek-reasoner",
            true,
            true,
        );

        LLMManager::merge_builtin_profile_user_aware(
            &mut profiles,
            builtin,
            Some(&previous_builtin),
        );

        assert_eq!(profiles.len(), 1);
        let merged = &profiles[0];
        assert_eq!(merged.label, "My Custom Label");
        assert_eq!(merged.model, "deepseek-reasoner-custom");
        assert!(!merged.supports_tools);
        assert!(merged.is_builtin);
    }

    #[test]
    fn merge_builtin_profile_user_aware_updates_untouched_fields_from_builtin() {
        let mut profiles = vec![profile(
            "builtin-deepseek-chat",
            "DeepSeek Chat (旧标签)",
            "deepseek-chat",
            true,
            false,
        )];
        let mut builtin = profile(
            "builtin-deepseek-chat",
            "DeepSeek Chat (新标签)",
            "deepseek-chat",
            true,
            true,
        );
        builtin.temperature = 0.2;

        let mut previous_builtin = profile(
            "builtin-deepseek-chat",
            "DeepSeek Chat (旧标签)",
            "deepseek-chat",
            true,
            true,
        );
        previous_builtin.temperature = 0.7;
        profiles[0].temperature = 0.7;

        LLMManager::merge_builtin_profile_user_aware(
            &mut profiles,
            builtin,
            Some(&previous_builtin),
        );

        assert_eq!(profiles.len(), 1);
        let merged = &profiles[0];
        assert_eq!(merged.label, "DeepSeek Chat (新标签)");
        assert!((merged.temperature - 0.2).abs() < f32::EPSILON);
        assert!(merged.is_builtin);
    }

    #[test]
    fn merge_builtin_profile_user_aware_adds_missing_builtin_profile() {
        let mut profiles = vec![];
        let builtin = profile(
            "builtin-deepseek-chat",
            "DeepSeek Chat (对话)",
            "deepseek-chat",
            true,
            true,
        );

        LLMManager::merge_builtin_profile_user_aware(&mut profiles, builtin, None);

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "builtin-deepseek-chat");
        assert!(profiles[0].is_builtin);
    }

    #[test]
    fn apply_reasoning_config_removes_official_v4_thinking_ignored_sampling_params() {
        let mut body = deepseek_sampling_body("deepseek-v4-pro");
        let config = ApiConfig {
            provider_type: Some("deepseek".to_string()),
            provider_scope: Some("deepseek".to_string()),
            model_adapter: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            supports_reasoning: true,
            is_reasoning: true,
            thinking_enabled: true,
            enable_thinking: Some(true),
            ..Default::default()
        };

        LLMManager::apply_reasoning_config(&mut body, &config, None);

        let map = body
            .as_object()
            .expect("request body should stay an object");
        for key in [
            "temperature",
            "top_p",
            "presence_penalty",
            "frequency_penalty",
            "logprobs",
        ] {
            assert!(!map.contains_key(key), "{key} should be removed");
        }
        assert_eq!(
            map.get("thinking").and_then(|value| value.get("type")),
            Some(&json!("enabled"))
        );
    }

    #[test]
    fn apply_reasoning_config_removes_future_siliconflow_v4_thinking_ignored_sampling_params() {
        let mut body = deepseek_sampling_body("deepseek-ai/DeepSeek-V4-Pro");
        let config = ApiConfig {
            provider_type: Some("siliconflow".to_string()),
            provider_scope: Some("deepseek".to_string()),
            model_adapter: "deepseek".to_string(),
            model: "deepseek-ai/DeepSeek-V4-Pro".to_string(),
            base_url: "https://api.siliconflow.cn/v1".to_string(),
            supports_reasoning: true,
            is_reasoning: true,
            thinking_enabled: true,
            enable_thinking: Some(true),
            reasoning_effort: Some("max".to_string()),
            thinking_budget: Some(4096),
            ..Default::default()
        };

        LLMManager::apply_reasoning_config(&mut body, &config, None);

        let map = body
            .as_object()
            .expect("request body should stay an object");
        for key in [
            "temperature",
            "top_p",
            "presence_penalty",
            "frequency_penalty",
            "logprobs",
        ] {
            assert!(!map.contains_key(key), "{key} should be removed");
        }
        assert_eq!(map.get("enable_thinking"), Some(&json!(true)));
        assert_eq!(map.get("reasoning_effort"), Some(&json!("max")));
        assert!(!map.contains_key("thinking_budget"));
        assert!(!map.contains_key("thinking"));
    }

    #[test]
    fn apply_reasoning_config_maps_siliconflow_v32_depth_to_budget() {
        let mut body = deepseek_sampling_body("deepseek-ai/DeepSeek-V3.2");
        let config = ApiConfig {
            provider_type: Some("siliconflow".to_string()),
            provider_scope: Some("deepseek".to_string()),
            model_adapter: "deepseek".to_string(),
            model: "deepseek-ai/DeepSeek-V3.2".to_string(),
            base_url: "https://api.siliconflow.cn/v1".to_string(),
            supports_reasoning: true,
            is_reasoning: true,
            thinking_enabled: true,
            enable_thinking: Some(true),
            reasoning_effort: Some("xhigh".to_string()),
            ..Default::default()
        };

        LLMManager::apply_reasoning_config(&mut body, &config, None);

        let map = body
            .as_object()
            .expect("request body should stay an object");
        assert_eq!(map.get("enable_thinking"), Some(&json!(true)));
        assert_eq!(map.get("thinking_budget"), Some(&json!(32768)));
        assert!(!map.contains_key("reasoning_effort"));
    }

    #[test]
    fn apply_reasoning_config_keeps_siliconflow_v32_sampling_params() {
        let mut body = deepseek_sampling_body("deepseek-ai/DeepSeek-V3.2");
        let config = ApiConfig {
            provider_type: Some("siliconflow".to_string()),
            provider_scope: Some("deepseek".to_string()),
            model_adapter: "deepseek".to_string(),
            model: "deepseek-ai/DeepSeek-V3.2".to_string(),
            base_url: "https://api.siliconflow.cn/v1".to_string(),
            supports_reasoning: true,
            is_reasoning: true,
            thinking_enabled: true,
            enable_thinking: Some(true),
            thinking_budget: Some(4096),
            ..Default::default()
        };

        LLMManager::apply_reasoning_config(&mut body, &config, None);

        let map = body
            .as_object()
            .expect("request body should stay an object");
        for key in [
            "temperature",
            "top_p",
            "presence_penalty",
            "frequency_penalty",
        ] {
            assert!(map.contains_key(key), "{key} should be preserved");
        }
        assert_eq!(map.get("enable_thinking"), Some(&json!(true)));
        assert_eq!(map.get("thinking_budget"), Some(&json!(4096)));
        assert!(!map.contains_key("thinking"));
        assert!(!map.contains_key("reasoning_effort"));
    }

    #[test]
    fn merge_builtin_profile_user_aware_without_snapshot_syncs_capability_fields() {
        let mut profiles = vec![profile(
            "builtin-deepseek-chat",
            "User Local Label",
            "deepseek-chat-custom",
            false,
            false,
        )];
        let builtin = profile(
            "builtin-deepseek-chat",
            "DeepSeek Chat (官方)",
            "deepseek-chat",
            true,
            true,
        );

        LLMManager::merge_builtin_profile_user_aware(&mut profiles, builtin, None);

        let merged = &profiles[0];
        assert_eq!(merged.label, "User Local Label");
        assert_eq!(merged.model, "deepseek-chat-custom");
        assert!(merged.supports_tools);
        assert!(merged.is_builtin);
    }

    #[tokio::test]
    async fn get_model_profiles_drops_hidden_flag_for_new_builtin_model_without_history() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = create_test_llm_manager(&temp_dir);

        manager
            .db
            .save_setting("vendor_configs", "[]")
            .expect("seed vendor configs");
        manager
            .db
            .save_setting("model_profiles", "[]")
            .expect("seed model profiles");
        manager
            .db
            .save_setting(
                HIDDEN_BUILTIN_MODEL_PROFILES_KEY,
                r#"["builtin-gemini-3-flash"]"#,
            )
            .expect("seed hidden builtin ids");
        manager
            .db
            .save_setting(BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY, "[]")
            .expect("seed builtin snapshot");
        manager
            .db
            .save_setting("builtin_caps_migration_v2", "done")
            .expect("skip caps migration");

        let profiles = manager
            .get_model_profiles()
            .await
            .expect("load model profiles");

        assert!(profiles.iter().any(|profile| {
            profile.id == "builtin-gemini-3-flash" && profile.model == "gemini-3.5-flash"
        }));

        let hidden_ids = manager.read_hidden_builtin_model_profile_ids();
        assert!(
            !hidden_ids.contains("builtin-gemini-3-flash"),
            "new builtin model should not stay hidden without prior snapshot/user history"
        );
    }

    #[tokio::test]
    async fn get_model_profiles_drops_hidden_flag_when_builtin_slot_points_to_new_model() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = create_test_llm_manager(&temp_dir);

        manager
            .db
            .save_setting("vendor_configs", "[]")
            .expect("seed vendor configs");
        manager
            .db
            .save_setting("model_profiles", "[]")
            .expect("seed model profiles");
        manager
            .db
            .save_setting(
                HIDDEN_BUILTIN_MODEL_PROFILES_KEY,
                r#"["builtin-gemini-3-flash"]"#,
            )
            .expect("seed hidden builtin ids");
        manager
            .save_builtin_profile_snapshot(&[profile(
                "builtin-gemini-3-flash",
                "Gemini 3 Flash (均衡)",
                "gemini-3-flash-preview",
                true,
                true,
            )])
            .expect("seed legacy builtin snapshot");
        manager
            .db
            .save_setting("builtin_caps_migration_v2", "done")
            .expect("skip caps migration");

        let profiles = manager
            .get_model_profiles()
            .await
            .expect("load model profiles");

        assert!(profiles.iter().any(|profile| {
            profile.id == "builtin-gemini-3-flash" && profile.model == "gemini-3.5-flash"
        }));

        let hidden_ids = manager.read_hidden_builtin_model_profile_ids();
        assert!(
            !hidden_ids.contains("builtin-gemini-3-flash"),
            "repurposed builtin slot should be treated as a new model and unhidden"
        );
    }

    #[tokio::test]
    async fn save_model_profiles_does_not_hide_new_builtin_models() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = create_test_llm_manager(&temp_dir);

        manager
            .db
            .save_setting("vendor_configs", "[]")
            .expect("seed vendor configs");
        manager
            .db
            .save_setting("model_profiles", "[]")
            .expect("seed model profiles");

        let known_builtin = profile(
            "builtin-gemini-3-pro",
            "Gemini 3.1 Pro Preview (旗舰)",
            "gemini-3.1-pro-preview",
            true,
            true,
        );

        manager
            .save_builtin_profile_snapshot(&[known_builtin.clone()])
            .expect("seed builtin snapshot");

        manager
            .save_model_profiles(&[known_builtin])
            .await
            .expect("save user-visible builtin profiles");

        let hidden_ids = manager.read_hidden_builtin_model_profile_ids();
        assert!(
            !hidden_ids.contains("builtin-gemini-3-flash"),
            "new builtin model should not be inferred as hidden when absent from older saved profile sets"
        );
    }

    #[tokio::test]
    async fn save_model_profiles_does_not_hide_retargeted_builtin_slot() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = create_test_llm_manager(&temp_dir);

        manager
            .db
            .save_setting("vendor_configs", "[]")
            .expect("seed vendor configs");
        manager
            .db
            .save_setting("model_profiles", "[]")
            .expect("seed model profiles");

        manager
            .save_builtin_profile_snapshot(&[profile(
                "builtin-gemini-3-flash",
                "Gemini 3 Flash (均衡)",
                "gemini-3-flash-preview",
                true,
                true,
            )])
            .expect("seed legacy builtin snapshot");

        let known_builtin = profile(
            "builtin-gemini-3-pro",
            "Gemini 3.1 Pro Preview (旗舰)",
            "gemini-3.1-pro-preview",
            true,
            true,
        );

        manager
            .save_model_profiles(&[known_builtin])
            .await
            .expect("save user-visible builtin profiles");

        let hidden_ids = manager.read_hidden_builtin_model_profile_ids();
        assert!(
            !hidden_ids.contains("builtin-gemini-3-flash"),
            "legacy preview snapshot should not cause the retargeted 3.5 flash slot to be hidden"
        );
    }

    #[test]
    fn convert_openai_tool_call_treats_empty_string_arguments_as_empty_object() {
        let tool_call = json!({
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "group_list",
                "arguments": ""
            }
        });

        let converted = LLMManager::convert_openai_tool_call(&tool_call)
            .expect("empty string args should be accepted as no-arg call");
        assert_eq!(converted.tool_name, "group_list");
        assert_eq!(converted.args_json, json!({}));
    }

    #[test]
    fn convert_openai_tool_call_treats_whitespace_arguments_as_empty_object() {
        let tool_call = json!({
            "id": "call_2",
            "type": "function",
            "function": {
                "name": "tag_list_all",
                "arguments": "   \n\t  "
            }
        });

        let converted = LLMManager::convert_openai_tool_call(&tool_call)
            .expect("whitespace args should be accepted as no-arg call");
        assert_eq!(converted.tool_name, "tag_list_all");
        assert_eq!(converted.args_json, json!({}));
    }

    #[test]
    fn registry_inference_matches_siliconflow_glm_46v() {
        let inferred = LLMManager::infer_capability_overrides_from_registry(
            "zai-org/GLM-4.6V",
            Some("siliconflow"),
        )
        .expect("registry should infer GLM-4.6V capabilities");

        assert!(inferred.is_multimodal);
        assert!(inferred.supports_tools);
        assert!(!inferred.supports_reasoning);
    }

    #[test]
    fn registry_inference_matches_base_glm_46_without_scope() {
        let inferred =
            LLMManager::infer_capability_overrides_from_registry("glm-4.6", Some("zhipu"))
                .expect("registry should infer GLM-4.6 capabilities");

        assert!(!inferred.is_multimodal);
        assert!(inferred.supports_tools);
        assert!(inferred.supports_reasoning);
    }

    #[test]
    fn builtin_catalog_inference_covers_missing_builtin_tool_models() {
        for (model, scope) in [
            ("qwen3-max", Some("qwen")),
            ("qwen-plus", Some("qwen")),
            ("qwq-plus", Some("qwen")),
            ("glm-5", Some("zhipu")),
            ("kimi-latest", Some("moonshot")),
            ("MiniMax-M2.5", Some("minimax")),
            ("doubao-seed-2-0-pro-260215", Some("doubao")),
            ("gpt-5-mini", Some("openai")),
            ("o3-mini", Some("openai")),
            ("gemini-3.5-flash", Some("gemini")),
        ] {
            let inferred = LLMManager::resolve_capability_overrides(model, scope);
            assert!(
                inferred.supports_tools,
                "expected builtin catalog tool support for model {} / {:?}",
                model, scope
            );
        }
    }
}
