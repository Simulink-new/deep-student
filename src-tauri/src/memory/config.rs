use rusqlite::params;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::vfs::types::VfsFolder;

use super::error::MemoryResult;
use super::storage_trait::MemoryStorage;

const CONFIG_KEY_ROOT_FOLDER_ID: &str = "memory_root_folder_id";
const CONFIG_KEY_AUTO_CREATE_SUBFOLDERS: &str = "auto_create_subfolders";
const CONFIG_KEY_DEFAULT_CATEGORY: &str = "default_category";
const CONFIG_KEY_PRIVACY_MODE: &str = "privacy_mode";
const CONFIG_KEY_AUTO_EXTRACT_FREQUENCY: &str = "auto_extract_frequency";

const DEFAULT_FOLDER_TITLE: &str = "记忆";

/// 自动提取频率档位
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoExtractFrequency {
    /// 完全禁用自动提取
    Off,
    /// 平衡模式（默认）：每轮对话提取，内容门槛 10 字符
    Balanced,
    /// 积极模式：降低门槛（4 字符），更频繁的分类刷新和自进化
    Aggressive,
}

impl AutoExtractFrequency {
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "off" => Self::Off,
            "balanced" => Self::Balanced,
            "aggressive" => Self::Aggressive,
            other => {
                warn!(
                    "[Memory::Config] Unknown auto_extract_frequency '{}', defaulting to Balanced",
                    other
                );
                Self::Balanced
            }
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
        }
    }

    /// 内容最短门槛（字符数），低于此值的对话不触发提取
    pub fn content_min_chars(&self) -> usize {
        match self {
            Self::Off => usize::MAX,
            Self::Balanced => 10,
            Self::Aggressive => 4,
        }
    }

    /// 分类刷新条件：给定记忆总数，是否应刷新分类文件
    pub fn should_refresh_categories(&self, total_memories: usize) -> bool {
        match self {
            Self::Off => false,
            Self::Balanced => total_memories <= 5 || total_memories % 5 == 0,
            Self::Aggressive => true,
        }
    }

    /// ★ perf-audit A8#3: 写计数版刷新条件。
    /// 旧 `total % 5 == 0` 在总数恰停在 5 的倍数且不变时(UPDATE 不改总数)，
    /// 每次写都触发全分类刷新 = 每分类 1 次 LLM 摘要调用(7 分类 ≈ 每写 7 次 LLM 的调用风暴)。
    /// 改为自上次刷新累计 ≥5 次写才再刷；bootstrap 阶段(total<=5)保持每写刷新。
    pub fn should_refresh_categories_with_writes(
        &self,
        total_memories: usize,
        writes_since_refresh: usize,
    ) -> bool {
        match self {
            Self::Off => false,
            Self::Balanced => total_memories <= 5 || writes_since_refresh >= 5,
            Self::Aggressive => true,
        }
    }

    /// 自进化周期间隔（毫秒）
    pub fn evolution_interval_ms(&self) -> i64 {
        match self {
            Self::Off => i64::MAX,
            Self::Balanced => 30 * 60 * 1000,
            Self::Aggressive => 15 * 60 * 1000,
        }
    }
}

#[derive(Clone)]
pub struct MemoryConfig {
    storage: Arc<dyn MemoryStorage>,
}

impl MemoryConfig {
    pub fn new(storage: Arc<dyn MemoryStorage>) -> Self {
        Self { storage }
    }

    pub fn get(&self, key: &str) -> MemoryResult<Option<String>> {
        let conn = self.storage.conn()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM memory_config WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok();
        Ok(value.filter(|v| !v.is_empty()))
    }

