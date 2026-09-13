use crate::crypto::{CryptoService, EncryptedData};
use crate::models::AppError;
use crate::vendors::load_builtin_api_configs;
use std::collections::HashMap;
use uuid::Uuid;

use super::config_types::{
    default_model_adapter, default_gemini_api_version, default_max_output_tokens,
    default_temperature, looks_like_image_generation_model_id, normalize_vendor_protocol_config,
    resolve_preferred_protocol_for_provider, provider_supports_openai_responses, ApiConfig,
    ModelProfile, VendorConfig, normalize_model_profile_protocol_config,
};
type Result<T> = std::result::Result<T, AppError>;

impl super::LLMManager {
    /// 初始化供应商与模型条目结构，兼容旧版 api_configs
    pub async fn bootstrap_vendor_model_config(&self) -> Result<()> {
        let vendor_exists = self
            .db
            .get_setting("vendor_configs")
            .map_err(|e| AppError::database(format!("检测供应商配置失败: {}", e)))?
            .is_some();
        let profile_exists = self
            .db
            .get_setting("model_profiles")
            .map_err(|e| AppError::database(format!("检测模型条目失败: {}", e)))?
            .is_some();

        if vendor_exists && profile_exists {
            self.repair_vendor_model_protocol_settings().await?;
            return Ok(());
        }

        let legacy_str = self
            .db
            .get_setting("api_configs")
            .map_err(|e| AppError::database(format!("获取旧版API配置失败: {}", e)))?
            .unwrap_or_else(|| "[]".to_string());

        let mut legacy_configs = if legacy_str.trim().is_empty() || legacy_str.trim() == "[]" {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<ApiConfig>>(&legacy_str) {
                Ok(mut configs) => {
                    for config in &mut configs {
                        config.api_key = self.decrypt_api_key_if_needed(&config.api_key)?;
                    }
                    configs
                }
                Err(_) => {
                    log::info!("检测到旧版API配置格式，正在迁移到供应商结构...");
                    self.migrate_api_configs_legacy(&legacy_str).await?
                }
            }
        };

        if legacy_configs.is_empty() {
            self.save_vendor_model_configs(&[], &[]).await?;
            return Ok(());
        }

        let (mut vendors, mut profiles) = self
            .flatten_api_configs_to_vendor_profiles(&legacy_configs)
            .await?;
        legacy_configs.clear();

        vendors.retain(|v| !v.is_builtin);
        profiles.retain(|p| !p.is_builtin);

        self.save_vendor_model_configs(&vendors, &profiles).await?;
        Ok(())
    }

    async fn repair_vendor_model_protocol_settings(&self) -> Result<()> {
        let raw_vendors = self
            .db
            .get_setting("vendor_configs")
            .map_err(|e| AppError::database(format!("读取供应商配置失败: {}", e)))?
            .unwrap_or_else(|| "[]".to_string());
        let raw_profiles = self
            .db
            .get_setting("model_profiles")
            .map_err(|e| AppError::database(format!("读取模型条目失败: {}", e)))?
            .unwrap_or_else(|| "[]".to_string());

        let mut vendors: Vec<VendorConfig> = serde_json::from_str(&raw_vendors)
            .map_err(|e| AppError::configuration(format!("解析供应商配置失败: {}", e)))?;
        let mut profiles: Vec<ModelProfile> = serde_json::from_str(&raw_profiles)
            .map_err(|e| AppError::configuration(format!("解析模型条目失败: {}", e)))?;

        let mut changed = false;
        for vendor in &mut vendors {
            changed |= normalize_vendor_protocol_config(vendor);
        }
        let vendor_map: HashMap<String, VendorConfig> = vendors
            .iter()
            .cloned()
            .map(|vendor| (vendor.id.clone(), vendor))
            .collect();
        let mut vendor_map = vendor_map;
        if let Ok((builtin_vendors, _)) = self.load_builtin_vendor_profiles() {
            for vendor in builtin_vendors {
                vendor_map.entry(vendor.id.clone()).or_insert(vendor);
            }
        }
        for profile in &mut profiles {
            let vendor = vendor_map.get(&profile.vendor_id);
            changed |= normalize_model_profile_protocol_config(profile, vendor);
        }

        if changed {
            log::info!(
                "[VendorModel] Normalized persisted protocol settings for OpenAI-compatible providers"
            );
            self.save_vendor_model_configs(&vendors, &profiles).await?;
        }

        Ok(())
    }

