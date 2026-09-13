use crate::models::{AppError, ModelAssignments};
use std::collections::{HashMap, HashSet};

use super::config_types::{
    default_gemini_api_version, looks_like_image_generation_model_id,
    normalize_model_profile_protocol_config, resolve_preferred_protocol_for_provider, ApiConfig,
    CapabilityOverrides, ModelProfile, RegistryModelRecord, ResolvedModelConfig, VendorConfig,
    BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY, HIDDEN_BUILTIN_MODEL_PROFILES_KEY,
    MODEL_CAPABILITY_REGISTRY, OcrModelConfig, effective_max_tokens,
};

type Result<T> = std::result::Result<T, AppError>;

impl super::LLMManager {
    // ==================== Capability inference ====================

    fn infer_capability_overrides_from_builtin_catalog(
        model_id: &str,
        provider_scope: Option<&str>,
    ) -> Option<CapabilityOverrides> {
        let normalized_model = Self::normalize_model_id(model_id);
        let requested_scope = Self::normalize_provider_scope(provider_scope);

        let vendor_scope_by_id: HashMap<&str, &str> = crate::llm_manager::builtin_vendors::BUILTIN_VENDORS
            .iter()
            .map(|vendor| (vendor.id, vendor.provider_type))
            .collect();

        let mut best_match: Option<CapabilityOverrides> = None;
        for model in crate::llm_manager::builtin_vendors::BUILTIN_MODELS.iter() {
            if Self::normalize_model_id(model.model) != normalized_model {
                continue;
            }

            let builtin_scope = vendor_scope_by_id
                .get(model.vendor_id)
                .copied()
                .and_then(|scope| Self::normalize_provider_scope(Some(scope)));
            if let Some(requested) = requested_scope.as_deref() {
                if builtin_scope.as_deref() != Some(requested) {
                    continue;
                }
            }

            best_match = Some(CapabilityOverrides {
                is_multimodal: model.is_multimodal,
                supports_tools: model.supports_tools,
                supports_reasoning: model.is_reasoning,
                context_window: crate::llm_manager::builtin_vendors::deepseek_context_window(model.model),
            });
            break;
        }

        best_match
    }

    fn normalize_model_id(value: &str) -> String {
        value.trim().to_lowercase()
    }

    fn normalize_provider_scope(value: Option<&str>) -> Option<String> {
        value
            .map(|scope| scope.trim().to_lowercase())
            .filter(|scope| !scope.is_empty())
    }

    fn split_model_name(value: &str) -> Vec<String> {
        Self::normalize_model_id(value)
            .split(['/', ':', '\\'])
            .map(|part| part.to_string())
            .collect()
    }

