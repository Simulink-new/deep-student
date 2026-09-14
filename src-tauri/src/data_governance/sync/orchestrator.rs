use super::envelope::{validate_change_payload, SyncEnvelope};
use super::manifest::*;
use super::retry::retry_async;
use super::tombstone;
use crate::backup_common;
use crate::cloud_storage::CloudStorage;
use crate::crypto::backup_crypto;
use std::collections::{HashMap, HashSet};

/// ★ perf-audit A5#4: 资产哈希进程内备忘 key -> (sha256, size, mtime)
/// 同会话内重复同步零变更时跳过全量 sha256(2GB 资产目录旧实现每次同步全量哈希)
static ASSET_HASH_MEMO: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, (String, u64, std::time::SystemTime)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// 生成文件级同步的字节进度回调（经全局 sink 上报到前端，未挂 sink 时为 no-op）。
/// 用于 put_file/get_file 的 progress 参数，消除大文件传输期间进度条静止的问题。
fn file_progress_callback(
    label: &'static str,
    name: String,
) -> Option<Box<dyn Fn(u64, u64) + Send + Sync>> {
    Some(Box::new(move |done, total| {
        super::emitter::report_file_sync_progress(label, &name, done, total);
    }))
}

/// 同步管理器
pub struct SyncManager {
    /// 本地设备 ID
    device_id: String,
    /// 可选的端到端加密密码（对文本 payload 生效，批判报告 P0-2 修复）
    ///
    /// 覆盖范围：
    /// - ✅ 加密：`SyncManifest`、`SyncChangesPayload`、`*Tombstones`、
    ///   各种 metadata manifest（workspaces/blobs/assets）
    /// - ❌ **不**加密：VFS blob 的 raw bytes、workspace `.db` 文件。
    ///   原因：blob 走内容寻址（sha256 作 key），加密会破坏去重语义；
    ///   workspace DB 的完整性校验依赖明文 sha256。这两类的加密需要
    ///   额外的密文-明文 hash 双校验，作为后续 P1 任务单独处理。
    ///
    /// 语义：
    /// - `None` 或空字符串：所有 payload 明文上传（向后兼容旧数据）
    /// - `Some(pw)` 非空：文本 payload 使用 `DSBK` 容器加密（AES-256-GCM + Argon2id）
    ///
    /// 解密端自动探测：遇到 `DSBK` 魔数走解密，否则当明文处理。这让加密可以
    /// 平滑启用，不破坏已存在的明文云端数据。
    #[cfg(feature = "data_governance")]
    encryption_password: Option<String>,
}

impl SyncManager {
    /// 创建新的同步管理器（不启用 payload 加密）
    pub fn new(device_id: String) -> Self {
        Self {
            device_id,
            #[cfg(feature = "data_governance")]
            encryption_password: None,
        }
    }

    /// 创建带可选加密密码的同步管理器
    ///
    /// 空字符串 / `None` 等价于 `new()`（明文模式）。
    #[cfg(feature = "data_governance")]
    pub fn with_encryption(device_id: String, password: Option<String>) -> Self {
        let password = password.filter(|s| !s.is_empty());
        Self {
            device_id,
            encryption_password: password,
        }
    }