    pub async fn get_vendor_configs(&self) -> Result<Vec<VendorConfig>> {
        self.bootstrap_vendor_model_config().await?;
        let mut vendors = self.read_user_vendor_configs().await?;
        if let Ok((builtin_vendors, _)) = self.load_builtin_vendor_profiles() {
            for mut vendor in builtin_vendors {
                if let Some(existing) = vendors.iter_mut().find(|v| v.id == vendor.id) {
                    existing.notes = vendor.notes.clone();
                    existing.name = vendor.name.clone();
                    existing.website_url = vendor.website_url.clone();
                    existing.is_builtin = true;
                    continue;
                }
                vendor.api_key = String::new();
                vendors.push(vendor);
            }
        }
        for vendor in &mut vendors {
            let is_builtin_vendor = vendor.is_builtin || vendor.id.starts_with("builtin-");
            if !is_builtin_vendor {
                continue;
            }
            let is_invalid = vendor.api_key.is_empty()
                || vendor.api_key == "***"
                || vendor.api_key.chars().all(|c| c == '*');
            if is_invalid {
                let secret_key = format!("{}.api_key", vendor.id);
                if let Ok(Some(key)) = self.db.get_secret(&secret_key) {
                    if !key.is_empty() {
                        vendor.api_key = key;
                    }
                }
                if vendor.id == "builtin-siliconflow" && vendor.api_key.is_empty() {
                    if let Ok(Some(sf_key)) = self.db.get_secret("siliconflow.api_key") {
                        if !sf_key.is_empty() {
                            vendor.api_key = sf_key;
                        }
                    }
                }
            }
            vendor.is_builtin = true;
        }
        Ok(vendors)
    }

    pub(crate) async fn read_user_vendor_configs(&self) -> Result<Vec<VendorConfig>> {
        let raw = self
            .db
            .get_setting("vendor_configs")
            .map_err(|e| AppError::database(format!("获取供应商配置失败: {}", e)))?
            .unwrap_or_else(|| "[]".to_string());

        let mut vendors: Vec<VendorConfig> = serde_json::from_str(&raw)
            .map_err(|e| AppError::configuration(format!("解析供应商配置失败: {}", e)))?;

        for vendor in &mut vendors {
            match self.decrypt_api_key_if_needed(&vendor.api_key) {
                Ok(decrypted) => {
                    let is_builtin_vendor = vendor.is_builtin || vendor.id.starts_with("builtin-");
                    if is_builtin_vendor && !decrypted.is_empty() {
                        let secret_key = format!("{}.api_key", vendor.id);
                        if let Err(e) = self.db.save_secret(&secret_key, &decrypted) {
                            tracing::warn!(
                                "⚠️ 迁移内置供应商 {} 的 API 密钥到安全存储失败: {}",
                                vendor.id,
                                e
                            );
                        } else {
                            tracing::info!(
                                "✅ 已迁移内置供应商 {} 的 API 密钥到安全存储",
                                vendor.id
                            );
                        }
                        vendor.api_key = String::new();
                        vendor.is_builtin = true;
                        continue;
                    }
                    vendor.api_key = decrypted;
                }
                Err(e) => {
                    tracing::warn!(
                        "⚠️ 供应商 {} 的 API 密钥解密失败，将清空密钥: {}",
                        vendor.id,
                        e
                    );
                    vendor.api_key = String::new();
                }
            }
            normalize_vendor_protocol_config(vendor);
        }
        Ok(vendors)
    }

    pub(crate) async fn vendor_configs_for_runtime(&self) -> Result<Vec<VendorConfig>> {
        let mut vendors = self.read_user_vendor_configs().await?;
        if let Ok((builtin_vendors, _)) = self.load_builtin_vendor_profiles() {
            for vendor in builtin_vendors {
                if vendors.iter().any(|v| v.id == vendor.id) {
                    continue;
                }
                vendors.push(vendor);
            }
        }

        for vendor in &mut vendors {
            let is_builtin_vendor = vendor.is_builtin || vendor.id.starts_with("builtin-");
            if is_builtin_vendor {
                let is_invalid = vendor.api_key.is_empty()
                    || vendor.api_key == "***"
                    || vendor.api_key.chars().all(|c| c == '*');
                if is_invalid {
                    let secret_key = format!("{}.api_key", vendor.id);
                    if let Ok(Some(key)) = self.db.get_secret(&secret_key) {
                        if !key.is_empty() {
                            vendor.api_key = key;
                        }
                    }
                    if vendor.id == "builtin-siliconflow" && vendor.api_key.is_empty() {
                        if let Ok(Some(sf_key)) = self.db.get_secret("siliconflow.api_key") {
                            if !sf_key.is_empty() {
                                vendor.api_key = sf_key;
                            }
                        }
                    }
                }
                vendor.is_builtin = true;
            }
        }

        Ok(vendors)
    }