    fn base_model_id(value: &str) -> String {
        Self::split_model_name(value)
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn matches_full_model_id(input: &str, candidate: Option<&str>) -> bool {
        let Some(candidate) = candidate else {
            return false;
        };
        let normalized = Self::normalize_model_id(candidate);
        input == normalized
            || input.ends_with(&format!("/{}", normalized))
            || input.ends_with(&format!(":{}", normalized))
    }

    fn registry_record_score(
        model_id: &str,
        provider_scope: Option<&str>,
        record: &RegistryModelRecord,
    ) -> i32 {
        let normalized_input = Self::normalize_model_id(model_id);
        if normalized_input.is_empty() {
            return -1;
        }

        let base_model_id = Self::base_model_id(model_id);
        let requested_scope = Self::normalize_provider_scope(provider_scope);
        let record_scope = Self::normalize_provider_scope(record.provider_scope.as_deref());

        let mut score = if Self::matches_full_model_id(
            &normalized_input,
            record.provider_model_id.as_deref(),
        ) {
            500
        } else if Self::matches_full_model_id(&normalized_input, Some(&record.model_id)) {
            450
        } else if Self::matches_full_model_id(&normalized_input, record.alias_of.as_deref()) {
            430
        } else if record
            .provider_model_id
            .as_deref()
            .map(|value| Self::base_model_id(value) == base_model_id)
            .unwrap_or(false)
        {
            320
        } else if Self::base_model_id(&record.model_id) == base_model_id {
            300
        } else if record
            .alias_of
            .as_deref()
            .map(|value| Self::base_model_id(value) == base_model_id)
            .unwrap_or(false)
        {
            280
        } else {
            -1
        };

        if score < 0 {
            return score;
        }

        if let Some(requested_scope) = requested_scope {
            if record_scope.as_deref() == Some(requested_scope.as_str()) {
                score += 40;
            } else if record_scope.is_none() {
                score += 10;
            }
        } else if record_scope.is_none() {
            score += 20;
        }

        score
    }

    pub(crate) fn infer_capability_overrides_from_registry(
        model_id: &str,
        provider_scope: Option<&str>,
    ) -> Option<CapabilityOverrides> {
        let mut best_record: Option<&RegistryModelRecord> = None;
        let mut best_score = -1;

        for record in MODEL_CAPABILITY_REGISTRY.iter() {
            let score = Self::registry_record_score(model_id, provider_scope, record);
            if score > best_score {
                best_score = score;
                best_record = Some(record);
            }
        }

        best_record.map(|record| CapabilityOverrides {
            is_multimodal: record.capabilities.vision,
            supports_tools: record.capabilities.function_calling,
            supports_reasoning: record.capabilities.reasoning,
            context_window: record.capabilities.max_context_tokens,
        })
    }

    pub(crate) fn resolve_capability_overrides(
        model_id: &str,
        provider_scope: Option<&str>,
    ) -> CapabilityOverrides {
        let registry = Self::infer_capability_overrides_from_registry(model_id, provider_scope)
            .unwrap_or_default();
        let builtin =
            Self::infer_capability_overrides_from_builtin_catalog(model_id, provider_scope)
                .unwrap_or_default();

        CapabilityOverrides {
            is_multimodal: registry.is_multimodal || builtin.is_multimodal,
            supports_tools: registry.supports_tools || builtin.supports_tools,
            supports_reasoning: registry.supports_reasoning || builtin.supports_reasoning,
            context_window: registry.context_window.or(builtin.context_window),
        }
    }

    // ==================== Profile merging ====================

    pub(crate) fn merge_builtin_profile_user_aware(
        profiles: &mut Vec<ModelProfile>,
        builtin_profile: ModelProfile,
        previous_builtin_profile: Option<&ModelProfile>,
    ) {
        if let Some(existing) = profiles.iter_mut().find(|p| p.id == builtin_profile.id) {
            existing.is_builtin = true;

            let Some(previous_builtin) = previous_builtin_profile else {
                existing.is_multimodal = builtin_profile.is_multimodal;
                existing.is_reasoning = builtin_profile.is_reasoning;
                existing.is_embedding = builtin_profile.is_embedding;
                existing.is_reranker = builtin_profile.is_reranker;
                existing.supports_tools = builtin_profile.supports_tools;
                existing.supports_reasoning = builtin_profile.supports_reasoning;
                return;
            };

            macro_rules! update_if_untouched {
                ($field:ident) => {
                    if existing.$field == previous_builtin.$field {
                        existing.$field = builtin_profile.$field.clone();
                    }
                };
            }

            update_if_untouched!(vendor_id);
            update_if_untouched!(label);
            update_if_untouched!(model);
            update_if_untouched!(model_adapter);
            update_if_untouched!(is_multimodal);
            update_if_untouched!(is_reasoning);
            update_if_untouched!(is_embedding);
            update_if_untouched!(is_reranker);
            update_if_untouched!(supports_tools);
            update_if_untouched!(supports_reasoning);
            update_if_untouched!(status);
            update_if_untouched!(enabled);
            update_if_untouched!(max_output_tokens);
            update_if_untouched!(temperature);
            update_if_untouched!(reasoning_effort);
            update_if_untouched!(thinking_enabled);
            update_if_untouched!(thinking_budget);
            update_if_untouched!(include_thoughts);
            update_if_untouched!(enable_thinking);
            update_if_untouched!(min_p);
            update_if_untouched!(top_k);
            update_if_untouched!(gemini_api_version);
            update_if_untouched!(repetition_penalty);
            update_if_untouched!(reasoning_split);
            update_if_untouched!(effort);
            update_if_untouched!(verbosity);
            update_if_untouched!(is_favorite);
            update_if_untouched!(max_tokens_limit);
            update_if_untouched!(context_window);
            return;
        }
        profiles.push(builtin_profile);
    }

    fn merge_vendor_profile(
        &self,
        vendor: &VendorConfig,
        profile: &ModelProfile,
    ) -> Result<ResolvedModelConfig> {
        let api_key = if vendor.is_builtin {
            vendor.api_key.trim().to_string()
        } else {
            self.decrypt_api_key_if_needed(&vendor.api_key)?
                .trim()
                .to_string()
        };

        let has_api_key =
            !api_key.is_empty() && api_key != "***" && !api_key.chars().all(|c| c == '*');

        let provider_scope = profile
            .provider_scope
            .clone()
            .or_else(|| Some(vendor.provider_type.clone()));
        let capability_overrides =
            Self::resolve_capability_overrides(&profile.model, provider_scope.as_deref());

        let runtime = ApiConfig {
            id: profile.id.clone(),
            name: profile.label.clone(),
            vendor_id: Some(vendor.id.clone()),
            vendor_name: Some(vendor.name.clone()),
            provider_type: Some(vendor.provider_type.clone()),
            provider_scope,
            api_protocol: profile
                .api_protocol
                .clone()
                .or_else(|| vendor.api_protocol.clone())
                .or_else(|| {
                    Some(resolve_preferred_protocol_for_provider(
                        Some(vendor.provider_type.as_str()),
                        Some(profile.model_adapter.as_str()),
                        &vendor.base_url,
                        vendor.supports_openai_responses,
                    ))
                }),
            api_key,
            base_url: vendor.base_url.clone(),
            model: profile.model.clone(),
            is_multimodal: profile.is_multimodal || capability_overrides.is_multimodal,
            is_reasoning: profile.is_reasoning,
            is_embedding: profile.is_embedding,
            is_reranker: profile.is_reranker,
            is_image_generation: profile.is_image_generation
                || looks_like_image_generation_model_id(&profile.model),
            enabled: profile.enabled && profile.status.to_lowercase() != "disabled" && has_api_key,
            model_adapter: profile.model_adapter.clone(),
            max_output_tokens: profile.max_output_tokens,
            temperature: profile.temperature,
            supports_tools: profile.supports_tools || capability_overrides.supports_tools,
            gemini_api_version: profile
                .gemini_api_version
                .clone()
                .unwrap_or_else(default_gemini_api_version),
            is_builtin: profile.is_builtin || vendor.is_builtin,
            is_read_only: vendor.is_read_only,
            reasoning_effort: profile.reasoning_effort.clone(),
            thinking_enabled: profile.thinking_enabled,
            thinking_budget: profile.thinking_budget,
            include_thoughts: profile.include_thoughts,
            min_p: profile.min_p,
            top_k: profile.top_k,
            enable_thinking: profile.enable_thinking,
            supports_reasoning: profile.supports_reasoning
                || profile.is_reasoning
                || capability_overrides.supports_reasoning,
            headers: Some(vendor.headers.clone()),
            top_p_override: None,
            frequency_penalty_override: None,
            presence_penalty_override: None,
            repetition_penalty: profile.repetition_penalty,
            reasoning_split: profile.reasoning_split,
            effort: profile.effort.clone(),
            verbosity: profile.verbosity.clone(),
            is_favorite: profile.is_favorite,
            max_tokens_limit: profile.max_tokens_limit,
            context_window: profile
                .context_window
                .or(capability_overrides.context_window),
            supports_openai_responses: vendor.supports_openai_responses,
        };

        Ok(ResolvedModelConfig {
            vendor: vendor.clone(),
            profile: profile.clone(),
            runtime,
        })
    }

    // ==================== Snapshot management ====================

    fn read_builtin_profile_snapshot_map(&self) -> HashMap<String, ModelProfile> {
        let raw = match self.db.get_setting(BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY) {
            Ok(Some(raw)) => raw,
            _ => return HashMap::new(),
        };

        let parsed: Vec<ModelProfile> = match serde_json::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                log::warn!("[VendorModel] 解析内置模型快照失败，回退为空快照: {}", err);
                return HashMap::new();
            }
        };