    /// 是否启用了 payload 加密
    #[cfg(feature = "data_governance")]
    pub fn encryption_enabled(&self) -> bool {
        self.encryption_password
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// 加密文本 payload 为上传格式（若未启用则原样返回）
    ///
    /// 输出：`DSBK` 容器（参见 `crypto::backup_crypto::encrypt_backup`）
    #[cfg(feature = "data_governance")]
    fn encode_payload(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError> {
        match self.encryption_password.as_deref() {
            Some(pw) if !pw.is_empty() => {
                backup_crypto::encrypt_backup(plaintext, pw)
                    .map_err(|e| SyncError::Database(format!("加密 sync payload 失败: {}", e)))
            }
            _ => Ok(plaintext.to_vec()),
        }
    }

    /// 解密下载的 payload（若魔数匹配则解密；否则原样返回，向后兼容老明文数据）
    ///
    /// 失败模式：
    /// - 数据带 `DSBK` 头但本端未配密码 → 返回错误（提示用户设置密码）
    /// - 数据带 `DSBK` 头但密码错误 → 返回错误
    /// - 数据未加密（无 `DSBK` 头） → 原样返回（兼容）
    #[cfg(feature = "data_governance")]
    fn decode_payload(&self, data: &[u8]) -> Result<Vec<u8>, SyncError> {
        if backup_crypto::is_encrypted_backup(data) {
            match self.encryption_password.as_deref() {
                Some(pw) if !pw.is_empty() => {
                    backup_crypto::decrypt_backup(data, pw).map_err(|e| {
                        SyncError::Database(format!(
                            "解密 sync payload 失败（密码错误或数据损坏）: {}",
                            e
                        ))
                    })
                }
                _ => Err(SyncError::Database(
                    "检测到加密的 sync payload 但本端未配置加密密码。\
                     请在云同步设置里填入正确的密码后重试。"
                        .to_string(),
                )),
            }
        } else {
            Ok(data.to_vec())
        }
    }

    /// 获取设备 ID
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 检测数据库级冲突
    ///
    /// 比较本地和云端的 SyncManifest，找出：
    /// 1. Schema 版本不匹配的数据库
    /// 2. 数据版本冲突（双方都有修改）
    /// 3. 仅存在于一方的数据库
    pub fn detect_conflicts(
        local_manifest: &SyncManifest,
        cloud_manifest: &SyncManifest,
    ) -> Result<ConflictDetectionResult, SyncError> {
        let mut result = ConflictDetectionResult::empty();

        // 收集所有数据库名称
        let mut all_databases: HashSet<&String> =
            local_manifest.databases.keys().collect();
        all_databases.extend(cloud_manifest.databases.keys());

        for db_name in all_databases {
            let local_state = local_manifest.databases.get(db_name);
            let cloud_state = cloud_manifest.databases.get(db_name);

            match (local_state, cloud_state) {
                // 双方都有该数据库
                (Some(local), Some(cloud)) => {
                    // 检查 Schema 版本
                    if local.schema_version != cloud.schema_version {
                        result.database_conflicts.push(DatabaseConflict {
                            database_name: db_name.clone(),
                            conflict_type: DatabaseConflictType::SchemaMismatch,
                            local_state: Some(local.clone()),
                            cloud_state: Some(cloud.clone()),
                        });
                        result.needs_migration = true;
                    }
                    // Schema 版本相同，检查数据版本
                    else if local.data_version != cloud.data_version {
                        // 双方数据版本不同，可能存在冲突
                        if local.checksum != cloud.checksum {
                            result.database_conflicts.push(DatabaseConflict {
                                database_name: db_name.clone(),
                                conflict_type: DatabaseConflictType::DataConflict,
                                local_state: Some(local.clone()),
                                cloud_state: Some(cloud.clone()),
                            });
                        }
                    }
                    // 数据版本相同但 checksum 不同（异常情况）
                    else if local.checksum != cloud.checksum {
                        result.database_conflicts.push(DatabaseConflict {
                            database_name: db_name.clone(),
                            conflict_type: DatabaseConflictType::ChecksumMismatch,
                            local_state: Some(local.clone()),
                            cloud_state: Some(cloud.clone()),
                        });
                    }
                }
                // 仅本地有
                (Some(local), None) => {
                    result.database_conflicts.push(DatabaseConflict {
                        database_name: db_name.clone(),
                        conflict_type: DatabaseConflictType::LocalOnly,
                        local_state: Some(local.clone()),
                        cloud_state: None,
                    });
                }
                // 仅云端有
                (None, Some(cloud)) => {
                    result.database_conflicts.push(DatabaseConflict {
                        database_name: db_name.clone(),
                        conflict_type: DatabaseConflictType::CloudOnly,
                        local_state: None,
                        cloud_state: Some(cloud.clone()),
                    });
                }
                // 双方都没有（不应该发生）
                (None, None) => {}
            }
        }

        result.has_conflicts =
            !result.database_conflicts.is_empty() || !result.record_conflicts.is_empty();

        Ok(result)
    }

    /// 检测记录级冲突
    ///
    /// 对于给定的数据库，比较本地和云端的记录差异。
    /// 这个方法需要实际的记录数据，通常在数据库级冲突检测后调用。
    pub fn detect_record_conflicts(
        database_name: &str,
        local_records: &[RecordSnapshot],
        cloud_records: &[RecordSnapshot],
    ) -> Vec<ConflictRecord> {
        let mut conflicts = Vec::new();

        // 构建云端记录索引（按 record_id）
        let cloud_index: HashMap<&str, &RecordSnapshot> = cloud_records
            .iter()
            .map(|r| (r.record_id.as_str(), r))
            .collect();

        // 遍历本地记录，查找冲突
        for local_record in local_records {
            if let Some(cloud_record) = cloud_index.get(local_record.record_id.as_str()) {
                // 双方都有该记录，检查是否冲突
                if Self::is_record_conflicting(local_record, cloud_record) {
                    conflicts.push(ConflictRecord {
                        database_name: database_name.to_string(),
                        table_name: local_record.table_name.clone(),
                        record_id: local_record.record_id.clone(),
                        local_version: local_record.local_version,
                        cloud_version: cloud_record.local_version,
                        local_updated_at: local_record.updated_at.clone(),
                        cloud_updated_at: cloud_record.updated_at.clone(),
                        local_data: local_record.data.clone(),
                        cloud_data: cloud_record.data.clone(),
                    });
                }
            }
        }

        conflicts
    }

    /// 判断两条记录是否冲突
    ///
    /// 冲突条件（LWW + 基线比对）：
    /// 1. 双方各自的 local_version > sync_version，表明都有未同步的修改
    /// 2. 数据内容不同
    ///
    /// 不再要求 sync_version 完全相等：当两台设备经过各自独立的同步周期后
    /// sync_version 自然会发散，原先的相等判断会导致静默数据覆盖。
    fn is_record_conflicting(local: &RecordSnapshot, cloud: &RecordSnapshot) -> bool {
        let local_modified = local.local_version > local.sync_version;
        let cloud_modified = cloud.local_version > cloud.sync_version;

        if local_modified && cloud_modified {
            return local.data != cloud.data;
        }
        false
    }

    /// 执行同步
    ///
    /// 根据合并策略处理冲突并返回同步结果。
    pub fn sync(
        &self,
        strategy: MergeStrategy,
        detection_result: &ConflictDetectionResult,
    ) -> Result<SyncResult, SyncError> {
        // 如果需要迁移，先处理 Schema 不匹配
        if detection_result.needs_migration {
            return Err(SyncError::SchemaMismatch {
                local: 0, // 具体版本在实际使用时填充
                cloud: 0,
            });
        }

        // 如果是手动模式且有冲突，返回需要手动处理
        if strategy == MergeStrategy::Manual && detection_result.has_conflicts {
            return Err(SyncError::ManualResolutionRequired {
                count: detection_result.total_conflicts(),
            });
        }

        let mut resolved_count = 0;
        let mut pending_manual = Vec::new();

        // 处理记录级冲突
        for conflict in &detection_result.record_conflicts {
            match strategy {
                MergeStrategy::KeepLocal => {
                    // 保留本地，标记云端需要更新
                    resolved_count += 1;
                }
                MergeStrategy::UseCloud => {
                    // 使用云端，本地需要更新
                    resolved_count += 1;
                }
                MergeStrategy::KeepLatest => {
                    // 比较时间戳，保留最新的
                    if conflict.local_updated_at >= conflict.cloud_updated_at {
                        // 本地更新，云端需要更新
                    } else {
                        // 云端更新，本地需要更新
                    }
                    resolved_count += 1;
                }
                MergeStrategy::Manual => {
                    // 需要用户手动处理
                    pending_manual.push(conflict.clone());
                }
            }
        }

        // 返回结果
        if pending_manual.is_empty() {
            Ok(SyncResult::success(
                detection_result.database_conflicts.len(),
                resolved_count,
            ))
        } else {
            Ok(SyncResult::needs_manual(pending_manual))
        }
    }

    /// 解决单个冲突
    ///
    /// 用户手动选择后调用此方法应用选择。
    pub fn resolve_conflict(
        &self,
        conflict: &ConflictRecord,
        resolution: ConflictResolution,
    ) -> Result<ResolvedRecord, SyncError> {
        let resolved_data = match resolution {
            ConflictResolution::KeepLocal => conflict.local_data.clone(),
            ConflictResolution::UseCloud => conflict.cloud_data.clone(),
            ConflictResolution::Merge(merged_data) => merged_data,
        };

        Ok(ResolvedRecord {
            database_name: conflict.database_name.clone(),
            table_name: conflict.table_name.clone(),
            record_id: conflict.record_id.clone(),
            resolved_data,
            new_version: conflict.local_version.max(conflict.cloud_version) + 1,
            resolved_at: chrono::Utc::now().to_rfc3339(),
            resolved_by: self.device_id.clone(),
        })
    }

    /// 创建同步清单
    pub fn create_manifest(&self, databases: HashMap<String, DatabaseSyncState>) -> SyncManifest {
        SyncManifest {
            sync_transaction_id: uuid::Uuid::new_v4().to_string(),
            databases,
            status: SyncTransactionStatus::Complete,
            created_at: chrono::Utc::now().to_rfc3339(),
            device_id: self.device_id.clone(),
        }
    }

    // ========================================================================
    // 云存储集成方法
    // ========================================================================

    /// 旧版单清单路径（用于向后兼容迁移读取）
    const LEGACY_MANIFEST_KEY: &'static str = "data_governance/sync_manifest.json";
    /// 按设备隔离的清单目录前缀
    const MANIFESTS_PREFIX: &'static str = "data_governance/manifests";
    /// 变更数据的云端路径前缀
    pub(crate) const CHANGES_PREFIX: &'static str = "data_governance/changes";

    /// 构建按设备隔离的清单路径
    fn device_manifest_key(device_id: &str) -> String {
        format!("{}/{}.json", Self::MANIFESTS_PREFIX, device_id)
    }

    /// 上传本地清单到云端（按设备隔离，自带网络重试）
    pub async fn upload_manifest(
        &self,
        storage: &dyn CloudStorage,
        manifest: &SyncManifest,
    ) -> Result<(), SyncError> {
        let json = serde_json::to_vec_pretty(manifest)
            .map_err(|e| SyncError::Database(format!("序列化清单失败: {}", e)))?;

        // [P0-2] 可选 payload 加密
        let payload = self.encode_payload(&json)?;

        let key = Self::device_manifest_key(&self.device_id);

        // [P3 Fix] 降低为 2 次，避免与传输层重试叠加
        retry_async("上传清单", 2, || {
            let payload = payload.clone();
            let key = key.clone();
            async move {
                storage
                    .put(&key, &payload)
                    .await
                    .map_err(|e| SyncError::Network(format!("上传清单失败: {}", e)))
            }
        })
        .await?;

        tracing::info!(
            "[sync] 清单已上传到云端: device={}, tx={}, databases={}, key={}, encrypted={}",
            manifest.device_id,
            manifest.sync_transaction_id,
            manifest.databases.len(),
            key,
            self.encryption_enabled()
        );

        Ok(())
    }

    /// 从云端下载清单（合并所有其他设备的清单）
    ///
    /// 策略：
    /// 1. 列出 `data_governance/manifests/` 下所有设备清单
    /// 2. 排除本设备，合并其他设备的数据库状态（取各库最高 data_version）
    /// 3. 向后兼容：若新目录为空，回退读取旧的单文件清单
    pub async fn download_manifest(
        &self,
        storage: &dyn CloudStorage,
    ) -> Result<SyncManifest, SyncError> {
        // 列出所有设备清单文件
        let files = storage
            .list(Self::MANIFESTS_PREFIX)
            .await
            .map_err(|e| SyncError::Network(format!("列出清单文件失败: {}", e)))?;

        let mut merged_databases: HashMap<String, DatabaseSyncState> = HashMap::new();
        let mut any_found = false;
        let mut latest_created_at: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut latest_created_at_raw = String::new();
        let mut merged_divergence: HashSet<String> =
            HashSet::new();

        for file in &files {
            let file_device_id = file
                .key
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".json"))
                .unwrap_or("");

            if file_device_id == self.device_id || file_device_id.is_empty() {
                continue;
            }

            let bytes = storage
                .get(&file.key)
                .await
                .map_err(|e| SyncError::Network(format!("下载设备清单失败 {}: {}", file.key, e)))?;
            if let Some(bytes) = bytes {
                // [P0-2] 透明解密：data_governance feature 下走 decode_payload；
                // 老明文数据 + 加密数据都由 decode_payload 自动识别 DSBK 魔数分流
                let decoded = match self.decode_payload(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "[sync] 跳过无法解密的设备清单: key={}, error={}",
                            file.key,
                            e
                        );
                        continue;
                    }
                };
                let manifest = match serde_json::from_slice::<SyncManifest>(&decoded) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("[sync] 跳过损坏设备清单: key={}, error={}", file.key, e);
                        continue;
                    }
                };
                any_found = true;
                if let Some(dt) = Self::parse_flexible_timestamp(&manifest.created_at) {
                    if latest_created_at.map_or(true, |prev| dt > prev) {
                        latest_created_at = Some(dt);
                        latest_created_at_raw = manifest.created_at.clone();
                    }
                }
                // 合并：对每个数据库取最高 data_version 的状态
                for (db_name, state) in &manifest.databases {
                    let entry = merged_databases
                        .entry(db_name.clone())
                        .or_insert_with(|| state.clone());
                    if state.data_version > entry.data_version {
                        *entry = state.clone();
                    } else if state.data_version == entry.data_version
                        && !entry.checksum.is_empty()
                        && !state.checksum.is_empty()
                        && state.checksum != entry.checksum
                    {
                        merged_divergence.insert(db_name.clone());
                        entry.checksum = Self::DIVERGED_CHECKSUM_SENTINEL.to_string();
                    }
                }
                tracing::debug!(
                    "[sync] 合并设备清单: device={}, databases={}",
                    file_device_id,
                    manifest.databases.len()
                );
            }
        }

        if !merged_divergence.is_empty() {
            tracing::warn!(
                "[sync] 检测到同版本云端分叉数据库: {}",
                merged_divergence
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }

        // 向后兼容：如果没有新格式清单，回退到旧的单文件
        if !any_found {
            if let Some(bytes) = storage
                .get(Self::LEGACY_MANIFEST_KEY)
                .await
                .map_err(|e| SyncError::Network(format!("下载旧版清单失败: {}", e)))?
            {
                let decoded = self.decode_payload(&bytes)?;
                let manifest = serde_json::from_slice::<SyncManifest>(&decoded)
                    .map_err(|e| SyncError::Database(format!("解析旧版清单失败: {}", e)))?;
                // 旧清单来自另一设备（或自己），直接使用
                if manifest.device_id != self.device_id {
                    tracing::info!(
                        "[sync] 从旧版单清单迁移读取: device={}, databases={}",
                        manifest.device_id,
                        manifest.databases.len()
                    );
                    return Ok(manifest);
                }
            }
        }

        if !any_found && merged_databases.is_empty() {
            tracing::info!("[sync] 云端没有其他设备的同步清单");
            return Ok(SyncManifest {
                sync_transaction_id: String::new(),
                databases: HashMap::new(),
                status: SyncTransactionStatus::Complete,
                created_at: chrono::Utc::now().to_rfc3339(),
                device_id: String::new(),
            });
        }

        tracing::info!(
            "[sync] 合并云端清单完成: other_devices={}, merged_databases={}",
            files.len().saturating_sub(1),
            merged_databases.len()
        );

        Ok(SyncManifest {
            sync_transaction_id: uuid::Uuid::new_v4().to_string(),
            databases: merged_databases,
            status: SyncTransactionStatus::Complete,
            created_at: if latest_created_at_raw.is_empty() {
                chrono::Utc::now().to_rfc3339()
            } else {
                latest_created_at_raw
            },
            device_id: "merged".to_string(),
        })
    }

    /// 上传变更数据（v1 旧格式：仅 ChangeLogEntry 元数据，不含行数据）
    ///
    /// **已废弃**：新代码应使用 `upload_enriched_changes`，它携带完整记录数据。
    /// 此方法仅保留用于极端回退场景。
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `changes` - 待上传的变更数据
    ///
    /// # 返回
    /// * `Ok(())` - 上传成功
    /// * `Err(SyncError)` - 上传失败
    pub async fn upload_changes(
        &self,
        storage: &dyn CloudStorage,
        changes: &PendingChanges,
    ) -> Result<(), SyncError> {
        if !changes.has_changes() {
            tracing::debug!("[sync] 没有变更需要上传");
            return Ok(());
        }

        // 生成变更数据文件的键（版本使用秒级时间戳，与 legacy 文件同一版本空间）
        // 秒级冲突由 build_change_key 的 UUID nonce 防护
        let version = chrono::Utc::now().timestamp() as u64;
        let key = self.build_change_key(version);

        let json = serde_json::to_vec_pretty(changes)
            .map_err(|e| SyncError::Database(format!("序列化变更数据失败: {}", e)))?;

        // [P0-2] 保持与新链路一致的加密行为
        let payload = self.encode_payload(&json)?;

        storage
            .put(&key, &payload)
            .await
            .map_err(|e| SyncError::Network(format!("上传变更数据失败: {}", e)))?;

        tracing::info!(
            "[sync] 变更数据已上传(legacy): device={}, count={}, key={}, encrypted={}",
            self.device_id,
            changes.total_count,
            key,
            self.encryption_enabled()
        );

        Ok(())
    }

    /// 上传带完整数据的变更（新链路）
    ///
    /// 将带完整记录数据的 `SyncChangeWithData` 序列化并上传到云端。
    /// 这确保下载端可以直接回放变更，无需再查询源数据库。
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `changes` - 带完整数据的变更列表
    pub async fn upload_enriched_changes(
        &self,
        storage: &dyn CloudStorage,
        changes: &[SyncChangeWithData],
        progress: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), SyncError> {
        if changes.is_empty() {
            tracing::debug!("[sync] 没有变更需要上传");
            return Ok(());
        }

        // 版本使用秒级时间戳，与 legacy 文件同一版本空间
        let version = chrono::Utc::now().timestamp() as u64;
        let key = self.build_change_key(version);

        // 序列化为带完整数据的新格式
        let payload = SyncChangesPayload {
            changes: changes.to_vec(),
            total_count: changes.len(),
            device_id: self.device_id.clone(),
            format_version: 2, // v2 = 带完整数据
        };

        // Phase 5 Optimization: Compact JSON + Zstd Compression
        // 1. Serialize to compact JSON
        let json = serde_json::to_vec(&payload)
            .map_err(|e| SyncError::Database(format!("序列化变更数据失败: {}", e)))?;

        // 2. Compress using Zstd (default level 0 is usually 3)
        //    **顺序重要**：先压缩后加密。密文几乎不可压缩，如果反过来会浪费 CPU 且
        //    文件反而变大；而且若先加密再压，解密端必须先解压再解密，流程不对称。
        let compressed = zstd::stream::encode_all(std::io::Cursor::new(json), 0)
            .map_err(|e| SyncError::Database(format!("压缩变更数据失败: {}", e)))?;

        // 3. [P0-2] 可选端到端加密（AES-256-GCM + Argon2id）
        let final_bytes = self.encode_payload(&compressed)?;

        let compressed_size = compressed.len();
        let uploaded_size = final_bytes.len();
        let _total_count = payload.total_count;

        if let Some(cb) = progress {
            // 有进度回调：写入临时文件，通过 put_file 流式上传以实时汇报字节进度
            let tmp = tempfile::NamedTempFile::new()
                .map_err(|e| SyncError::Database(format!("创建临时上传文件失败: {}", e)))?;
            std::fs::write(tmp.path(), &final_bytes)
                .map_err(|e| SyncError::Database(format!("写入临时上传文件失败: {}", e)))?;
            storage
                .put_file(&key, tmp.path(), Some(cb))
                .await
                .map_err(|e| SyncError::Network(format!("上传变更数据失败: {}", e)))?;
        } else {
            // 无进度回调：直接 PUT 字节，带指数退避重试
            // [P3 Fix] 降低为 2 次，避免与传输层重试叠加
            retry_async("上传变更数据", 2, || {
                let final_bytes = final_bytes.clone();
                let key = key.clone();
                async move {
                    storage
                        .put(&key, &final_bytes)
                        .await
                        .map_err(|e| SyncError::Network(format!("上传变更数据失败: {}", e)))
                }
            })
            .await?;
        }

        tracing::info!(
            "[sync] 带完整数据的变更已上传: device={}, count={}, key={}, compressed_size={}, uploaded_size={}, encrypted={}",
            self.device_id,
            changes.len(),
            key,
            compressed_size,
            uploaded_size,
            self.encryption_enabled()
        );

        Ok(())
    }

    /// 下载变更数据（支持新旧多种格式）
    ///
    /// 从云端下载指定版本之后的所有变更数据。
    /// - v3（信封）：`SyncEnvelope`，含完整记录数据 + 格式层逐条校验
    /// - v2：`SyncChangesPayload`，包含完整记录数据
    /// - v1：`PendingChanges`，仅含 ChangeLogEntry 元数据
    ///
    /// 返回统一的 `Vec<SyncChangeWithData>`，v3/v2 数据已含 `data` 字段，
    /// v1 的 INSERT/UPDATE 变更 `data` 字段为 None（回放时会记录告警并跳过）。
    /// v3 单条格式校验失败的记录进入 `quarantined_records`（隔离跳过，非致命）。
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `since_version` - 起始版本号（时间戳），获取此版本之后的变更
    /// * `per_db_since` - 各数据库的起始版本号（用于跨库过滤）
    ///
    /// # 返回
    /// * `Ok(DownloadChangesResult)` - 下载的变更数据（含完整记录）及非致命解析告警
    /// * `Err(SyncError)` - 下载失败
    pub async fn download_changes(
        &self,
        storage: &dyn CloudStorage,
        since_version: u64,
        per_db_since: Option<&HashMap<String, u64>>,
    ) -> Result<DownloadChangesResult, SyncError> {
        let files = storage
            .list(Self::CHANGES_PREFIX)
            .await
            .map_err(|e| SyncError::Network(format!("列出变更文件失败: {}", e)))?;

        let mut all_changes: Vec<(u64, SyncChangeWithData)> = Vec::new();
        let mut skipped_self = 0usize;
        let mut decode_failures: Vec<String> = Vec::new();
        // v3 SyncEnvelope 单条格式校验失败的记录（隔离跳过，非致命，随结果上报）
        let mut quarantined_records: Vec<QuarantineRow> = Vec::new();

        for file in files {
            // 跳过本设备上传的变更文件，避免回声下载
            // 路径格式: data_governance/changes/{device_id}/{version}-{nonce}.json[.zst]
            if Self::is_own_change_file(&file.key, &self.device_id) {
                skipped_self += 1;
                continue;
            }

            if let Some(version) = Self::parse_version_from_key(&file.key) {
                // >= 防止同秒上传的变更被跳过，apply 层幂等保证安全
                if version >= since_version {
                    if let Some(data) = storage
                        .get(&file.key)
                        .await
                        .map_err(|e| SyncError::Network(format!("下载变更文件失败: {}", e)))?
                    {
                        // [P0-2] 解密顺序：先 decode_payload（若带 DSBK 魔数则解密，
                        // 否则直通），再 zstd 解压。失败时细分错误来源：
                        // - 解密失败 → 记为 decode_failure，不致命
                        // - 解密成功但 zstd 失败 → fallback 到原明文（兼容极老的未压缩格式）
                        let decrypted = match self.decode_payload(&data) {
                            Ok(v) => v,
                            Err(e) => {
                                decode_failures.push(file.key.clone());
                                tracing::warn!(
                                    "[sync] 跳过无法解密的变更文件: key={}, error={}",
                                    file.key,
                                    e
                                );
                                continue;
                            }
                        };
                        let decoded_data =
                            zstd::stream::decode_all(std::io::Cursor::new(decrypted.as_slice()))
                                .unwrap_or(decrypted);

                        // 格式解析顺序（写入顺序）: v2 (SyncChangesPayload) → v3 (SyncEnvelope) → v1 (PendingChanges)
                        // 任一成功即视为已解析; 全部失败记 decode_failures（非致命，已有语义）。
                        // v3 要求 format_version >= 3，防止缺字段的老 JSON 被误识别为信封。
                        let mut parsed = false;

                        if let Ok(payload) =
                            serde_json::from_slice::<SyncChangesPayload>(&decoded_data)
                        {
                            parsed = true;
                            tracing::debug!(
                                "[sync] 下载变更文件(v2): key={}, count={}",
                                file.key,
                                payload.total_count
                            );
                            for change in payload.changes {
                                if let Some(db) = change.database_name.as_deref() {
                                    if let Some(db_since) = per_db_since.and_then(|m| m.get(db)) {
                                        if version < *db_since {
                                            continue;
                                        }
                                    }
                                }
                                all_changes.push((version, change));
                            }
                        }

                        if !parsed {
                            if let Ok(envelope) =
                                serde_json::from_slice::<SyncEnvelope>(&decoded_data)
                            {
                                if envelope.format_version >= 3 {
                                    parsed = true;
                                    // v3: 逐条格式校验，不合规进 quarantine（跳过继续），
                                    // 合规条目转成 SyncChangeWithData 进入现有应用流，
                                    // 不改变任何应用逻辑
                                    let mut file_quarantined: Vec<QuarantineRow> = Vec::new();
                                    for change in &envelope.changes {
                                        if let Err(reason) = validate_change_payload(change) {
                                            file_quarantined.push(QuarantineRow {
                                                table_name: change.table_name.clone(),
                                                record_id: change.record_id.clone(),
                                                reason,
                                            });
                                            continue;
                                        }
                                        let converted = change
                                            .to_sync_change_with_data(&envelope.created_at_hlc);
                                        if let Some(db) = converted.database_name.as_deref() {
                                            if let Some(db_since) =
                                                per_db_since.and_then(|m| m.get(db))
                                            {
                                                if version < *db_since {
                                                    continue;
                                                }
                                            }
                                        }
                                        all_changes.push((version, converted));
                                    }
                                    if file_quarantined.is_empty() {
                                        tracing::debug!(
                                            "[sync] 下载变更文件(v3): key={}, count={}",
                                            file.key,
                                            envelope.changes.len()
                                        );
                                    } else {
                                        tracing::warn!(
                                            "[sync] 下载变更文件(v3): key={}, count={}, quarantined={}, 示例: {}/{} ({})",
                                            file.key,
                                            envelope.changes.len(),
                                            file_quarantined.len(),
                                            file_quarantined[0].table_name,
                                            file_quarantined[0].record_id,
                                            file_quarantined[0].reason
                                        );
                                        quarantined_records.extend(file_quarantined);
                                    }
                                }
                                // format_version < 3: 不是合法 v3 信封，落回 v1 继续尝试
                            }
                        }

                        if !parsed {
                            if let Ok(changes) =
                                serde_json::from_slice::<PendingChanges>(&decoded_data)
                            {
                                parsed = true;
                                tracing::warn!(
                                    "[sync] 下载变更文件(v1/旧格式，数据不完整): key={}, count={}",
                                    file.key,
                                    changes.total_count
                                );
                                for entry in &changes.entries {
                                    let change = SyncChangeWithData::from_entry(entry);
                                    if let Some(db) = change.database_name.as_deref() {
                                        if let Some(db_since) =
                                            per_db_since.and_then(|m| m.get(db))
                                        {
                                            if version < *db_since {
                                                continue;
                                            }
                                        }
                                    }
                                    all_changes.push((version, change));
                                }
                            }
                        }

                        if !parsed {
                            decode_failures.push(file.key.clone());
                            tracing::error!("[sync] 无法解析变更文件: key={}", file.key);
                        }
                    }
                }
            }
        }

        if !decode_failures.is_empty() {
            let samples = decode_failures
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                "[sync] 存在 {} 个无法解析的变更文件，已跳过（示例: {}）",
                decode_failures.len(),
                samples
            );
        }

        // 使用时间戳归一化排序（兼容 SQLite datetime 和 RFC 3339 格式）
        all_changes.sort_by(|a, b| {
            let (a_version, a_change) = a;
            let (b_version, b_change) = b;

            match a_version.cmp(b_version) {
                std::cmp::Ordering::Equal => {}
                ord => return ord,
            }

            let ta = Self::parse_flexible_timestamp(&a_change.changed_at);
            let tb = Self::parse_flexible_timestamp(&b_change.changed_at);
            match (ta, tb) {
                (Some(a_dt), Some(b_dt)) => a_dt.cmp(&b_dt),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a_change.changed_at.cmp(&b_change.changed_at),
            }
            .then_with(|| a_change.database_name.cmp(&b_change.database_name))
            .then_with(|| a_change.table_name.cmp(&b_change.table_name))
            .then_with(|| a_change.record_id.cmp(&b_change.record_id))
            .then_with(|| a_change.operation.as_str().cmp(b_change.operation.as_str()))
            .then_with(|| a_change.change_log_id.cmp(&b_change.change_log_id))
        });

        if skipped_self > 0 {
            tracing::debug!(
                "[sync] 跳过本设备变更文件: {} 个（避免回声下载）",
                skipped_self
            );
        }

        tracing::info!(
            "[sync] 从云端下载变更: since={}, total={}, skipped_self={}",
            since_version,
            all_changes.len(),
            skipped_self
        );

        Ok(DownloadChangesResult {
            changes: all_changes.into_iter().map(|(_, change)| change).collect(),
            decode_failures,
            quarantined_records,
        })
    }

    /// 判断变更文件是否属于本设备
    fn is_own_change_file(key: &str, self_device_id: &str) -> bool {
        // 路径: data_governance/changes/{device_id}/{version}-{nonce}.json[.zst]
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() >= 3 {
            // parts: ["data_governance", "changes", "{device_id}", "{filename}"]
            if let Some(device_part) = parts.get(2) {
                return *device_part == self_device_id;
            }
        }
        false
    }

    /// 从文件路径解析版本号
    pub(crate) fn parse_version_from_key(key: &str) -> Option<u64> {
        // 新格式: data_governance/changes/{device_id}/{version}-{nonce}.json.zst
        // 旧格式: data_governance/changes/{device_id}/{version}-{nonce}.json
        //     或: data_governance/changes/{device_id}/{version}.json
        key.rsplit('/')
            .next()
            .and_then(|filename| {
                filename
                    .strip_suffix(".json.zst")
                    .or_else(|| filename.strip_suffix(".json"))
            })
            .and_then(|stem| stem.split('-').next())
            .and_then(|version_str| version_str.parse().ok())
    }

    /// 将版本号归一化为秒级时间戳
    ///
    /// 历史代码可能将 sync_version 写入了毫秒值（>1e12）。
    /// 秒级时间戳范围大约是 1e9 ~ 2e9（1970-2038），
    /// 毫秒时间戳在 1e12 ~ 2e12。阈值 1e11 可安全区分。
    pub(crate) fn normalize_version_to_seconds(version: u64) -> u64 {
        const MILLIS_THRESHOLD: u64 = 100_000_000_000; // 1e11
        if version > MILLIS_THRESHOLD {
            version / 1000
        } else {
            version
        }
    }

    /// 构造变更文件 key（避免秒级冲突覆盖）
    pub(crate) fn build_change_key(&self, version: u64) -> String {
        let nonce = uuid::Uuid::new_v4();
        format!(
            "{}/{}/{}-{}.json.zst",
            Self::CHANGES_PREFIX,
            self.device_id,
            version,
            nonce
        )
    }

    /// 清理云端过期的变更文件
    ///
    /// 两级清理策略：
    /// 1. 本设备文件：删除版本号早于 `retention_days` 天前的文件
    /// 2. [P2 Fix] 任意设备文件：删除版本号早于 `retention_days * 3` 天前的文件，
    ///    解决退役/重装设备遗留的变更文件永久占用云端存储的问题。
    ///    3 倍宽限期确保即使设备长期离线，也有足够的窗口恢复同步。
    pub async fn prune_old_changes(
        &self,
        storage: &dyn CloudStorage,
        retention_days: u64,
    ) -> Result<usize, SyncError> {
        let own_cutoff =
            (chrono::Utc::now().timestamp() as u64).saturating_sub(retention_days * 86400);
        // [P2 Fix] 对其他设备的文件使用 3 倍宽限期
        let global_cutoff =
            (chrono::Utc::now().timestamp() as u64).saturating_sub(retention_days * 3 * 86400);

        let files = storage
            .list(Self::CHANGES_PREFIX)
            .await
            .map_err(|e| SyncError::Network(format!("列出变更文件失败: {}", e)))?;

        let mut deleted_own = 0usize;
        let mut deleted_stale = 0usize;
        for file in &files {
            let is_own = Self::is_own_change_file(&file.key, &self.device_id);
            let cutoff = if is_own { own_cutoff } else { global_cutoff };

            if let Some(raw_version) = Self::parse_version_from_key(&file.key) {
                let version = Self::normalize_version_to_seconds(raw_version);
                if version < cutoff {
                    match storage.delete(&file.key).await {
                        Ok(_) => {
                            if is_own {
                                deleted_own += 1;
                            } else {
                                deleted_stale += 1;
                            }
                            tracing::debug!("[sync] 已清理过期变更文件: {}", file.key);
                        }
                        Err(e) => {
                            tracing::warn!("[sync] 清理变更文件失败（跳过）: {}: {}", file.key, e);
                        }
                    }
                }
            }
        }

        let total_deleted = deleted_own + deleted_stale;
        if total_deleted > 0 {
            tracing::info!(
                "[sync] 云端变更文件清理完成: 删除 {} 个本设备旧文件（{}天）+ {} 个其他设备过期文件（{}天）",
                deleted_own,
                retention_days,
                deleted_stale,
                retention_days * 3
            );
        }

        Ok(total_deleted)
    }

    /// 执行完整的上传同步流程（v1 旧格式：不含完整行数据）
    ///
    /// **已废弃**：新代码应在调用方直接使用 `upload_enriched_changes` + `upload_manifest`。
    /// 此方法上传的 `PendingChanges` 仅含 ChangeLogEntry 元数据，下载端无法回放 INSERT/UPDATE。
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `pending` - 待上传的变更数据（已从数据库获取）
    /// * `local_manifest` - 本地同步清单
    ///
    /// # 返回
    /// * `(SyncExecutionResult, Vec<i64>)` - 同步执行结果和需要标记为已同步的变更 ID
    pub async fn execute_upload(
        &self,
        storage: &dyn CloudStorage,
        pending: &PendingChanges,
        local_manifest: &SyncManifest,
    ) -> Result<(SyncExecutionResult, Vec<i64>), SyncError> {
        let start = std::time::Instant::now();

        if !pending.has_changes() {
            return Ok((
                SyncExecutionResult {
                    success: true,
                    direction: SyncDirection::Upload,
                    changes_uploaded: 0,
                    changes_downloaded: 0,
                    conflicts_detected: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    error_message: None,
                    quarantined_records: vec![],
                },
                vec![],
            ));
        }

        // 1. 上传变更数据
        self.upload_changes(storage, pending).await?;

        // 2. 上传清单
        self.upload_manifest(storage, local_manifest).await?;

        // 3. 返回需要标记的变更 ID
        let change_ids = pending.get_change_ids();
        let changes_count = pending.total_count;

        Ok((
            SyncExecutionResult {
                success: true,
                direction: SyncDirection::Upload,
                changes_uploaded: changes_count,
                changes_downloaded: 0,
                conflicts_detected: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                error_message: None,
                quarantined_records: vec![],
            },
            change_ids,
        ))
    }

    /// 执行完整的下载同步流程
    ///
    /// 1. 从云端下载清单
    /// 2. 检测冲突
    /// 3. 下载变更数据
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `local_manifest` - 本地同步清单
    /// * `strategy` - 冲突合并策略
    ///
    /// # 返回
    /// * `(SyncExecutionResult, Vec<SyncChangeWithData>)` - 同步执行结果和下载的变更数据（含完整记录）
    pub async fn execute_download(
        &self,
        storage: &dyn CloudStorage,
        local_manifest: &SyncManifest,
        strategy: MergeStrategy,
    ) -> Result<(SyncExecutionResult, Vec<SyncChangeWithData>), SyncError> {
        let start = std::time::Instant::now();

        // 1. 下载云端清单
        let cloud_manifest = self.download_manifest(storage).await?;

        // 云端无清单事务时，仍兜底扫描 changes/，避免"变更已上传但清单缺失"导致不可见
        if cloud_manifest.sync_transaction_id.is_empty() {
            let per_db_since: HashMap<String, u64> = local_manifest
                .databases
                .iter()
                .map(|(name, state)| (name.clone(), state.data_version))
                .collect();
            let since_version = per_db_since.values().min().copied().unwrap_or(0);

            let downloaded = self
                .download_changes(storage, since_version, Some(&per_db_since))
                .await?;
            let warning = if downloaded.decode_failures.is_empty()
                && downloaded.quarantined_records.is_empty()
            {
                None
            } else {
                Some(format!(
                    "检测到 {} 个云端变更文件解析失败、{} 条记录格式校验未通过已隔离，均已跳过并继续同步。",
                    downloaded.decode_failures.len(),
                    downloaded.quarantined_records.len()
                ))
            };

            return Ok((
                SyncExecutionResult {
                    success: true,
                    direction: SyncDirection::Download,
                    changes_uploaded: 0,
                    changes_downloaded: downloaded.changes.len(),
                    conflicts_detected: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    error_message: warning,
                    // 隔离记录随结果上报，命令层聚合进 SyncExecutionResponse
                    quarantined_records: downloaded.quarantined_records,
                },
                downloaded.changes,
            ));
        }

        // 2. 检测冲突
        let detection = Self::detect_conflicts(local_manifest, &cloud_manifest)?;

        if detection.needs_migration {
            return Err(SyncError::SchemaMismatch {
                local: detection
                    .database_conflicts
                    .first()
                    .and_then(|c| c.local_state.as_ref())
                    .map(|s| s.schema_version)
                    .unwrap_or(0),
                cloud: detection
                    .database_conflicts
                    .first()
                    .and_then(|c| c.cloud_state.as_ref())
                    .map(|s| s.schema_version)
                    .unwrap_or(0),
            });
        }

        // 3. 如果有冲突且是手动模式，返回错误
        if detection.has_conflicts && strategy == MergeStrategy::Manual {
            return Err(SyncError::ManualResolutionRequired {
                count: detection.total_conflicts(),
            });
        }

        // 4. 下载变更数据
        // 使用最小数据版本作为文件级过滤，并按库进一步过滤
        let per_db_since: HashMap<String, u64> = local_manifest
            .databases
            .iter()
            .map(|(name, state)| (name.clone(), state.data_version))
            .collect();
        let since_version = per_db_since.values().min().copied().unwrap_or(0);

        let downloaded = self
            .download_changes(storage, since_version, Some(&per_db_since))
            .await?;
        let warning = if downloaded.decode_failures.is_empty()
            && downloaded.quarantined_records.is_empty()
        {
            None
        } else {
            Some(format!(
                "检测到 {} 个云端变更文件解析失败、{} 条记录格式校验未通过已隔离，均已跳过并继续同步。",
                downloaded.decode_failures.len(),
                downloaded.quarantined_records.len()
            ))
        };

        let conflicts_count = if detection.has_conflicts {
            detection.total_conflicts()
        } else {
            0
        };

        Ok((
            SyncExecutionResult {
                success: true,
                direction: SyncDirection::Download,
                changes_uploaded: 0,
                changes_downloaded: downloaded.changes.len(),
                conflicts_detected: conflicts_count,
                duration_ms: start.elapsed().as_millis() as u64,
                error_message: warning,
                // 隔离记录随结果上报，命令层聚合进 SyncExecutionResponse
                quarantined_records: downloaded.quarantined_records,
            },
            downloaded.changes,
        ))
    }

    /// 执行双向同步流程
    ///
    /// 1. 先执行下载同步
    /// 2. 再执行上传同步
    ///
    /// # 参数
    /// * `storage` - 云存储实例
    /// * `pending` - 待上传的变更数据（已从数据库获取）
    /// * `local_manifest` - 本地同步清单
    /// * `strategy` - 冲突合并策略
    ///
    /// # 返回
    /// * `(SyncExecutionResult, Vec<i64>, Vec<SyncChangeWithData>)` - 同步结果、需要标记的变更 ID、下载的变更（含完整数据）
    ///
    /// **重要**：此方法只执行下载，**不执行上传**。
    /// 调用方需自行调用 `upload_enriched_changes` + `upload_manifest` 上传带完整数据的变更。
    /// 这避免了"内部 v1 上传 + 外部 v2 上传"导致的重复/覆盖问题。
    pub async fn execute_bidirectional(
        &self,
        storage: &dyn CloudStorage,
        pending: &PendingChanges,
        local_manifest: &SyncManifest,
        strategy: MergeStrategy,
    ) -> Result<(SyncExecutionResult, Vec<i64>, Vec<SyncChangeWithData>), SyncError> {
        let start = std::time::Instant::now();

        // 1. 下载并应用云端变更
        let (download_result, downloaded_changes) = self
            .execute_download(storage, local_manifest, strategy)
            .await?;

        // 2. 上传由调用方负责（使用 enriched 数据），这里只返回需要标记的变更 ID
        let change_ids = pending.get_change_ids();
        let changes_count = pending.total_count;

        Ok((
            SyncExecutionResult {
                success: true,
                direction: SyncDirection::Bidirectional,
                changes_uploaded: changes_count,
                changes_downloaded: download_result.changes_downloaded,
                conflicts_detected: download_result.conflicts_detected,
                duration_ms: start.elapsed().as_millis() as u64,
                error_message: download_result.error_message,
                // 双向分支聚合下载阶段的隔离记录（上传无隔离概念）
                quarantined_records: download_result.quarantined_records,
            },
            change_ids,
            downloaded_changes,
        ))
    }

    /// 灵活解析时间戳，兼容 RFC 3339 和 SQLite datetime('now') 格式
    pub(crate) fn parse_flexible_timestamp(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        use chrono::{DateTime, NaiveDateTime, Utc};
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
            return Some(naive.and_utc());
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
            return Some(naive.and_utc());
        }
        None
    }
}