    pub async fn save_vendor_configs(&self, configs: &[VendorConfig]) -> Result<()> {
        let existing_vendors = self.read_user_vendor_configs().await.unwrap_or_default();
        let existing_map: std::collections::HashMap<String, String> = existing_vendors
            .into_iter()
            .map(|v| (v.id.clone(), v.api_key))
            .collect();

        let mut sanitized = Vec::new();
        for cfg in configs {
            let mut clone = cfg.clone();
            normalize_vendor_protocol_config(&mut clone);

            let trimmed = cfg.api_key.trim();
            let keep_placeholder =
                trimmed == "***" || (!trimmed.is_empty() && trimmed.chars().all(|c| c == '*'));
            let is_builtin_vendor = cfg.is_builtin || cfg.id.starts_with("builtin-");

            if is_builtin_vendor {
                let secret_key = format!("{}.api_key", cfg.id);
                if keep_placeholder {
                    // no-op
                } else if trimmed.is_empty() {
                    self.db.delete_secret(&secret_key).map_err(|e| {
                        AppError::database(format!(
                            "Failed to clear builtin vendor API key for {}: {}",
                            cfg.id, e
                        ))
                    })?;
                    if cfg.id == "builtin-siliconflow" {
                        self.db.delete_secret("siliconflow.api_key").map_err(|e| {
                            AppError::database(format!(
                                "Failed to clear SiliconFlow compatibility key: {}",
                                e
                            ))
                        })?;
                    }
                } else {
                    self.db.save_secret(&secret_key, trimmed).map_err(|e| {
                        AppError::database(format!("Failed to save builtin vendor API key: {}", e))
                    })?;
                    if cfg.id == "builtin-siliconflow" {
                        self.db
                            .save_secret("siliconflow.api_key", trimmed)
                            .map_err(|e| {
                                AppError::database(format!(
                                    "Failed to save SiliconFlow compatibility key: {}",
                                    e
                                ))
                            })?;
                    }
                }
                clone.api_key = String::new();
                clone.is_builtin = true;
            } else {
                let effective_api_key = if keep_placeholder {
                    existing_map.get(&cfg.id).cloned().unwrap_or_default()
                } else {
                    trimmed.to_string()
                };
                clone.api_key = self.encrypt_api_key(&effective_api_key)?;
                clone.is_read_only = false;
            }
            sanitized.push(clone);
        }

        let json = serde_json::to_string(&sanitized)
            .map_err(|e| AppError::configuration(format!("序列化供应商配置失败: {}", e)))?;
        self.db
            .save_setting("vendor_configs", &json)
            .map_err(|e| AppError::database(format!("保存供应商配置失败: {}", e)))?;
        self.invalidate_api_configs_cache().await; // A6#1: 配置写入失效缓存
        Ok(())
    }

    pub async fn save_vendor_model_configs(
        &self,
        vendors: &[VendorConfig],
        profiles: &[ModelProfile],
    ) -> Result<()> {
        self.save_vendor_configs(vendors).await?;
        self.save_model_profiles(profiles).await?;
        Ok(())
    }

    pub async fn save_api_configurations(&self, configs: &[ApiConfig]) -> Result<()> {
        self.bootstrap_vendor_model_config().await?;

        let mut plain_configs: Vec<ApiConfig> = configs
            .iter()
            .filter(|cfg| !cfg.is_builtin)
            .cloned()
            .collect();

        for cfg in &mut plain_configs {
            cfg.api_key = self.decrypt_api_key_if_needed(&cfg.api_key)?;
        }

        let (mut vendors, mut profiles) = self
            .flatten_api_configs_to_vendor_profiles(&plain_configs)
            .await?;

        vendors.retain(|v| !v.is_builtin);
        profiles.retain(|p| !p.is_builtin);

        self.save_vendor_model_configs(&vendors, &profiles).await
    }