        parsed
            .into_iter()
            .map(|profile| (profile.id.clone(), profile))
            .collect()
    }

    pub(crate) fn save_builtin_profile_snapshot(&self, builtin_profiles: &[ModelProfile]) -> Result<()> {
        let json = serde_json::to_string(builtin_profiles)
            .map_err(|e| AppError::configuration(format!("序列化内置模型快照失败: {}", e)))?;
        self.db
            .save_setting(BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY, &json)
            .map_err(|e| AppError::database(format!("保存内置模型快照失败: {}", e)))
    }

    pub(crate) fn read_hidden_builtin_model_profile_ids(&self) -> HashSet<String> {
        let raw = match self.db.get_setting(HIDDEN_BUILTIN_MODEL_PROFILES_KEY) {
            Ok(Some(raw)) => raw,
            _ => return HashSet::new(),
        };

        match serde_json::from_str::<Vec<String>>(&raw) {
            Ok(ids) => ids
                .into_iter()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect(),
            Err(err) => {
                log::warn!(
                    "[VendorModel] 解析隐藏内置模型列表失败，回退为空列表: {}",
                    err
                );
                HashSet::new()
            }
        }
    }

    fn save_hidden_builtin_model_profile_ids(&self, ids: &HashSet<String>) -> Result<()> {
        let mut sorted_ids: Vec<String> = ids
            .iter()
            .filter(|id| !id.trim().is_empty())
            .cloned()
            .collect();
        sorted_ids.sort();
        let json = serde_json::to_string(&sorted_ids)
            .map_err(|e| AppError::configuration(format!("序列化隐藏内置模型列表失败: {}", e)))?;
        self.db
            .save_setting(HIDDEN_BUILTIN_MODEL_PROFILES_KEY, &json)
            .map_err(|e| AppError::database(format!("保存隐藏内置模型列表失败: {}", e)))
    }

    fn reconcile_hidden_builtin_model_profile_ids(
        &self,
        builtin_profiles: &[ModelProfile],
        user_profiles: &[ModelProfile],
        hidden_builtin_ids: &mut HashSet<String>,
        snapshot_map: &HashMap<String, ModelProfile>,
    ) -> Result<bool> {
        let builtin_id_set: HashSet<&str> = builtin_profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect();
        let builtin_profile_map: HashMap<&str, &ModelProfile> = builtin_profiles
            .iter()
            .map(|profile| (profile.id.as_str(), profile))
            .collect();
        let known_user_builtin_models: HashMap<&str, &str> = user_profiles
            .iter()
            .filter_map(|profile| {
                builtin_id_set
                    .contains(profile.id.as_str())
                    .then_some((profile.id.as_str(), profile.model.as_str()))
            })
            .collect();

        let original = hidden_builtin_ids.clone();
        hidden_builtin_ids.retain(|id| {
            let Some(current_builtin) = builtin_profile_map.get(id.as_str()) else {
                return false;
            };

            let snapshot_matches = snapshot_map
                .get(id)
                .map(|snapshot| snapshot.model == current_builtin.model)
                .unwrap_or(false);
            let user_model_matches = known_user_builtin_models
                .get(id.as_str())
                .map(|model| *model == current_builtin.model.as_str())
                .unwrap_or(false);

            snapshot_matches || user_model_matches
        });

        if *hidden_builtin_ids != original {
            self.save_hidden_builtin_model_profile_ids(hidden_builtin_ids)?;
            return Ok(true);
        }

        Ok(false)
    }

    // ==================== Profile CRUD ====================

    pub async fn get_model_profiles(&self) -> Result<Vec<ModelProfile>> {
        self.bootstrap_vendor_model_config().await?;
        let mut profiles = self.read_user_model_profiles().await?;
        let mut hidden_builtin_ids = self.read_hidden_builtin_model_profile_ids();

        const BUILTIN_CAPS_MIGRATION_KEY: &str = "builtin_caps_migration_v2";
        if self
            .db
            .get_setting(BUILTIN_CAPS_MIGRATION_KEY)
            .ok()
            .flatten()
            .is_none()
        {
            if let Ok((_, builtin_list)) = self.load_builtin_vendor_profiles() {
                let builtin_map: HashMap<String, &ModelProfile> =
                    builtin_list.iter().map(|p| (p.id.clone(), p)).collect();
                let mut patched = false;
                for profile in &mut profiles {
                    if let Some(builtin) = builtin_map.get(&profile.id) {
                        if profile.supports_tools != builtin.supports_tools
                            || profile.is_multimodal != builtin.is_multimodal
                            || profile.is_reasoning != builtin.is_reasoning
                            || profile.supports_reasoning != builtin.supports_reasoning
                        {
                            profile.is_multimodal = builtin.is_multimodal;
                            profile.is_reasoning = builtin.is_reasoning;
                            profile.is_embedding = builtin.is_embedding;
                            profile.is_reranker = builtin.is_reranker;
                            profile.supports_tools = builtin.supports_tools;
                            profile.supports_reasoning = builtin.supports_reasoning;
                            patched = true;
                            log::info!(
                                "[VendorModel] 迁移: {} 能力字段已从内置定义同步 (supports_tools={})",
                                profile.id, builtin.supports_tools
                            );
                        }
                    }
                }
                if patched {
                    if let Err(e) = self.save_model_profiles(&profiles).await {
                        log::warn!("[VendorModel] 迁移保存失败（不影响本次读取）: {}", e);
                    }
                }
            }
            let _ = self
                .db
                .save_setting(BUILTIN_MODEL_PROFILES_SNAPSHOT_KEY, "[]");
            let _ = self.db.save_setting(BUILTIN_CAPS_MIGRATION_KEY, "done");
            log::info!("[VendorModel] 能力字段迁移完成");
        }

        let snapshot_map = self.read_builtin_profile_snapshot_map();
        if let Ok((_, builtin_profiles)) = self.load_builtin_vendor_profiles() {
            if self
                .reconcile_hidden_builtin_model_profile_ids(
                    &builtin_profiles,
                    &profiles,
                    &mut hidden_builtin_ids,
                    &snapshot_map,
                )
                .is_err()
            {
                log::warn!("[VendorModel] 修正隐藏内置模型列表失败，继续使用现有值");
            }

            if !hidden_builtin_ids.is_empty() {
                profiles.retain(|profile| !hidden_builtin_ids.contains(&profile.id));
            }

            for builtin_profile in &builtin_profiles {
                if hidden_builtin_ids.contains(&builtin_profile.id) {
                    continue;
                }
                Self::merge_builtin_profile_user_aware(
                    &mut profiles,
                    builtin_profile.clone(),
                    snapshot_map.get(&builtin_profile.id),
                );
            }
            if let Err(err) = self.save_builtin_profile_snapshot(&builtin_profiles) {
                log::warn!("[VendorModel] 保存内置模型快照失败（不影响读取）: {}", err);
            }
        }
        Ok(profiles)
    }

    pub(crate) async fn read_user_model_profiles(&self) -> Result<Vec<ModelProfile>> {
        let raw = self
            .db
            .get_setting("model_profiles")
            .map_err(|e| AppError::database(format!("获取模型条目失败: {}", e)))?
            .unwrap_or_else(|| "[]".to_string());

        let profiles: Vec<ModelProfile> = serde_json::from_str(&raw)
            .map_err(|e| AppError::configuration(format!("解析模型条目失败: {}", e)))?;
        Ok(profiles)
    }

    async fn model_profiles_for_runtime(&self) -> Result<Vec<ModelProfile>> {
        self.get_model_profiles().await
    }

    pub async fn save_model_profiles(&self, profiles: &[ModelProfile]) -> Result<()> {
        if let Ok((_, builtin_profiles)) = self.load_builtin_vendor_profiles() {
            let builtin_id_set: HashSet<String> = builtin_profiles
                .iter()
                .map(|profile| profile.id.clone())
                .collect();
            let builtin_model_map: HashMap<&str, &str> = builtin_profiles
                .iter()
                .map(|profile| (profile.id.as_str(), profile.model.as_str()))
                .collect();
            let incoming_builtin_ids: HashSet<String> = profiles
                .iter()
                .map(|profile| profile.id.clone())
                .filter(|id| builtin_id_set.contains(id))
                .collect();

            let existing_profiles = self.read_user_model_profiles().await.unwrap_or_default();
            let snapshot_map = self.read_builtin_profile_snapshot_map();
            let previously_known_builtin_ids: HashSet<String> = builtin_id_set
                .iter()
                .filter(|id| {
                    let Some(current_model) = builtin_model_map.get(id.as_str()) else {
                        return false;
                    };

                    let snapshot_matches = snapshot_map
                        .get(*id)
                        .map(|snapshot| snapshot.model.as_str() == *current_model)
                        .unwrap_or(false);
                    let existing_matches = existing_profiles.iter().any(|profile| {
                        profile.id == **id && profile.model.as_str() == *current_model
                    });

                    snapshot_matches || existing_matches
                })
                .cloned()
                .collect();

            if !previously_known_builtin_ids.is_empty() || !incoming_builtin_ids.is_empty() {
                let hidden_builtin_ids: HashSet<String> = previously_known_builtin_ids
                    .difference(&incoming_builtin_ids)
                    .cloned()
                    .collect();
                self.save_hidden_builtin_model_profile_ids(&hidden_builtin_ids)?;
            }
        }

        let vendor_map: HashMap<String, VendorConfig> = self
            .read_user_vendor_configs()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|vendor| (vendor.id.clone(), vendor))
            .collect();
        let mut vendor_map = vendor_map;
        if let Ok((builtin_vendors, _)) = self.load_builtin_vendor_profiles() {
            for vendor in builtin_vendors {
                vendor_map.entry(vendor.id.clone()).or_insert(vendor);
            }
        }

        let mut sanitized = profiles.to_vec();
        for profile in &mut sanitized {
            let vendor = vendor_map.get(&profile.vendor_id);
            normalize_model_profile_protocol_config(profile, vendor);
        }

        let json = serde_json::to_string(&sanitized)
            .map_err(|e| AppError::configuration(format!("序列化模型条目失败: {}", e)))?;
        self.db
            .save_setting("model_profiles", &json)
            .map_err(|e| AppError::database(format!("保存模型条目失败: {}", e)))?;
        self.invalidate_api_configs_cache().await; // A6#1: 配置写入失效缓存
        Ok(())
    }

    // ==================== Config resolution ====================

    pub async fn get_api_configs(&self) -> Result<Vec<ApiConfig>> {
        // ★ perf-audit A6#1: 读缓存(60s TTL, 写路径主动失效)——20 个调用点原本每请求
        // 重复 bootstrap/repair/runtime 三轮 settings 读与全量 JSON 解析
        const API_CONFIGS_TTL_SECS: u64 = 60;
        if let Some((at, cached)) = self.api_configs_cache.lock().await.as_ref() {
            if at.elapsed().as_secs() < API_CONFIGS_TTL_SECS {
                return Ok(cached.as_ref().clone());
            }
        }

        self.bootstrap_vendor_model_config().await?;
        let vendors = self.vendor_configs_for_runtime().await?;
        let profiles = self.model_profiles_for_runtime().await?;
        let vendor_map: HashMap<String, VendorConfig> =
            vendors.into_iter().map(|v| (v.id.clone(), v)).collect();

        let mut resolved = Vec::new();
        for profile in profiles {
            if let Some(vendor) = vendor_map.get(&profile.vendor_id) {
                let merged = self.merge_vendor_profile(vendor, &profile)?;
                resolved.push(merged.runtime);
            } else {
                log::warn!("[VendorModel] 找不到模型条目关联的供应商: {}", profile.id);
            }
        }

        *self.api_configs_cache.lock().await =
            Some((std::time::Instant::now(), std::sync::Arc::new(resolved.clone())));
        Ok(resolved)
    }

    /// 配置写入后失效 get_api_configs 缓存(A6#1)
    pub(crate) async fn invalidate_api_configs_cache(&self) {
        *self.api_configs_cache.lock().await = None;
    }

    // ==================== Model selection/assignment ====================

    pub async fn get_model2_config(&self) -> Result<ApiConfig> {
        let assignments = self.get_model_assignments().await?;
        let model2_id = assignments
            .model2_config_id
            .ok_or_else(|| AppError::configuration("对话模型未配置"))?;

        let configs = self.get_api_configs().await?;
        let config = configs
            .into_iter()
            .find(|c| c.id == model2_id && !c.is_embedding && !c.is_reranker)
            .ok_or_else(|| {
                AppError::configuration(
                    "找不到有效的对话模型配置（禁止使用嵌入/重排序模型作为对话模型）",
                )
            })?;

        Ok(config)
    }

    pub async fn get_memory_decision_model_config(&self) -> Result<ApiConfig> {
        let assignments = self.get_model_assignments().await?;
        let model_id = assignments
            .memory_decision_model_config_id
            .or(assignments.model2_config_id)
            .ok_or_else(|| AppError::configuration("没有配置可用的记忆决策模型"))?;

        let configs = self.get_api_configs().await?;
        let config = configs
            .into_iter()
            .find(|c| c.id == model_id && !c.is_embedding && !c.is_reranker)
            .ok_or_else(|| {
                AppError::configuration("找不到有效的记忆决策模型配置（禁止使用嵌入/重排序模型）")
            })?;

        Ok(config)
    }

    pub async fn get_chat_title_model_config(&self) -> Result<ApiConfig> {
        let assignments = self.get_model_assignments().await?;
        let model_id = assignments
            .chat_title_model_config_id
            .or(assignments.model2_config_id)
            .ok_or_else(|| AppError::configuration("没有配置可用的标题/标签生成模型"))?;

        let configs = self.get_api_configs().await?;
        let config = configs
            .into_iter()
            .find(|c| c.id == model_id && !c.is_embedding && !c.is_reranker)
            .ok_or_else(|| {
                AppError::configuration(
                    "找不到有效的标题/标签生成模型配置（禁止使用嵌入/重排序模型）",
                )
            })?;

        Ok(config)
    }

    pub(crate) async fn get_anki_model_config(&self) -> Result<ApiConfig> {
        let assignments = self.get_model_assignments().await?;
        let anki_model_id = assignments
            .anki_card_model_config_id
            .ok_or_else(|| AppError::configuration("Anki制卡模型未配置"))?;

        let configs = self.get_api_configs().await?;
        let config_count = configs.len();
        let found_disabled = configs
            .iter()
            .any(|c| c.id == anki_model_id && !c.enabled);
        let config = configs
            .into_iter()
            .find(|c| c.id == anki_model_id && c.enabled)
            .ok_or_else(|| {
                let hint = if found_disabled {
                    format!("Anki制卡模型配置(ID: {})已存在但被禁用，请在设置中启用", anki_model_id)
                } else {
                    format!("找不到有效的Anki制卡模型配置. Tried to find ID: {} in {} available configs.", anki_model_id, config_count)
                };
                AppError::configuration(hint)
            })?;

        log::debug!(
            "找到 Anki 制卡模型配置: 模型={}, API地址={}",
            config.model, config.base_url
        );
        Ok(config)
    }

    pub async fn select_model_for(
        &self,
        task: &str,
        override_id: Option<String>,
        temperature: Option<f32>,
        top_p: Option<f32>,
        frequency_penalty: Option<f32>,
        presence_penalty: Option<f32>,
        max_output_tokens: Option<u32>,
    ) -> Result<(ApiConfig, bool)> {
        if let Some(ref override_id) = override_id {
            let configs = self.get_api_configs().await?;
            let config_count = configs.len();
            let found_disabled = configs
                .iter()
                .any(|c| c.id == *override_id && !c.enabled);
            let mut config = configs
                .into_iter()
                .find(|c| c.id == *override_id && c.enabled)
                .ok_or_else(|| {
                    let hint = if found_disabled {
                        format!("模型配置(ID: {})已存在但被禁用，请在设置中启用", override_id)
                    } else {
                        format!("找不到可用的模型配置. Tried to find ID: {} in {} available configs.", override_id, config_count)
                    };
                    AppError::configuration(hint)
                })?;

            if let Some(temp) = temperature {
                config.temperature = temp;
            }
            if let Some(max_tokens) = max_output_tokens {
                config.max_output_tokens = max_tokens;
            }
            config.top_p_override = top_p;
            config.frequency_penalty_override = frequency_penalty;
            config.presence_penalty_override = presence_penalty;

            let enable_cot = config.is_reasoning;
            return Ok((config, enable_cot));
        }

        let assignments = self.get_model_assignments().await?;
        let configs = self.get_api_configs().await?;

        let (model_id, enable_cot) = match task {
            "default" => {
                let model_id = assignments
                    .model2_config_id
                    .ok_or_else(|| AppError::configuration("对话模型未配置"))?;
                (model_id, true)
            }
            "chat_title" | "tag_generation" => {
                let model_id = assignments
                    .chat_title_model_config_id
                    .or(assignments.model2_config_id)
                    .ok_or_else(|| AppError::configuration("没有配置可用的标题/标签生成模型"))?;
                (model_id, false)
            }
            "review" => {
                let model_id = assignments
                    .review_analysis_model_config_id
                    .ok_or_else(|| AppError::configuration("未配置回顾分析模型"))?;
                (model_id, true)
            }
            _ => {
                return Err(AppError::configuration(format!(
                    "不支持的任务类型: {}",
                    task
                )))
            }
        };

        let config_count = configs.len();
        let found_disabled = configs
            .iter()
            .any(|c| c.id == model_id && !c.enabled);
        let mut config = configs
            .into_iter()
            .find(|c| c.id == model_id && c.enabled)
            .ok_or_else(|| {
                let hint = if found_disabled {
                    format!("模型配置(ID: {})已存在但被禁用，请在设置中启用", model_id)
                } else {
                    format!("找不到可用的模型配置. Tried to find ID: {} in {} available configs.", model_id, config_count)
                };
                AppError::configuration(hint)
            })?;

        if let Some(temp) = temperature {
            config.temperature = temp;
        }
        if let Some(max_tokens) = max_output_tokens {
            config.max_output_tokens = max_tokens;
        }
        config.top_p_override = top_p;
        config.frequency_penalty_override = frequency_penalty;
        config.presence_penalty_override = presence_penalty;

        let final_enable_cot = config.is_reasoning && enable_cot;

        Ok((config, final_enable_cot))
    }

    pub async fn get_model_assignments(&self) -> Result<ModelAssignments> {
        let assignments_str = self.db.get_setting("model_assignments")
            .map_err(|e| AppError::database(format!("获取模型分配配置失败: {}", e)))?
            .unwrap_or_else(|| r#"{"model2_config_id": null, "review_analysis_model_config_id": null, "anki_card_model_config_id": null, "qbank_ai_grading_model_config_id": null}"#.to_string());

        let assignments: ModelAssignments = serde_json::from_str(&assignments_str)
            .map_err(|e| AppError::configuration(format!("解析模型分配配置失败: {}", e)))?;

        Ok(assignments)
    }

    pub async fn save_model_assignments(&self, assignments: &ModelAssignments) -> Result<()> {
        let assignments_str = serde_json::to_string(assignments)
            .map_err(|e| AppError::configuration(format!("序列化模型分配配置失败: {}", e)))?;

        self.db
            .save_setting("model_assignments", &assignments_str)
            .map_err(|e| AppError::database(format!("保存模型分配配置失败: {}", e)))?;
        self.invalidate_api_configs_cache().await; // A6#1: 配置写入失效缓存

        Ok(())
    }

    // ==================== OCR Config ====================

    pub async fn get_ocr_model_config(&self) -> Result<ApiConfig> {
        use crate::ocr_adapters::OcrEngineType;

        let configs = self.get_api_configs().await?;
        let available = self.get_available_ocr_models().await;

        let mut enabled_models: Vec<&OcrModelConfig> =
            available.iter().filter(|m| m.enabled).collect();
        enabled_models.sort_by_key(|m| {
            let engine = OcrEngineType::from_str(&m.engine_type);
            (if engine.is_dedicated_ocr() { 0u8 } else { 1 }, m.priority)
        });

        for ocr_config in &enabled_models {
            if let Some(config) = configs.iter().find(|c| c.id == ocr_config.config_id) {
                let engine = OcrEngineType::from_str(&ocr_config.engine_type);
                // PaddleOCR REST API 引擎使用 job-based 流程，不标记 multimodal，
                // 但仍然是合法的 OCR 引擎，需要加入可用列表。
                let is_ocr_usable = config.is_multimodal || engine == OcrEngineType::PaddleOcrApi;
                if is_ocr_usable {
                    log::debug!(
                        "[OCR] 使用引擎 {} 对应的模型配置: id={}, model={} (priority={})",
                        ocr_config.engine_type, config.id, config.model, ocr_config.priority
                    );
                    return Ok(config.clone());
                } else {
                    log::warn!(
                        "[OCR] 引擎 {} 对应的模型 {} 不支持多模态，跳过",
                        ocr_config.engine_type, config.model
                    );
                }
            } else {
                log::warn!(
                    "[OCR] 引擎 {} 对应的配置 ID {} 不存在，跳过",
                    ocr_config.engine_type, ocr_config.config_id
                );
            }
        }

        let assignments = self.get_model_assignments().await?;
        let model_id = assignments.exam_sheet_ocr_model_config_id.ok_or_else(|| {
            AppError::configuration("OCR 模型未配置，请在模型分配中添加 OCR 引擎")
        })?;

        let config = configs
            .into_iter()
            .find(|c| c.id == model_id)
            .ok_or_else(|| {
                AppError::configuration(format!("找不到 ID 为 {} 的模型配置", model_id))
            })?;

        // PaddleOCR REST API 使用 job-based 流程，不标记 multimodal，也是合法的 OCR 模型
        if !config.is_multimodal && !config.model.contains("PaddleOCR") && !config.model.contains("PP-OCR") && !config.model.contains("PP-Structure") {
            return Err(AppError::configuration(
                "当前配置的 OCR 模型未启用多模态能力，请选择支持图像输入的模型（如 DeepSeek-OCR）",
            ));
        }

        log::debug!(
            "[OCR] 使用配置的模型（回退）: id={}, model={}",
            config.id, config.model
        );

        Ok(config)
    }

    pub async fn get_ocr_configs_by_priority(
        &self,
        task_type: crate::ocr_adapters::OcrTaskType,
    ) -> Result<Vec<(ApiConfig, crate::ocr_adapters::OcrEngineType)>> {
        use crate::ocr_adapters::{OcrAdapterFactory, OcrEngineType, OcrTaskType};

        let configs = self.get_api_configs().await?;
        let available = self.get_available_ocr_models().await;

        let mut enabled_models: Vec<&OcrModelConfig> =
            available.iter().filter(|m| m.enabled).collect();
        enabled_models.sort_by_key(|m| m.priority);

        let mut result = Vec::new();
        for ocr_config in &enabled_models {
            if let Some(config) = configs.iter().find(|c| c.id == ocr_config.config_id) {
                let engine = OcrEngineType::from_str(&ocr_config.engine_type);
                // PaddleOCR REST API 引擎使用非 VLM 的 job-based 流程，不标记 multimodal，
                // 但仍然是合法的 OCR 引擎，需要加入优先级列表。
                if !config.is_multimodal && engine != OcrEngineType::PaddleOcrApi {
                    continue;
                }
                let effective_engine =
                    if OcrAdapterFactory::validate_model_for_engine(&config.model, engine) {
                        engine
                    } else {
                        OcrAdapterFactory::infer_engine_from_model(&config.model)
                    };
                result.push((config.clone(), effective_engine));
            }
        }

        if result.is_empty() {
            if let Ok(config) = self.get_ocr_model_config().await {
                let engine = OcrAdapterFactory::infer_engine_from_model(&config.model);
                result.push((config, engine));
            }
        }

        if result.is_empty() {
            return Err(AppError::configuration(
                "没有可用的 OCR 引擎配置，请在设置中添加 OCR 引擎",
            ));
        }

        match task_type {
            OcrTaskType::FreeText => {
                result.sort_by_key(|(_, engine)| if engine.is_dedicated_ocr() { 0 } else { 1 });
            }
            OcrTaskType::Structured => {
                result.sort_by_key(|(_, engine)| if engine.is_dedicated_ocr() { 1 } else { 0 });
            }
        }

        log::debug!(
            "[OCR] 引擎优先级（{:?}）: {}",
            task_type,
            result
                .iter()
                .enumerate()
                .map(|(i, (c, e))| format!("#{} {}({})", i, e.display_name(), c.model))
                .collect::<Vec<_>>()
                .join(" → ")
        );

        Ok(result)
    }

    pub async fn get_available_ocr_models(&self) -> Vec<OcrModelConfig> {
        if let Ok(Some(json)) = self.db.get_setting("ocr.available_models") {
            if let Ok(mut models) = serde_json::from_str::<Vec<OcrModelConfig>>(&json) {
                let mut needs_save = crate::cmd::ocr::migrate_paddle_ocr_models(&mut models);

                let glm_migrate_ids: Vec<String> = models
                    .iter()
                    .filter(|m| {
                        m.engine_type == "glm4v_ocr" && m.model.to_lowercase().contains("glm-4.1v")
                    })
                    .map(|m| m.config_id.clone())
                    .collect();

                if crate::cmd::ocr::migrate_glm_ocr_models(&mut models) {
                    needs_save = true;
                    if !glm_migrate_ids.is_empty() {
                        if let Ok(mut api_configs) = self.get_api_configs().await {
                            let mut api_changed = false;
                            for cfg in api_configs.iter_mut() {
                                if glm_migrate_ids.contains(&cfg.id)
                                    && cfg.model.to_lowercase().contains("glm-4.1v")
                                {
                                    log::info!(
                                        "[OCR] 同步更新 ApiConfig model: {} → zai-org/GLM-4.6V (id={})",
                                        cfg.model, cfg.id
                                    );
                                    cfg.model = "zai-org/GLM-4.6V".to_string();
                                    cfg.name = cfg
                                        .name
                                        .replace("GLM-4.1V", "GLM-4.6V")
                                        .replace("4.1V", "4.6V");
                                    api_changed = true;
                                }
                            }
                            if api_changed {
                                let _ = self.save_api_configurations(&api_configs).await;
                            }
                        }
                    }
                }

                if models.len() > 1 && models.iter().all(|m| m.priority == 0) {
                    if let Ok(Some(old_engine)) = self.db.get_setting("ocr.engine_type") {
                        for (i, model) in models.iter_mut().enumerate() {
                            if model.engine_type == old_engine {
                                model.priority = 0;
                            } else {
                                model.priority = (i as u32) + 1;
                            }
                        }
                        models.sort_by_key(|m| m.priority);
                        for (i, model) in models.iter_mut().enumerate() {
                            model.priority = i as u32;
                        }
                        needs_save = true;
                        log::info!(
                            "[OCR] 已从旧 ocr.engine_type='{}' 迁移到优先级列表",
                            old_engine
                        );
                    }
                }

                if needs_save {
                    if let Ok(updated_json) = serde_json::to_string(&models) {
                        let _ = self.db.save_setting("ocr.available_models", &updated_json);
                    }
                }
                // ★ Fix 3: 诊断日志 — 列出可用 OCR 引擎
                let enabled_summary: Vec<String> = models
                    .iter()
                    .filter(|m| m.enabled)
                    .map(|m| format!("{}::{} (pri={})", m.engine_type, m.name, m.priority))
                    .collect();
                log::info!(
                    "[OCR_ENGINES] Available OCR models: enabled=[{}], total={}",
                    enabled_summary.join(", "),
                    models.len()
                );
                return models;
            }
        }
        Vec::new()
    }

    pub fn is_ocr_thinking_enabled(&self) -> bool {
        self.db
            .get_setting("ocr.enable_thinking")
            .ok()
            .flatten()
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false)
    }

    pub async fn get_ocr_engine_type(&self) -> crate::ocr_adapters::OcrEngineType {
        use crate::ocr_adapters::OcrEngineType;

        let available = self.get_available_ocr_models().await;
        let mut enabled: Vec<&OcrModelConfig> = available.iter().filter(|m| m.enabled).collect();
        enabled.sort_by_key(|m| {
            let engine = OcrEngineType::from_str(&m.engine_type);
            (if engine.is_dedicated_ocr() { 0u8 } else { 1 }, m.priority)
        });

        if let Some(first) = enabled.first() {
            return OcrEngineType::from_str(&first.engine_type);
        }

        let engine_str = self
            .db
            .get_setting("ocr.engine_type")
            .ok()
            .flatten()
            .unwrap_or_else(|| "paddle_ocr_vl".to_string());

        OcrEngineType::from_str(&engine_str)
    }

    pub async fn get_ocr_adapter(&self) -> std::sync::Arc<dyn crate::ocr_adapters::OcrAdapter> {
        use crate::ocr_adapters::OcrAdapterFactory;

        let engine_type = self.get_ocr_engine_type().await;
        OcrAdapterFactory::create(engine_type)
    }

    pub async fn get_ocr_config_with_effective_engine(
        &self,
    ) -> Result<(ApiConfig, crate::ocr_adapters::OcrEngineType)> {
        use crate::ocr_adapters::{OcrAdapterFactory, OcrEngineType};

        let config = self.get_ocr_model_config().await?;

        let available = self.get_available_ocr_models().await;
        let effective_engine =
            if let Some(ocr_model) = available.iter().find(|m| m.config_id == config.id) {
                let declared = OcrEngineType::from_str(&ocr_model.engine_type);
                if OcrAdapterFactory::validate_model_for_engine(&config.model, declared) {
                    declared
                } else {
                    OcrAdapterFactory::infer_engine_from_model(&config.model)
                }
            } else {
                OcrAdapterFactory::infer_engine_from_model(&config.model)
            };

        log::debug!(
            "[OCR] effective engine={}, model={}",
            effective_engine.as_str(),
            config.model
        );

        Ok((config, effective_engine))
    }

    async fn get_ocr_model_config_for_engine(
        &self,
        engine_type: crate::ocr_adapters::OcrEngineType,
    ) -> Result<ApiConfig> {
        let configs = self.get_api_configs().await?;

        if let Ok(Some(available_models_json)) = self.db.get_setting("ocr.available_models") {
            if let Ok(available_models) =
                serde_json::from_str::<Vec<OcrModelConfig>>(&available_models_json)
            {
                if let Some(ocr_config) = available_models
                    .iter()
                    .find(|m| m.engine_type == engine_type.as_str())
                {
                    if let Some(config) = configs.iter().find(|c| c.id == ocr_config.config_id) {
                        return Ok(config.clone());
                    }
                }
            }
        }

        let recommended_model = engine_type.recommended_model();
        if let Some(config) = configs
            .iter()
            .find(|c| c.model.contains(recommended_model) || recommended_model.contains(&c.model))
        {
            return Ok(config.clone());
        }

        self.get_ocr_model_config().await
    }

    /// 使用指定引擎测试 OCR
    ///
    /// 用于对比不同 OCR 引擎的速度和质量
    pub async fn test_ocr_with_engine(
        &self,
        image_path: String,
        engine_type: crate::ocr_adapters::OcrEngineType,
        config_id: Option<&str>,
    ) -> Result<(String, Vec<crate::ocr_adapters::OcrRegion>)> {
        use crate::ocr_adapters::{OcrAdapterFactory, OcrMode, OcrRegion};
        use crate::providers::ProviderAdapter;
        use serde_json::json;

        // ── 特殊路径: PaddleOCR REST API (job-based, 非 VLM prompt/response) ──
        // 仅 PaddleOcrApi 使用 AI Studio job-based API (POST /api/v2/ocr/jobs)。
        // PaddleOcrVl / PaddleOcrVlV1 是 VLM 引擎（如 SiliconFlow 托管的 PaddleOCR-VL），
        // 使用标准 /v1/chat/completions 格式，应走 VLM 路径。
        if engine_type == crate::ocr_adapters::OcrEngineType::PaddleOcrApi {
            return self
                .test_paddle_ocr_api_engine(&image_path, config_id)
                .await;
        }

        // 获取配置，用于 base_url 检查和后续 VLM 路径
        let config = if let Some(cid) = config_id {
            let configs = self.get_api_configs().await?;
            configs
                .into_iter()
                .find(|c| c.id == cid)
                .ok_or_else(|| AppError::configuration(format!("找不到配置 ID: {}", cid)))?
        } else {
            self.get_ocr_model_config_for_engine(engine_type).await?
        };

        // ── 补充检查: base_url 指向 PaddleOCR API 但 engine_type 未命中 → 路由到 job-based 路径 ──
        // 当用户配置的引擎类型未标记为 PaddleOCR（如泛型 vl_model）但 base_url
        // 指向 https://paddleocr.aistudio-app.com 时，此检查可防止请求被发送到不存在的
        // /v1/chat/completions 端点（导致 404）。
        if config.base_url.contains("paddleocr.aistudio-app.com") {
            log::info!(
                "[OCR Test] base_url 指向 PaddleOCR API (engine={:?})，路由到 job-based 路径",
                engine_type
            );
            return self
                .test_paddle_ocr_api_engine(&image_path, config_id)
                .await;
        }

        let adapter = OcrAdapterFactory::create(engine_type);
        let engine_name = adapter.display_name();
        let ocr_mode = OcrMode::Grounding;

        log::debug!(
            "[OCR Test] 使用引擎 {} 测试，模型: {}",
            engine_name, config.model
        );

        let mime = super::LLMManager::infer_image_mime(&image_path);
        let (data_url, _) = self
            .prepare_segmentation_image_data(&image_path, mime)
            .await?;

        let prompt_text = adapter.build_prompt(ocr_mode);
        let messages = vec![json!({
            "role": "user",
            "content": [
                { "type": "image_url", "image_url": { "url": data_url, "detail": if adapter.requires_high_detail() { "high" } else { "low" } } },
                { "type": "text", "text": prompt_text }
            ]
        })];

        let max_tokens = effective_max_tokens(config.max_output_tokens, config.max_tokens_limit)
            .min(adapter.recommended_max_tokens(ocr_mode))
            .max(2048)
            .min(8000);

        let mut request_body = json!({
            "model": config.model,
            "messages": messages,
            "temperature": adapter.recommended_temperature(),
            "max_tokens": max_tokens,
            "stream": false,
        });

        if let Some(extra) = adapter.get_extra_request_params() {
            if let Some(obj) = request_body.as_object_mut() {
                if let Some(extra_obj) = extra.as_object() {
                    for (k, v) in extra_obj {
                        obj.insert(k.to_string(), v.clone());
                    }
                } else {
                    obj.insert("extra_params".to_string(), extra);
                }
            }
        }

        if let Some(repetition_penalty) = adapter.recommended_repetition_penalty() {
            if let Some(obj) = request_body.as_object_mut() {
                obj.insert("repetition_penalty".to_string(), json!(repetition_penalty));
            }
            log::debug!(
                "[OCR Test] 设置 repetition_penalty = {} (避免重复输出)",
                repetition_penalty
            );
        }

        let provider_adapter: Box<dyn ProviderAdapter> = match config.model_adapter.as_str() {
            "google" | "gemini" => Box::new(crate::providers::GeminiAdapter::new()),
            "anthropic" | "claude" => Box::new(crate::providers::AnthropicAdapter::new()),
            _ => Box::new(crate::providers::OpenAIAdapter),
        };

        let preq = provider_adapter
            .build_request(
                &config.base_url,
                &config.api_key,
                &config.model,
                &request_body,
            )
            .map_err(|e| AppError::llm(format!("{} 请求构建失败: {}", engine_name, e)))?;

        super::model2_pipeline::log_llm_request_audit(
            "OCR_ENGINE_TEST",
            &preq.url,
            &config.model,
            &request_body,
            None,
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| AppError::network(format!("创建 HTTP 客户端失败: {}", e)))?;

        let mut header_map = reqwest::header::HeaderMap::new();
        for (k, v) in preq.headers.iter() {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let response = client
            .post(&preq.url)
            .headers(header_map)
            .json(&preq.body)
            .send()
            .await
            .map_err(|e| AppError::network(format!("{} 请求失败: {}", engine_name, e)))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(AppError::llm(format!(
                "{} API 返回错误 ({}): {}",
                engine_name, status, error_text
            )));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AppError::llm(format!("解析 {} 响应失败: {}", engine_name, e)))?;

        let content = response_json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let (image_width, image_height) =
            image::io::Reader::open(&image_path)
                .and_then(|r| r.into_dimensions().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)))
                .unwrap_or((1000, 1000));

        let parse_result = adapter.parse_response(
            &content,
            image_width,
            image_height,
            0,
            &image_path,
            OcrMode::Grounding,
        );

        let regions = match parse_result {
            Ok(page_result) => page_result.regions,
            Err(_) => {
                vec![OcrRegion {
                    label: "text".to_string(),
                    text: content.clone(),
                    bbox_normalized: None,
                    bbox_pixels: None,
                    confidence: None,
                    raw_output: Some(content.clone()),
                }]
            }
        };

        let final_regions = if regions.is_empty() {
            vec![OcrRegion {
                label: "text".to_string(),
                text: content.clone(),
                bbox_normalized: None,
                bbox_pixels: None,
                confidence: None,
                raw_output: Some(content.clone()),
            }]
        } else {
            regions
        };

        let full_text = final_regions
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok((full_text, final_regions))
    }

    /// PaddleOCR REST API 引擎测试路径
    ///
    /// 使用 `PaddleOcrApiClient` 直接调用 AI Studio REST API，
    /// 不同于 VLM 引擎的 prompt/response 流程。
    async fn test_paddle_ocr_api_engine(
        &self,
        image_path: &str,
        config_id: Option<&str>,
    ) -> Result<(String, Vec<crate::ocr_adapters::OcrRegion>)> {
        use crate::ocr_adapters::OcrRegion;
        use crate::paddleocr_api::PaddleOcrApiClient;

        let config = if let Some(cid) = config_id {
            let configs = self.get_api_configs().await?;
            configs
                .into_iter()
                .find(|c| c.id == cid)
                .ok_or_else(|| AppError::configuration(format!("找不到配置 ID: {}", cid)))?
        } else {
            self.get_ocr_model_config_for_engine(
                crate::ocr_adapters::OcrEngineType::PaddleOcrApi,
            )
            .await?
        };

        let mut api_key = self.decrypt_api_key_if_needed(&config.api_key).map_err(|e| {
            AppError::configuration(format!("PaddleOCR API key 解密失败: {}", e))
        })?;
        // ★ 兜底：如果 config 中的 api_key 为空，尝试从专用 ocr.paddleocr.token 设置获取
        if api_key.is_empty() {
            if let Ok(Some(token)) = self.db.get_setting("ocr.paddleocr.token") {
                if !token.is_empty() {
                    log::info!("[OCR Test::PaddleApi] Using token from ocr.paddleocr.token setting");
                    api_key = token;
                }
            }
        }

        let model = &config.model;

        log::info!(
            "[OCR Test::PaddleApi] 提交 OCR job: model={}, file={}",
            model,
            image_path
        );

        let client = PaddleOcrApiClient::new(api_key);
        let result = client
            .ocr_file(image_path, model)
            .await
            .map_err(|e| AppError::llm(format!("PaddleOCR API 调用失败: {}", e)))?;

        log::info!(
            "[OCR Test::PaddleApi] OCR job 完成: {} 页",
            result.total_pages
        );

        // 转换为统一的 (text, regions) 返回格式
        let mut all_text = String::new();
        let mut all_regions = Vec::new();

        for page in &result.pages {
            let text = page.markdown_text.trim();
            if !text.is_empty() {
                if !all_text.is_empty() {
                    all_text.push_str("\n\n---\n\n");
                }
                all_text.push_str(text);
            }

            for img in &page.images {
                all_regions.push(OcrRegion {
                    label: format!("image_{}_p{}", img.name, page.page_index),
                    text: String::new(),
                    bbox_normalized: None,
                    bbox_pixels: None,
                    confidence: None,
                    raw_output: Some(img.url.clone()),
                });
            }
        }

        if all_regions.is_empty() && !all_text.is_empty() {
            all_regions.push(OcrRegion {
                label: "document".to_string(),
                text: all_text.clone(),
                bbox_normalized: None,
                bbox_pixels: None,
                confidence: None,
                raw_output: None,
            });
        }

        Ok((all_text, all_regions))
    }
}