// [P0-2] 让 SyncManager 满足 tombstone 模块需要的 Codec 接口。
// 放在文件顶层（impl 块之外）以便 tombstone.rs 里的函数签名可以引用它。
#[cfg(feature = "data_governance")]
impl tombstone::PayloadCodec for SyncManager {
    fn encode(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError> {
        self.encode_payload(plaintext)
    }
    fn decode(&self, data: &[u8]) -> Result<Vec<u8>, SyncError> {
        self.decode_payload(data)
    }
}

impl SyncManager {
    // ========================================================================
    // 文件级云同步：工作区数据库（ws_*.db）+ VFS blobs
    // ========================================================================

    const WORKSPACES_MANIFEST_KEY: &'static str = "data_governance/workspaces_manifest.json";
    const WORKSPACES_CLOUD_PREFIX: &'static str = "data_governance/workspaces";
    const BLOBS_MANIFEST_KEY: &'static str = "data_governance/blobs_manifest.json";
    const BLOBS_CLOUD_PREFIX: &'static str = "data_governance/blobs";
    const ASSETS_MANIFEST_KEY: &'static str = "data_governance/assets_manifest.json";
    const ASSETS_CLOUD_PREFIX: &'static str = "data_governance/assets";
    const DIVERGED_CHECKSUM_SENTINEL: &'static str = "__cloud_diverged_same_version__";
    const ACTIVE_ASSET_DIRS: [&'static str; 7] = [
        "images",
        "notes_assets",
        "documents",
        "subjects",
        "textbooks",
        "audio",
        "videos",
    ];