    fn encrypt_api_key(&self, api_key: &str) -> Result<String> {
        if CryptoService::is_encrypted_format(api_key) {
            return Ok(api_key.to_string());
        }

        let encrypted_data = self
            .crypto_service
            .encrypt_api_key(api_key)
            .map_err(|e| AppError::configuration(format!("加密API密钥失败: {}", e)))?;

        serde_json::to_string(&encrypted_data)
            .map_err(|e| AppError::configuration(format!("序列化加密数据失败: {}", e)))
    }

    pub(crate) fn decrypt_api_key_if_needed(&self, api_key: &str) -> Result<String> {
        if CryptoService::is_encrypted_format(api_key) {
            let encrypted_data: EncryptedData = serde_json::from_str(api_key)
                .map_err(|e| AppError::configuration(format!("解析加密数据失败: {}", e)))?;

            self.crypto_service
                .decrypt_api_key(&encrypted_data)
                .map_err(|e| AppError::configuration(format!("解密API密钥失败: {}", e)))
        } else {
            Ok(api_key.to_string())
        }
    }

    pub fn decrypt_api_key(&self, api_key: &str) -> Result<String> {
        self.decrypt_api_key_if_needed(api_key)
    }

    pub(crate) fn load_builtin_vendor_profiles(&self) -> Result<(Vec<VendorConfig>, Vec<ModelProfile>)> {
        let mut vendors = Vec::new();
        let mut profiles = Vec::new();

        let builtin = match load_builtin_api_configs() {
            Ok(configs) => configs,
            Err(err) => {
                log::error!("[VendorModel] 加载内置模型配置失败: {}", err);
                Vec::new()
            }
        };
        for cfg in builtin {
            let is_siliconflow = cfg.base_url.to_lowercase().contains("siliconflow");
            let vendor_id = if is_siliconflow {
                "builtin-siliconflow".to_string()
            } else {
                format!("builtin-{}", cfg.id)
            };
            let vendor_name = if is_siliconflow {
                "SiliconFlow".to_string()
            } else {
                cfg.name.clone()
            };
            if !vendors.iter().any(|v: &VendorConfig| v.id == vendor_id) {
                vendors.push(VendorConfig {
                    id: vendor_id.clone(),
                    name: vendor_name,
                    provider_type: if is_siliconflow {
                        "siliconflow".to_string()
                    } else {
                        cfg.model_adapter.clone()
                    },
                    api_protocol: cfg.api_protocol.clone(),
                    supports_openai_responses: cfg.supports_openai_responses,
                    base_url: cfg.base_url.clone(),
                    api_key: cfg.api_key.clone(),
                    headers: cfg.headers.clone().unwrap_or_default(),
                    rate_limit_per_minute: None,
                    default_timeout_ms: None,
                    notes: None,
                    is_builtin: true,
                    is_read_only: true,
                    sort_order: None,
                    max_tokens_limit: cfg.max_tokens_limit,
                    website_url: None,
                });
            }
            profiles.push(ModelProfile {
                id: cfg.id.clone(),
                vendor_id: vendor_id.clone(),
                label: cfg.name.clone(),
                model: cfg.model.clone(),
                provider_scope: cfg
                    .provider_scope
                    .clone()
                    .or_else(|| cfg.provider_type.clone()),
                api_protocol: cfg.api_protocol.clone(),
                model_adapter: cfg.model_adapter.clone(),
                is_multimodal: cfg.is_multimodal,
                is_reasoning: cfg.is_reasoning,
                is_embedding: cfg.is_embedding,
                is_reranker: cfg.is_reranker,
                is_image_generation: cfg.is_image_generation,
                supports_tools: cfg.supports_tools,
                supports_reasoning: cfg.supports_reasoning || cfg.is_reasoning,
                status: if cfg.enabled {
                    "enabled".to_string()
                } else {
                    "disabled".to_string()
                },
                enabled: cfg.enabled,
                max_output_tokens: cfg.max_output_tokens,
                temperature: cfg.temperature,
                reasoning_effort: cfg.reasoning_effort.clone(),
                thinking_enabled: cfg.thinking_enabled,
                thinking_budget: cfg.thinking_budget,
                include_thoughts: cfg.include_thoughts,
                enable_thinking: cfg.enable_thinking,
                min_p: cfg.min_p,
                top_k: cfg.top_k,
                gemini_api_version: Some(cfg.gemini_api_version.clone()),
                is_builtin: true,
                is_favorite: cfg.is_favorite,
                max_tokens_limit: cfg.max_tokens_limit,
                context_window: cfg.context_window,
                repetition_penalty: cfg.repetition_penalty,
                reasoning_split: cfg.reasoning_split,
                effort: cfg.effort.clone(),
                verbosity: cfg.verbosity.clone(),
            });
        }

        let existing_vendor_ids: Vec<String> = vendors.iter().map(|v| v.id.clone()).collect();
        let existing_profile_ids: Vec<String> = profiles.iter().map(|p| p.id.clone()).collect();

        let (new_vendors, new_profiles) =
            crate::llm_manager::builtin_vendors::load_all_builtins(&existing_vendor_ids, &existing_profile_ids);

        vendors.extend(new_vendors);
        profiles.extend(new_profiles);

        Ok((vendors, profiles))
    }