    pub fn set(&self, key: &str, value: &str) -> MemoryResult<()> {
        let conn = self.storage.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO memory_config (key, value, updated_at) VALUES (?1, ?2, datetime('now'))",
            params![key, value],
        )?;
        debug!("[Memory::Config] Set {} = {}", key, value);
        Ok(())
    }

    pub fn get_root_folder_id(&self) -> MemoryResult<Option<String>> {
        self.get(CONFIG_KEY_ROOT_FOLDER_ID)
    }

    pub fn set_root_folder_id(&self, folder_id: &str) -> MemoryResult<()> {
        self.set(CONFIG_KEY_ROOT_FOLDER_ID, folder_id)
    }

    pub fn get_or_create_root_folder(&self) -> MemoryResult<String> {
        if let Some(folder_id) = self.get_root_folder_id()? {
            if self.storage.folder_exists(&folder_id)? {
                debug!("[Memory::Config] Using existing root folder: {}", folder_id);
                return Ok(folder_id);
            }
            warn!(
                "[Memory::Config] Configured folder {} not found, creating new one",
                folder_id
            );
        }

        let folder = VfsFolder::new(DEFAULT_FOLDER_TITLE.to_string(), None, None, None);
        self.storage.create_folder(&folder)?;
        self.set_root_folder_id(&folder.id)?;
        info!(
            "[Memory::Config] Created default memory folder: {} ({})",
            DEFAULT_FOLDER_TITLE, folder.id
        );
        Ok(folder.id)
    }

    pub fn create_root_folder(&self, title: &str) -> MemoryResult<String> {
        let folder = VfsFolder::new(title.to_string(), None, None, None);
        self.storage.create_folder(&folder)?;
        self.set_root_folder_id(&folder.id)?;
        info!(
            "[Memory::Config] Created memory root folder: {} ({})",
            title, folder.id
        );
        Ok(folder.id)
    }

    pub fn get_root_folder_title(&self) -> MemoryResult<Option<String>> {
        if let Some(folder_id) = self.get_root_folder_id()? {
            if let Some(folder) = self.storage.get_folder(&folder_id)? {
                return Ok(Some(folder.title));
            }
        }
        Ok(None)
    }

    pub fn is_auto_create_subfolders(&self) -> MemoryResult<bool> {
        Ok(self
            .get(CONFIG_KEY_AUTO_CREATE_SUBFOLDERS)?
            .map(|v| v == "true")
            .unwrap_or(true))
    }

    pub fn is_privacy_mode(&self) -> MemoryResult<bool> {
        Ok(self
            .get(CONFIG_KEY_PRIVACY_MODE)?
            .map(|v| v == "true")
            .unwrap_or(false))
    }

    pub fn set_privacy_mode(&self, enabled: bool) -> MemoryResult<()> {
        self.set(
            CONFIG_KEY_PRIVACY_MODE,
            if enabled { "true" } else { "false" },
        )
    }

    pub fn get_default_category(&self) -> MemoryResult<String> {
        Ok(self
            .get(CONFIG_KEY_DEFAULT_CATEGORY)?
            .unwrap_or_else(|| "通用".to_string()))
    }

    pub fn set_auto_create_subfolders(&self, enabled: bool) -> MemoryResult<()> {
        self.set(
            CONFIG_KEY_AUTO_CREATE_SUBFOLDERS,
            if enabled { "true" } else { "false" },
        )
    }

    pub fn set_default_category(&self, category: &str) -> MemoryResult<()> {
        self.set(CONFIG_KEY_DEFAULT_CATEGORY, category)
    }

    pub fn get_auto_extract_frequency(&self) -> MemoryResult<AutoExtractFrequency> {
        Ok(self
            .get(CONFIG_KEY_AUTO_EXTRACT_FREQUENCY)?
            .map(|v| AutoExtractFrequency::from_str_lossy(&v))
            .unwrap_or(AutoExtractFrequency::Balanced))
    }

    pub fn set_auto_extract_frequency(&self, frequency: AutoExtractFrequency) -> MemoryResult<()> {
        self.set(CONFIG_KEY_AUTO_EXTRACT_FREQUENCY, frequency.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_key_constants() {
        assert_eq!(CONFIG_KEY_ROOT_FOLDER_ID, "memory_root_folder_id");
        assert_eq!(CONFIG_KEY_AUTO_CREATE_SUBFOLDERS, "auto_create_subfolders");
        assert_eq!(CONFIG_KEY_DEFAULT_CATEGORY, "default_category");
        assert_eq!(CONFIG_KEY_PRIVACY_MODE, "privacy_mode");
        assert_eq!(CONFIG_KEY_AUTO_EXTRACT_FREQUENCY, "auto_extract_frequency");
        assert_eq!(DEFAULT_FOLDER_TITLE, "记忆");
    }

    #[test]
    fn test_auto_extract_frequency() {
        assert_eq!(
            AutoExtractFrequency::from_str_lossy("off"),
            AutoExtractFrequency::Off
        );
        assert_eq!(
            AutoExtractFrequency::from_str_lossy("balanced"),
            AutoExtractFrequency::Balanced
        );
        assert_eq!(
            AutoExtractFrequency::from_str_lossy("aggressive"),
            AutoExtractFrequency::Aggressive
        );
        assert_eq!(
            AutoExtractFrequency::from_str_lossy("unknown"),
            AutoExtractFrequency::Balanced
        );
        assert_eq!(AutoExtractFrequency::Off.as_str(), "off");
        assert_eq!(AutoExtractFrequency::Balanced.content_min_chars(), 10);
        assert_eq!(AutoExtractFrequency::Aggressive.content_min_chars(), 4);
        assert!(AutoExtractFrequency::Aggressive.should_refresh_categories(3));
        assert!(!AutoExtractFrequency::Balanced.should_refresh_categories(7));
        assert!(AutoExtractFrequency::Balanced.should_refresh_categories(10));
    }
}