    /// 同步工作区数据库（ws_*.db）与云端
    ///
    /// 策略：
    /// - 本地有，与云端 sha256 不同 → 上传（本地优先，保护运行中工作区）
    /// - 云端有，本地没有 → 下载
    /// - 失败不阻断主流程
    pub async fn sync_workspace_databases(
        &self,
        storage: &dyn CloudStorage,
        active_dir: &std::path::Path,
    ) -> Result<(), SyncError> {
        let workspaces_dir = active_dir.join("workspaces");

        // 1. 下载云端清单
        let cloud_manifest = self.download_workspaces_manifest(storage).await?;

        // 2. 扫描本地 ws_*.db
        let mut local_entries: HashMap<String, (std::path::PathBuf, String, u64)> = HashMap::new();
        if workspaces_dir.exists() {
            for entry in std::fs::read_dir(&workspaces_dir)
                .map_err(|e| SyncError::Database(format!("读取工作区目录失败: {}", e)))?
            {
                let entry =
                    entry.map_err(|e| SyncError::Database(format!("读取目录条目失败: {}", e)))?;
                let path = entry.path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if !name.starts_with("ws_") || !name.ends_with(".db") {
                    continue;
                }
                let ws_id = name.trim_end_matches(".db").to_string();
                // [P1 Fix] 使用 PASSIVE 模式代替 TRUNCATE，避免与并发写入者竞争。
                // PASSIVE 模式不会阻塞其他连接，也不会清空正在使用的 WAL 文件。
                // 设置 busy_timeout 防止在数据库被锁定时立即失败。
                if let Ok(conn) = rusqlite::Connection::open(&path) {
                    let _ = conn.execute_batch("PRAGMA busy_timeout = 1000");
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
                }
                let sha256 = backup_common::calculate_file_hash(&path).map_err(|e| {
                    SyncError::Database(format!("计算工作区数据库校验和失败 {:?}: {}", path, e))
                })?;
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                local_entries.insert(ws_id, (path, sha256, size));
            }
        }

        // 3. 上传本地新增或已修改的 ws_*.db（先收集待传列表，申报进度总数）
        let mut new_manifest = cloud_manifest.clone();
        let pending_uploads: Vec<(&String, &(std::path::PathBuf, String, u64))> = local_entries
            .iter()
            .filter(|(ws_id, (_, sha256, _))| match cloud_manifest.entries.get(*ws_id) {
                None => true,
                Some(ce) => ce.sha256 != *sha256,
            })
            .collect();
        super::emitter::report_file_sync_totals(pending_uploads.len() as u32);
        for (ws_id, (path, sha256, size)) in pending_uploads {
            let key = format!("{}/{}.db", Self::WORKSPACES_CLOUD_PREFIX, ws_id);
            match storage
                .put_file(&key, path, file_progress_callback("上传工作区数据库", ws_id.clone()))
                .await
            {
                Ok(_) => {
                    super::emitter::report_file_sync_file_done();
                    new_manifest.entries.insert(
                        ws_id.clone(),
                        WorkspaceEntry {
                            sha256: sha256.clone(),
                            size: *size,
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        },
                    );
                    tracing::info!("[sync] 工作区数据库已上传: {}", ws_id);
                }
                Err(e) => {
                    tracing::warn!("[sync] 工作区数据库上传失败（跳过）: {}: {}", ws_id, e);
                }
            }
        }

        // 4. 下载云端有但本地没有的 ws_*.db（先收集待传列表）
        if !workspaces_dir.exists() {
            let _ = std::fs::create_dir_all(&workspaces_dir);
        }
        let pending_downloads: Vec<(&String, &WorkspaceEntry)> = cloud_manifest
            .entries
            .iter()
            .filter(|(ws_id, _)| !local_entries.contains_key(ws_id.as_str()))
            .collect();
        super::emitter::report_file_sync_totals(pending_downloads.len() as u32);
        for (ws_id, cloud_entry) in pending_downloads {
            let dest = workspaces_dir.join(format!("{}.db", ws_id));
            let key = format!("{}/{}.db", Self::WORKSPACES_CLOUD_PREFIX, ws_id);
            match storage
                .get_file(
                    &key,
                    &dest,
                    Some(&cloud_entry.sha256),
                    file_progress_callback("下载工作区数据库", ws_id.clone()),
                )
                .await
            {
                Ok(_) => {
                    super::emitter::report_file_sync_file_done();
                    tracing::info!("[sync] 工作区数据库已下载: {}", ws_id);
                }
                Err(e) => {
                    tracing::warn!("[sync] 工作区数据库下载失败（跳过）: {}: {}", ws_id, e);
                }
            }
        }

        // 5. 仅在有上传时更新云端清单
        if new_manifest.entries != cloud_manifest.entries {
            new_manifest.updated_at = chrono::Utc::now().to_rfc3339();
            let json = serde_json::to_vec(&new_manifest)
                .map_err(|e| SyncError::Database(format!("序列化工作区清单失败: {}", e)))?;
            // [P0-2] 可选加密
            let payload = self.encode_payload(&json)?;
            storage
                .put(Self::WORKSPACES_MANIFEST_KEY, &payload)
                .await
                .map_err(|e| SyncError::Network(format!("上传工作区清单失败: {}", e)))?;
        }

        Ok(())
    }