    async fn migrate_api_configs_legacy(&self, old_config_str: &str) -> Result<Vec<ApiConfig>> {
        #[derive(serde::Deserialize)]
        struct OldApiConfigV2 {
            id: String,
            name: String,
            api_key: String,
            base_url: String,
            model: String,
            is_multimodal: bool,
            is_reasoning: bool,
            enabled: bool,
        }

        #[derive(serde::Deserialize)]
        struct OldApiConfigV1 {
            id: String,
            name: String,
            api_key: String,
            base_url: String,
            model: String,
            is_multimodal: bool,
            enabled: bool,
        }

        if let Ok(old_configs) = serde_json::from_str::<Vec<OldApiConfigV2>>(old_config_str) {
            return Ok(old_configs
                .into_iter()
                .map(|old| ApiConfig {
                    id: old.id,
                    name: old.name,
                    vendor_id: None,
                    vendor_name: None,
                    provider_type: None,
                    provider_scope: None,
                    api_protocol: Some(resolve_preferred_protocol_for_provider(
                        None,
                        Some(default_model_adapter().as_str()),
                        &old.base_url,
                        None,
                    )),
                    supports_openai_responses: Some(provider_supports_openai_responses(
                        None,
                        &old.base_url,
                        None,
                    )),
                    api_key: old.api_key,
                    base_url: old.base_url,
                    model: old.model,
                    is_multimodal: old.is_multimodal,
                    is_reasoning: old.is_reasoning,
                    is_embedding: false,
                    is_reranker: false,
                    is_image_generation: false,
                    enabled: old.enabled,
                    model_adapter: default_model_adapter(),
                    max_output_tokens: default_max_output_tokens(),
                    temperature: default_temperature(),
                    supports_tools: false,
                    gemini_api_version: default_gemini_api_version(),
                    min_p: None,
                    top_k: None,
                    enable_thinking: None,
                    is_builtin: false,
                    is_read_only: false,
                    reasoning_effort: None,
                    thinking_enabled: false,
                    thinking_budget: None,
                    include_thoughts: false,
                    supports_reasoning: old.is_reasoning,
                    headers: None,
                    top_p_override: None,
                    frequency_penalty_override: None,
                    presence_penalty_override: None,
                    repetition_penalty: None,
                    reasoning_split: None,
                    effort: None,
                    verbosity: None,
                    is_favorite: false,
                    max_tokens_limit: None,
                    context_window: None,
                })
                .collect());
        }

        let old_configs: Vec<OldApiConfigV1> = serde_json::from_str(old_config_str)
            .map_err(|e| AppError::configuration(format!("解析旧版API配置失败: {}", e)))?;

        Ok(old_configs
            .into_iter()
            .map(|old| ApiConfig {
                id: old.id,
                name: old.name,
                vendor_id: None,
                vendor_name: None,
                provider_type: None,
                provider_scope: None,
                api_protocol: Some(resolve_preferred_protocol_for_provider(
                    None,
                    Some(default_model_adapter().as_str()),
                    &old.base_url,
                    None,
                )),
                supports_openai_responses: Some(provider_supports_openai_responses(
                    None,
                    &old.base_url,
                    None,
                )),
                api_key: old.api_key,
                base_url: old.base_url,
                model: old.model,
                is_multimodal: old.is_multimodal,
                is_reasoning: false,
                is_embedding: false,
                is_reranker: false,
                is_image_generation: false,
                enabled: old.enabled,
                model_adapter: default_model_adapter(),
                max_output_tokens: default_max_output_tokens(),
                temperature: default_temperature(),
                supports_tools: false,
                gemini_api_version: default_gemini_api_version(),
                min_p: None,
                top_k: None,
                enable_thinking: None,
                is_builtin: false,
                is_read_only: false,
                reasoning_effort: None,
                thinking_enabled: false,
                thinking_budget: None,
                include_thoughts: false,
                supports_reasoning: false,
                headers: None,
                top_p_override: None,
                frequency_penalty_override: None,
                presence_penalty_override: None,
                is_favorite: false,
                max_tokens_limit: None,
                context_window: None,
                repetition_penalty: None,
                reasoning_split: None,
                effort: None,
                verbosity: None,
            })
            .collect())
    }