    async fn download_workspaces_manifest(
        &self,
        storage: &dyn CloudStorage,
    ) -> Result<WorkspacesManifest, SyncError> {
        match storage
            .get(Self::WORKSPACES_MANIFEST_KEY)
            .await
            .map_err(|e| SyncError::Network(format!("获取工作区清单失败: {}", e)))?
        {
            Some(bytes) => {
                let decoded = self.decode_payload(&bytes)?;
                serde_json::from_slice::<WorkspacesManifest>(&decoded)
                    .map_err(|e| SyncError::Database(format!("解析工作区清单失败: {}", e)))
            }
            None => Ok(WorkspacesManifest::default()),
        }
    }

    /// 同步 VFS blobs（内容寻址，纯增量，无冲突）
    ///
    const BLOB_MAX_RETRIES: u32 = 3;
    const BLOB_RETRY_BASE_MS: u64 = 500;

    /// 策略：
    /// - 本地有但云端没有 → 上传
    /// - 云端有但本地没有 → 下载（带重试）
    /// - hash 即内容唯一标识，天然去重，无冲突问题
    ///
    /// 返回 `BlobSyncOutcome` 以便调用方区分完全成功与部分失败。
    pub async fn sync_vfs_blobs(
        &self,
        storage: &dyn CloudStorage,
        blobs_dir: &std::path::Path,
    ) -> Result<BlobSyncOutcome, SyncError> {
        if !blobs_dir.exists() {
            return Ok(BlobSyncOutcome::default());
        }

        let cloud_manifest = self.download_blobs_manifest(storage).await?;

        let mut local_blobs: HashMap<String, std::path::PathBuf> = HashMap::new();
        Self::scan_blobs_dir(blobs_dir, &mut local_blobs)?;

        let mut new_manifest = cloud_manifest.clone();
        let mut uploaded = 0usize;
        let mut upload_failures: Vec<String> = Vec::new();

        // 先收集待传列表，申报进度总数（进度按 真实比例 推进）
        let pending_uploads: Vec<(&String, &std::path::PathBuf)> = local_blobs
            .iter()
            .filter(|(hash, _)| !cloud_manifest.entries.contains_key(hash.as_str()))
            .collect();
        super::emitter::report_file_sync_totals(pending_uploads.len() as u32);

        for (hash, path) in pending_uploads {
            let relative = path
                .strip_prefix(blobs_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("{}/{}", Self::BLOBS_CLOUD_PREFIX, relative);
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

            let mut last_err = String::new();
            let mut ok = false;
            for attempt in 0..Self::BLOB_MAX_RETRIES {
                match storage
                    .put_file(&key, path, file_progress_callback("上传附件", relative.clone()))
                    .await
                {
                    Ok(_) => {
                        super::emitter::report_file_sync_file_done();
                        new_manifest.entries.insert(
                            hash.clone(),
                            BlobEntry {
                                relative_path: relative.clone(),
                                size,
                            },
                        );
                        uploaded += 1;
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        if attempt + 1 < Self::BLOB_MAX_RETRIES {
                            let delay = Self::BLOB_RETRY_BASE_MS * (1u64 << attempt);
                            tracing::warn!(
                                "[sync] blob 上传重试 {}/{}: {}: {}",
                                attempt + 1,
                                Self::BLOB_MAX_RETRIES,
                                hash,
                                e
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                    }
                }
            }
            if !ok {
                tracing::error!("[sync] blob 上传最终失败: {}: {}", hash, last_err);
                upload_failures.push(hash.clone());
            }
        }

        let mut downloaded_count = 0usize;
        let mut download_failures: Vec<String> = Vec::new();

        let pending_downloads: Vec<(&String, &BlobEntry)> = cloud_manifest
            .entries
            .iter()
            .filter(|(hash, _)| !local_blobs.contains_key(hash.as_str()))
            .collect();
        super::emitter::report_file_sync_totals(pending_downloads.len() as u32);

        for (hash, cloud_entry) in pending_downloads {
            let dest = blobs_dir.join(&cloud_entry.relative_path);
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let key = format!("{}/{}", Self::BLOBS_CLOUD_PREFIX, cloud_entry.relative_path);

            let mut last_err = String::new();
            let mut ok = false;
            for attempt in 0..Self::BLOB_MAX_RETRIES {
                match storage
                    .get_file(
                        &key,
                        &dest,
                        Some(hash),
                        file_progress_callback("下载附件", cloud_entry.relative_path.clone()),
                    )
                    .await
                {
                    Ok(_) => {
                        let actual_size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                        if cloud_entry.size > 0 && actual_size != cloud_entry.size {
                            last_err = format!(
                                "blob 大小不匹配: 期望 {} 字节, 实际 {} 字节",
                                cloud_entry.size, actual_size
                            );
                            let _ = std::fs::remove_file(&dest);
                            if attempt + 1 < Self::BLOB_MAX_RETRIES {
                                let delay = Self::BLOB_RETRY_BASE_MS * (1u64 << attempt);
                                tracing::warn!(
                                    "[sync] blob 大小校验失败，重试 {}/{}: {}: {}",
                                    attempt + 1,
                                    Self::BLOB_MAX_RETRIES,
                                    hash,
                                    last_err
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            }
                            continue;
                        }
                        downloaded_count += 1;
                        super::emitter::report_file_sync_file_done();
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        // 清理可能写到一半的文件
                        let _ = std::fs::remove_file(&dest);
                        if attempt + 1 < Self::BLOB_MAX_RETRIES {
                            let delay = Self::BLOB_RETRY_BASE_MS * (1u64 << attempt);
                            tracing::warn!(
                                "[sync] blob 下载重试 {}/{}: {}: {}",
                                attempt + 1,
                                Self::BLOB_MAX_RETRIES,
                                hash,
                                e
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                    }
                }
            }
            if !ok {
                tracing::error!("[sync] blob 下载最终失败: {}: {}", hash, last_err);
                download_failures.push(hash.clone());
            }
        }

        if uploaded > 0 || downloaded_count > 0 {
            tracing::info!(
                "[sync] blob 同步: 上传 {}, 下载 {}, 上传失败 {}, 下载失败 {}",
                uploaded,
                downloaded_count,
                upload_failures.len(),
                download_failures.len()
            );
        }

        if uploaded > 0 {
            new_manifest.updated_at = chrono::Utc::now().to_rfc3339();
            let json = serde_json::to_vec(&new_manifest)
                .map_err(|e| SyncError::Database(format!("序列化 blob 清单失败: {}", e)))?;
            // [P0-2] 可选加密（注意：这里加密的是 **清单** 文件，blob 原文件本身不加密）
            let payload = self.encode_payload(&json)?;
            storage
                .put(Self::BLOBS_MANIFEST_KEY, &payload)
                .await
                .map_err(|e| SyncError::Network(format!("上传 blob 清单失败: {}", e)))?;
        }

        Ok(BlobSyncOutcome {
            uploaded,
            downloaded: downloaded_count,
            upload_failures,
            download_failures,
        })
    }

    async fn download_blobs_manifest(
        &self,
        storage: &dyn CloudStorage,
    ) -> Result<BlobsManifest, SyncError> {
        match storage
            .get(Self::BLOBS_MANIFEST_KEY)
            .await
            .map_err(|e| SyncError::Network(format!("获取 blob 清单失败: {}", e)))?
        {
            Some(bytes) => {
                let decoded = self.decode_payload(&bytes)?;
                serde_json::from_slice::<BlobsManifest>(&decoded)
                    .map_err(|e| SyncError::Database(format!("解析 blob 清单失败: {}", e)))
            }
            None => Ok(BlobsManifest::default()),
        }
    }

    /// 同步关键资产目录（除 vfs_blobs/workspaces 外）
    pub async fn sync_asset_directories(
        &self,
        storage: &dyn CloudStorage,
        active_dir: &std::path::Path,
        app_data_dir: &std::path::Path,
    ) -> Result<AssetSyncOutcome, SyncError> {
        let cloud_manifest = self.download_assets_manifest(storage).await?;

        let mut local_files: HashMap<String, (std::path::PathBuf, String, u64)> = HashMap::new();
        for dir_name in Self::ACTIVE_ASSET_DIRS {
            let dir = active_dir.join(dir_name);
            if !dir.exists() {
                continue;
            }
            Self::scan_asset_tree("active", dir_name, &dir, &dir, &mut local_files)?;
        }

        let app_side = app_data_dir.join("pdf_ocr_sessions");
        if app_side.exists() {
            Self::scan_asset_tree(
                "app_data",
                "pdf_ocr_sessions",
                &app_side,
                &app_side,
                &mut local_files,
            )?;
        }

        let mut new_manifest = cloud_manifest.clone();
        let mut uploaded = 0usize;
        let mut upload_failures = Vec::new();

        // 先收集待传列表，申报进度总数（进度按 真实比例 推进）
        let pending_uploads: Vec<(&String, &(std::path::PathBuf, String, u64))> = local_files
            .iter()
            .filter(|(key, (_, sha256, size))| match cloud_manifest.entries.get(*key) {
                None => true,
                Some(entry) => entry.sha256 != *sha256 || entry.size != *size,
            })
            .collect();
        super::emitter::report_file_sync_totals(pending_uploads.len() as u32);

        for (key, (path, sha256, size)) in pending_uploads {
            let remote_key = format!("{}/{}", Self::ASSETS_CLOUD_PREFIX, key);
            match storage
                .put_file(&remote_key, path, file_progress_callback("上传资产文件", key.clone()))
                .await
            {
                Ok(_) => {
                    super::emitter::report_file_sync_file_done();
                    new_manifest.entries.insert(
                        key.clone(),
                        AssetFileEntry {
                            sha256: sha256.clone(),
                            size: *size,
                        },
                    );
                    uploaded += 1;
                }
                Err(e) => {
                    tracing::warn!("[sync] 资产上传失败（跳过）: {}: {}", key, e);
                    upload_failures.push(key.clone());
                }
            }
        }

        let mut downloaded = 0usize;
        let mut download_failures = Vec::new();
        let pending_downloads: Vec<(&String, &AssetFileEntry)> = cloud_manifest
            .entries
            .iter()
            .filter(|(key, _)| !local_files.contains_key(key.as_str()))
            .collect();
        super::emitter::report_file_sync_totals(pending_downloads.len() as u32);

        for (key, entry) in pending_downloads {
            let Some(dest) = Self::asset_local_path_from_key(active_dir, app_data_dir, key) else {
                tracing::warn!("[sync] 非法资产键，跳过下载: {}", key);
                continue;
            };
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let remote_key = format!("{}/{}", Self::ASSETS_CLOUD_PREFIX, key);
            match storage
                .get_file(
                    &remote_key,
                    &dest,
                    Some(&entry.sha256),
                    file_progress_callback("下载资产文件", key.clone()),
                )
                .await
            {
                Ok(_) => {
                    super::emitter::report_file_sync_file_done();
                    downloaded += 1;
                }
                Err(e) => {
                    tracing::warn!("[sync] 资产下载失败（跳过）: {}: {}", key, e);
                    let _ = std::fs::remove_file(&dest);
                    download_failures.push(key.clone());
                }
            }
        }

        if new_manifest.entries != cloud_manifest.entries {
            new_manifest.updated_at = chrono::Utc::now().to_rfc3339();
            let json = serde_json::to_vec(&new_manifest)
                .map_err(|e| SyncError::Database(format!("序列化资产清单失败: {}", e)))?;
            // [P0-2] 可选加密
            let payload = self.encode_payload(&json)?;
            storage
                .put(Self::ASSETS_MANIFEST_KEY, &payload)
                .await
                .map_err(|e| SyncError::Network(format!("上传资产清单失败: {}", e)))?;
        }

        Ok(AssetSyncOutcome {
            uploaded,
            downloaded,
            upload_failures,
            download_failures,
        })
    }

    async fn download_assets_manifest(
        &self,
        storage: &dyn CloudStorage,
    ) -> Result<AssetDirsManifest, SyncError> {
        match storage
            .get(Self::ASSETS_MANIFEST_KEY)
            .await
            .map_err(|e| SyncError::Network(format!("获取资产清单失败: {}", e)))?
        {
            Some(bytes) => {
                let decoded = match self.decode_payload(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("[sync] 资产清单解密失败，忽略并继续: {}", e);
                        return Ok(AssetDirsManifest::default());
                    }
                };
                match serde_json::from_slice::<AssetDirsManifest>(&decoded) {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        tracing::warn!("[sync] 资产清单损坏，忽略并继续: {}", e);
                        Ok(AssetDirsManifest::default())
                    }
                }
            }
            None => Ok(AssetDirsManifest::default()),
        }
    }

    fn scan_asset_tree(
        root_alias: &str,
        top_dir: &str,
        base_dir: &std::path::Path,
        current_dir: &std::path::Path,
        out: &mut HashMap<String, (std::path::PathBuf, String, u64)>,
    ) -> Result<(), SyncError> {
        for entry in std::fs::read_dir(current_dir)
            .map_err(|e| SyncError::Database(format!("读取资产目录失败: {}", e)))?
        {
            let entry =
                entry.map_err(|e| SyncError::Database(format!("读取资产条目失败: {}", e)))?;
            let path = entry.path();
            if path.is_dir() {
                Self::scan_asset_tree(root_alias, top_dir, base_dir, &path, out)?;
                continue;
            }
            if !path.is_file() {
                continue;
            }

            let rel = path
                .strip_prefix(base_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("{}/{}/{}", root_alias, top_dir, rel);

            // ★ perf-audit A5#4: mtime+size 快路径——同会话内重复同步时零变更文件
            // 跳过全量 sha256(旧实现每次同步全量哈希 2GB 资产;跨重启持久化需清单加
            // mtime 字段、涉远端格式兼容,留专项)。metadata 复用 DirEntry 自带,免二次 stat。
            let metadata = entry.metadata().ok();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = metadata
                .as_ref()
                .and_then(|m| m.modified().ok());
            let sha256 = {
                let memo = ASSET_HASH_MEMO
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let (Some(mt), Some((m_sha, m_size, m_mtime))) =
                    (mtime, memo.get(&key))
                {
                    if *m_size == size && *m_mtime == mt {
                        m_sha.clone()
                    } else {
                        Self::hash_and_memo(&path, &key, size, Some(mt))
                            .map_err(|e| {
                                SyncError::Database(format!(
                                    "计算资产文件校验和失败 {:?}: {}",
                                    path, e
                                ))
                            })?
                    }
                } else {
                    Self::hash_and_memo(&path, &key, size, mtime)
                        .map_err(|e| {
                            SyncError::Database(format!(
                                "计算资产文件校验和失败 {:?}: {}",
                                path, e
                            ))
                        })?
                }
            };
            out.insert(key, (path, sha256, size));
        }
        Ok(())
    }

    /// 哈希并写入备忘(在 memo 锁外计算,避免持锁做重 IO)
    fn hash_and_memo(
        path: &std::path::Path,
        key: &str,
        size: u64,
        mtime: Option<std::time::SystemTime>,
    ) -> Result<String, SyncError> {
        let sha = backup_common::calculate_file_hash(path)
            .map_err(|e| SyncError::Database(format!("计算资产文件校验和失败: {}", e)))?;
        ASSET_HASH_MEMO
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.to_string(),
                (sha.clone(), size, mtime.unwrap_or(std::time::SystemTime::UNIX_EPOCH)),
            );
        Ok(sha)
    }

    fn asset_local_path_from_key(
        active_dir: &std::path::Path,
        app_data_dir: &std::path::Path,
        key: &str,
    ) -> Option<std::path::PathBuf> {
        let mut parts = key.splitn(3, '/');
        let root = parts.next()?;
        let top = parts.next()?;
        let rel = parts.next()?;
        let rel_path = std::path::PathBuf::from(rel);
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        let base = match root {
            "active" => active_dir,
            "app_data" => app_data_dir,
            _ => return None,
        };
        Some(base.join(top).join(rel_path))
    }

    fn scan_blobs_dir(
        dir: &std::path::Path,
        result: &mut HashMap<String, std::path::PathBuf>,
    ) -> Result<(), SyncError> {
        for entry in std::fs::read_dir(dir)
            .map_err(|e| SyncError::Database(format!("读取 blobs 目录失败: {}", e)))?
        {
            let entry =
                entry.map_err(|e| SyncError::Database(format!("读取目录条目失败: {}", e)))?;
            let path = entry.path();
            if path.is_dir() {
                Self::scan_blobs_dir(&path, result)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext != "tmp" {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        result.insert(stem.to_string(), path);
                    }
                }
            }
        }
        Ok(())
    }

    // ========================================================================
    // 删除传播 (Tombstone) — 修复 #6
    // ========================================================================

    /// 标记 blob 已删除（本地调用）。后续 `sync_vfs_blobs_with_tombstones` 会把删除
    /// 传播到云端和其他设备。
    pub async fn mark_blob_deleted(
        &self,
        storage: &dyn CloudStorage,
        hash: &str,
        relative_path: Option<String>,
        size: Option<u64>,
    ) -> Result<(), SyncError> {
        // [P0-2] tombstone 清单也走 E2EE（self 实现了 PayloadCodec）
        let mut manifest = tombstone::download_blob_tombstones(storage, self).await?;
        manifest.entries.insert(
            hash.to_string(),
            tombstone::BlobTombstoneEntry {
                deleted_at: chrono::Utc::now().to_rfc3339(),
                device_id: self.device_id.clone(),
                size,
                relative_path,
            },
        );
        tombstone::upload_blob_tombstones(storage, self, manifest).await
    }

    /// 标记资产已删除
    pub async fn mark_asset_deleted(
        &self,
        storage: &dyn CloudStorage,
        key: &str,
        size: Option<u64>,
    ) -> Result<(), SyncError> {
        let mut manifest = tombstone::download_asset_tombstones(storage, self).await?;
        manifest.entries.insert(
            key.to_string(),
            tombstone::AssetTombstoneEntry {
                deleted_at: chrono::Utc::now().to_rfc3339(),
                device_id: self.device_id.clone(),
                size,
            },
        );
        tombstone::upload_asset_tombstones(storage, self, manifest).await
    }

    /// 同步 VFS blobs + 消费 tombstone（修复 #6）
    ///
    /// 与 `sync_vfs_blobs` 不同：先按 tombstone 清理本地与云端的已删 blob，
    /// 再走常规 "本地→上传 / 云端→下载" 流程。
    pub async fn sync_vfs_blobs_with_tombstones(
        &self,
        storage: &dyn CloudStorage,
        blobs_dir: &std::path::Path,
    ) -> Result<BlobSyncOutcome, SyncError> {
        // 1. 拉取 tombstone 并执行删除传播
        let tombstones = tombstone::download_blob_tombstones(storage, self).await?;
        if !tombstones.entries.is_empty() {
            let _ = tombstone::apply_blob_tombstones(
                storage,
                &tombstones,
                blobs_dir,
                Self::BLOBS_CLOUD_PREFIX,
            )
            .await?;

            // 同时从 blob manifest 里摘掉 tombstoned 条目
            // [P0-2] 读写都需要透明 encode/decode
            if let Ok(Some(bytes)) = storage.get(Self::BLOBS_MANIFEST_KEY).await {
                if let Ok(decoded) = self.decode_payload(&bytes) {
                    if let Ok(mut mf) = serde_json::from_slice::<BlobsManifest>(&decoded) {
                        let before = mf.entries.len();
                        for hash in tombstones.entries.keys() {
                            mf.entries.remove(hash);
                        }
                        if mf.entries.len() != before {
                            mf.updated_at = chrono::Utc::now().to_rfc3339();
                            if let Ok(json) = serde_json::to_vec(&mf) {
                                if let Ok(payload) = self.encode_payload(&json) {
                                    let _ = storage.put(Self::BLOBS_MANIFEST_KEY, &payload).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. 走标准上传/下载流程（现在云端/本地里已无 tombstoned 条目）
        self.sync_vfs_blobs(storage, blobs_dir).await
    }

    /// 同步资产目录 + 消费 asset tombstone
    ///
    /// 与 `sync_asset_directories` 不同：先按 tombstone 清理本地与云端的已删资产文件，
    /// 再走常规上传/下载流程。
    pub async fn sync_asset_directories_with_tombstones(
        &self,
        storage: &dyn CloudStorage,
        active_dir: &std::path::Path,
        app_data_dir: &std::path::Path,
    ) -> Result<AssetSyncOutcome, SyncError> {
        // 1. 拉取 asset tombstone 并删除本地/云端对应文件
        let tombstones = tombstone::download_asset_tombstones(storage, self).await?;
        if !tombstones.entries.is_empty() {
            for (key, _entry) in &tombstones.entries {
                // 云端删除
                let remote_key = format!("{}/{}", Self::ASSETS_CLOUD_PREFIX, key);
                if let Err(e) = storage.delete(&remote_key).await {
                    tracing::warn!("[sync] 删除云端资产失败（忽略）: {}: {}", remote_key, e);
                }
                // 本地删除
                if let Some(local) = Self::asset_local_path_from_key(active_dir, app_data_dir, key)
                {
                    if local.exists() {
                        let _ = std::fs::remove_file(&local);
                    }
                }
            }

            // 从云端资产清单摘掉 tombstoned 条目
            // [P0-2] 同样需要透明 encode/decode
            if let Ok(Some(bytes)) = storage.get(Self::ASSETS_MANIFEST_KEY).await {
                if let Ok(decoded) = self.decode_payload(&bytes) {
                    if let Ok(mut mf) = serde_json::from_slice::<AssetDirsManifest>(&decoded) {
                        let before = mf.entries.len();
                        for key in tombstones.entries.keys() {
                            mf.entries.remove(key);
                        }
                        if mf.entries.len() != before {
                            mf.updated_at = chrono::Utc::now().to_rfc3339();
                            if let Ok(json) = serde_json::to_vec(&mf) {
                                if let Ok(payload) = self.encode_payload(&json) {
                                    let _ = storage.put(Self::ASSETS_MANIFEST_KEY, &payload).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. 走标准同步流程
        self.sync_asset_directories(storage, active_dir, app_data_dir)
            .await
    }
}