    async fn flatten_api_configs_to_vendor_profiles(
        &self,
        configs: &[ApiConfig],
    ) -> Result<(Vec<VendorConfig>, Vec<ModelProfile>)> {
        let mut vendors_map: HashMap<String, VendorConfig> = HashMap::new();
        let mut profiles: Vec<ModelProfile> = Vec::new();

        for cfg in configs {
            let provider_scope = cfg
                .provider_scope
                .clone()
                .or_else(|| cfg.provider_type.clone());
            let capability_overrides =
                Self::resolve_capability_overrides(&cfg.model, provider_scope.as_deref());
            let base_key = format!("{}::{}", cfg.base_url.trim(), cfg.api_key.trim());
            let key = cfg
                .vendor_id
                .clone()
                .unwrap_or_else(|| format!("auto::{}", base_key));
            let vendor_entry = vendors_map.entry(key.clone()).or_insert_with(|| {
                let provider_type = cfg
                    .provider_type
                    .clone()
                    .unwrap_or_else(|| cfg.model_adapter.clone());
                let vendor_id = cfg
                    .vendor_id
                    .clone()
                    .or_else(|| Some(format!("vendor-{}", Uuid::new_v4())))
                    .unwrap();
                VendorConfig {
                    id: vendor_id,
                    name: cfg
                        .vendor_name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| cfg.name.clone()),
                    provider_type,
                    api_protocol: cfg.api_protocol.clone(),
                    supports_openai_responses: cfg.supports_openai_responses,
                    base_url: cfg.base_url.clone(),
                    api_key: cfg.api_key.clone(),
                    headers: cfg.headers.clone().unwrap_or_default(),
                    rate_limit_per_minute: None,
                    default_timeout_ms: None,
                    notes: None,
                    is_builtin: cfg.is_builtin,
                    is_read_only: cfg.is_read_only,
                    sort_order: None,
                    max_tokens_limit: cfg.max_tokens_limit,
                    website_url: None,
                }
            });
            let vendor_id = vendor_entry.id.clone();

            profiles.push(ModelProfile {
                id: cfg.id.clone(),
                vendor_id,
                label: cfg.name.clone(),
                model: cfg.model.clone(),
                provider_scope,
                api_protocol: cfg.api_protocol.clone(),
                model_adapter: cfg.model_adapter.clone(),
                is_multimodal: cfg.is_multimodal || capability_overrides.is_multimodal,
                is_reasoning: cfg.is_reasoning,
                is_embedding: cfg.is_embedding,
                is_reranker: cfg.is_reranker,
                is_image_generation: cfg.is_image_generation
                    || looks_like_image_generation_model_id(&cfg.model),
                supports_tools: cfg.supports_tools || capability_overrides.supports_tools,
                supports_reasoning: cfg.supports_reasoning
                    || cfg.is_reasoning
                    || capability_overrides.supports_reasoning,
                status: if cfg.enabled {
                    "enabled".to_string()
                } else {
                    "disabled".to_string()
                },
                enabled: cfg.enabled,
                max_output_tokens: cfg.max_output_tokens,
                temperature: cfg.temperature,
                reasoning_effort: cfg.reasoning_effort.clone(),
                thinking_enabled: cfg.thinking_enabled,
                thinking_budget: cfg.thinking_budget,
                include_thoughts: cfg.include_thoughts,
                enable_thinking: cfg.enable_thinking,
                min_p: cfg.min_p,
                top_k: cfg.top_k,
                gemini_api_version: Some(cfg.gemini_api_version.clone()),
                is_builtin: cfg.is_builtin,
                is_favorite: cfg.is_favorite,
                max_tokens_limit: cfg.max_tokens_limit,
                context_window: cfg.context_window.or(capability_overrides.context_window),
                repetition_penalty: cfg.repetition_penalty,
                reasoning_split: cfg.reasoning_split,
                effort: cfg.effort.clone(),
                verbosity: cfg.verbosity.clone(),
            });
        }

        Ok((vendors_map.into_values().collect(), profiles))
    }
}
