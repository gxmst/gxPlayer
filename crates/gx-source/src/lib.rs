use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gx_contracts::NetworkRoute;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod network_policy;
pub mod safe_http;

const MAX_SCRIPT_BYTES: usize = 5 * 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 256 * 1024;
const MAX_CAPABILITIES_BYTES: usize = 256 * 1024;
const SOURCE_HEALTH_WINDOW_SIZE: usize = 16;
const SOURCE_HEALTH_MIN_SAMPLES: usize = 3;
const SOURCE_HEALTH_FAST_MS: u64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptMetadata {
    pub name: String,
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedSource {
    pub id: String,
    pub script_path: PathBuf,
    pub origin: String,
    pub imported_at_ms: u64,
    pub metadata: ScriptMetadata,
    #[serde(default = "default_true")]
    pub updates_enabled: bool,
    #[serde(default = "empty_config")]
    pub config: Value,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub health_samples: Vec<SourceHealthSample>,
    #[serde(default)]
    pub last_successful_route: Option<NetworkRoute>,
}

impl ManagedSource {
    pub fn health_summary(&self) -> SourceHealthSummary {
        summarize_source_health(&self.health_samples)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthSample {
    pub success: bool,
    pub latency_ms: u64,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthSummary {
    pub state: SourceHealthState,
    pub sample_count: usize,
    pub success_count: usize,
    pub success_rate_percent: Option<u8>,
    pub average_latency_ms: Option<u64>,
    pub last_success: Option<bool>,
    pub last_latency_ms: Option<u64>,
    pub last_recorded_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBackup {
    pub version: u32,
    pub active_source_id: Option<String>,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    /// `None` preserves automatic import-order fallback selection for older backups.
    #[serde(default)]
    pub fallback_source_ids: Option<Vec<String>>,
    pub sources: Vec<BackupSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFallbackConfig {
    pub enabled: bool,
    pub source_ids: Vec<String>,
    /// False means the order is still automatic and follows the stable import order.
    pub explicitly_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSource {
    pub origin: String,
    pub fallback_name: String,
    pub updates_enabled: bool,
    pub script: String,
    #[serde(default = "empty_config")]
    pub config: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceConfig {
    active_source_id: Option<String>,
    #[serde(default = "default_true")]
    fallback_enabled: bool,
    #[serde(default)]
    fallback_source_ids: Option<Vec<String>>,
    sources: Vec<ManagedSource>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            active_source_id: None,
            fallback_enabled: true,
            fallback_source_ids: None,
            sources: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceStoreError {
    #[error("source script is empty")]
    EmptyScript,
    #[error("source script is larger than 5 MiB")]
    ScriptTooLarge,
    #[error("source script does not appear to use the LX runtime contract")]
    InvalidScript,
    #[error("source '{0}' does not exist")]
    SourceNotFound(String),
    #[error("unsupported source backup version {0}")]
    InvalidBackupVersion(u32),
    #[error("source backup exceeds the allowed source count or total size")]
    BackupTooLarge,
    #[error("source config must be a JSON object")]
    InvalidConfig,
    #[error("source config is larger than 256 KiB")]
    ConfigTooLarge,
    #[error("source capabilities are larger than 256 KiB")]
    CapabilitiesTooLarge,
    #[error("invalid source fallback order: {0}")]
    InvalidFallbackOrder(String),
    #[error("source storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source storage JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropInImportIssue {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropInImportReport {
    pub discovered: usize,
    pub imported: Vec<ManagedSource>,
    pub already_present: Vec<ManagedSource>,
    pub failures: Vec<DropInImportIssue>,
    pub active_source_id: Option<String>,
}

pub struct SourceStore {
    root: PathBuf,
    config: SourceConfig,
}

impl SourceStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, SourceStoreError> {
        let root = root.into();
        fs::create_dir_all(root.join("scripts"))?;
        let config_path = root.join("sources.json");
        let mut config: SourceConfig = if config_path.exists() {
            serde_json::from_slice(&fs::read(config_path)?)?
        } else {
            SourceConfig::default()
        };
        for source in &mut config.sources {
            trim_health_samples(&mut source.health_samples);
        }
        Ok(Self { root, config })
    }

    pub fn list(&self) -> Vec<(ManagedSource, bool)> {
        self.config
            .sources
            .iter()
            .cloned()
            .map(|source| {
                let active = self.config.active_source_id.as_deref() == Some(source.id.as_str());
                (source, active)
            })
            .collect()
    }

    pub fn import_script(
        &mut self,
        script: &str,
        origin: impl Into<String>,
        fallback_name: &str,
    ) -> Result<ManagedSource, SourceStoreError> {
        validate_script(script)?;
        let id = script_id(script.as_bytes());
        if let Some(existing) = self.config.sources.iter().find(|source| source.id == id) {
            return Ok(existing.clone());
        }
        let metadata = parse_script_metadata(script, fallback_name);
        let script_path = self.root.join("scripts").join(format!("{id}.js"));
        fs::write(&script_path, script.as_bytes())?;
        let source = ManagedSource {
            id: id.clone(),
            script_path,
            origin: origin.into(),
            imported_at_ms: unix_time_ms(),
            metadata,
            updates_enabled: true,
            config: empty_config(),
            capabilities: Value::Null,
            health_samples: Vec::new(),
            last_successful_route: None,
        };
        self.config.sources.push(source.clone());
        if self.config.active_source_id.is_none() {
            self.config.active_source_id = Some(id);
        }
        self.persist()?;
        Ok(source)
    }

    pub fn import_file(&mut self, path: &Path) -> Result<ManagedSource, SourceStoreError> {
        let script = fs::read_to_string(path)?;
        let fallback = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("LX Source");
        self.import_script(&script, path.display().to_string(), fallback)
    }

    /// Imports every regular `.js` file from an external user-managed directory.
    ///
    /// Individual bad files are reported and skipped so one broken community source cannot
    /// prevent the application from starting. Files are processed in a stable path order. An
    /// already-valid active source is preserved; otherwise the first loadable drop-in becomes
    /// active (falling back to the lexicographically smallest managed source when necessary).
    pub fn import_drop_in_dir(
        &mut self,
        directory: &Path,
    ) -> Result<DropInImportReport, SourceStoreError> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DropInImportReport {
                    active_source_id: self.valid_active_source_id(),
                    ..DropInImportReport::default()
                });
            }
            Err(error) => return Err(error.into()),
        };

        let mut report = DropInImportReport::default();
        let mut paths = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.failures.push(DropInImportIssue {
                        path: directory.to_path_buf(),
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let is_regular_file = match entry.file_type() {
                Ok(file_type) => file_type.is_file(),
                Err(error) => {
                    report.failures.push(DropInImportIssue {
                        path,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            if is_regular_file
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
            {
                paths.push(path);
            }
        }
        paths.sort_by(|left, right| {
            left.to_string_lossy()
                .to_lowercase()
                .cmp(&right.to_string_lossy().to_lowercase())
                .then_with(|| left.cmp(right))
        });
        report.discovered = paths.len();

        let valid_active_before_import = self.valid_active_source_id();
        let mut known_ids: HashSet<String> = self
            .config
            .sources
            .iter()
            .map(|source| source.id.clone())
            .collect();
        let mut first_loadable_id = None;

        for path in paths {
            match self.import_file(&path) {
                Ok(source) => {
                    first_loadable_id.get_or_insert_with(|| source.id.clone());
                    if known_ids.insert(source.id.clone()) {
                        report.imported.push(source);
                    } else {
                        report.already_present.push(source);
                    }
                }
                Err(error) => report.failures.push(DropInImportIssue {
                    path,
                    error: error.to_string(),
                }),
            }
        }

        if valid_active_before_import.is_none() {
            let fallback = first_loadable_id.or_else(|| {
                self.config
                    .sources
                    .iter()
                    .map(|source| source.id.clone())
                    .min()
            });
            if let Some(id) = fallback
                && self.config.active_source_id.as_deref() != Some(id.as_str())
            {
                self.activate(&id)?;
            }
        }
        report.active_source_id = self.valid_active_source_id();
        Ok(report)
    }

    pub fn activate(&mut self, id: &str) -> Result<(), SourceStoreError> {
        if !self.config.sources.iter().any(|source| source.id == id) {
            return Err(SourceStoreError::SourceNotFound(id.into()));
        }
        self.config.active_source_id = Some(id.into());
        self.persist()
    }

    pub fn set_updates_enabled(&mut self, id: &str, enabled: bool) -> Result<(), SourceStoreError> {
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        source.updates_enabled = enabled;
        self.persist()
    }

    pub fn config(&self, id: &str) -> Result<Value, SourceStoreError> {
        self.config
            .sources
            .iter()
            .find(|source| source.id == id)
            .map(|source| source.config.clone())
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))
    }

    pub fn set_config(&mut self, id: &str, config: Value) -> Result<(), SourceStoreError> {
        validate_config(&config)?;
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        source.config = config;
        self.persist()
    }

    pub fn set_capabilities(
        &mut self,
        id: &str,
        capabilities: Value,
    ) -> Result<(), SourceStoreError> {
        if serde_json::to_vec(&capabilities)?.len() > MAX_CAPABILITIES_BYTES {
            return Err(SourceStoreError::CapabilitiesTooLarge);
        }
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        if source.capabilities == capabilities {
            return Ok(());
        }
        source.capabilities = capabilities;
        self.persist()
    }

    pub fn record_health_sample(
        &mut self,
        id: &str,
        success: bool,
        latency_ms: u64,
    ) -> Result<(), SourceStoreError> {
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        source.health_samples.push(SourceHealthSample {
            success,
            latency_ms,
            recorded_at_ms: unix_time_ms(),
        });
        trim_health_samples(&mut source.health_samples);
        self.persist()
    }

    pub fn preferred_route(&self, id: &str) -> Result<Option<NetworkRoute>, SourceStoreError> {
        self.config
            .sources
            .iter()
            .find(|source| source.id == id)
            .map(|source| source.last_successful_route)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))
    }

    pub fn record_successful_route(
        &mut self,
        id: &str,
        route: NetworkRoute,
    ) -> Result<(), SourceStoreError> {
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        if source.last_successful_route == Some(route) {
            return Ok(());
        }
        source.last_successful_route = Some(route);
        self.persist()
    }

    pub fn fallback_config(&self) -> SourceFallbackConfig {
        let explicitly_configured = self.config.fallback_source_ids.is_some();
        let configured = self
            .config
            .fallback_source_ids
            .as_ref()
            .cloned()
            .unwrap_or_else(|| {
                self.config
                    .sources
                    .iter()
                    .map(|source| source.id.clone())
                    .collect()
            });
        let valid_ids = self
            .config
            .sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let source_ids = configured
            .into_iter()
            .filter(|id| valid_ids.contains(id.as_str()) && seen.insert(id.clone()))
            .collect();
        SourceFallbackConfig {
            enabled: self.config.fallback_enabled,
            source_ids,
            explicitly_configured,
        }
    }

    pub fn set_fallback_config(
        &mut self,
        enabled: bool,
        source_ids: Vec<String>,
    ) -> Result<(), SourceStoreError> {
        let valid_ids = self
            .config
            .sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for id in &source_ids {
            if !valid_ids.contains(id.as_str()) {
                return Err(SourceStoreError::SourceNotFound(id.clone()));
            }
            if !seen.insert(id.clone()) {
                return Err(SourceStoreError::InvalidFallbackOrder(format!(
                    "source '{id}' appears more than once"
                )));
            }
        }
        self.config.fallback_enabled = enabled;
        self.config.fallback_source_ids = Some(source_ids);
        self.persist()
    }

    /// Returns the requested/active source first, then enabled fallbacks in stable order.
    pub fn resolution_source_ids(
        &self,
        requested_source_id: Option<&str>,
    ) -> Result<Vec<String>, SourceStoreError> {
        let primary = requested_source_id
            .map(str::to_owned)
            .or_else(|| self.valid_active_source_id());
        if let Some(id) = primary.as_deref()
            && !self.config.sources.iter().any(|source| source.id == id)
        {
            return Err(SourceStoreError::SourceNotFound(id.into()));
        }
        let mut source_ids = primary.into_iter().collect::<Vec<_>>();
        if !self.config.fallback_enabled {
            return Ok(source_ids);
        }
        for id in self.fallback_config().source_ids {
            if !source_ids.iter().any(|known| known == &id) {
                source_ids.push(id);
            }
        }
        Ok(source_ids)
    }

    pub fn active_updates_enabled(&self) -> bool {
        self.config
            .active_source_id
            .as_deref()
            .and_then(|id| self.config.sources.iter().find(|source| source.id == id))
            .is_some_and(|source| source.updates_enabled)
    }

    pub fn updates_enabled(&self, id: &str) -> Result<bool, SourceStoreError> {
        self.config
            .sources
            .iter()
            .find(|source| source.id == id)
            .map(|source| source.updates_enabled)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))
    }

    pub fn remove(&mut self, id: &str) -> Result<(), SourceStoreError> {
        let index = self
            .config
            .sources
            .iter()
            .position(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        let removed = self.config.sources.remove(index);
        if let Some(source_ids) = self.config.fallback_source_ids.as_mut() {
            source_ids.retain(|source_id| source_id != id);
        }
        if removed.script_path.starts_with(&self.root) {
            let _ = fs::remove_file(removed.script_path);
        }
        if self.config.active_source_id.as_deref() == Some(id) {
            self.config.active_source_id =
                self.config.sources.first().map(|source| source.id.clone());
        }
        self.persist()
    }

    pub fn active_script(&self) -> Result<Option<(ManagedSource, String)>, SourceStoreError> {
        let Some(id) = self.config.active_source_id.as_deref() else {
            return Ok(None);
        };
        let source = self
            .config
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?
            .clone();
        let script = fs::read_to_string(&source.script_path)?;
        Ok(Some((source, script)))
    }

    pub fn script_by_id(&self, id: &str) -> Result<(ManagedSource, String), SourceStoreError> {
        let source = self
            .config
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?
            .clone();
        let script = fs::read_to_string(&source.script_path)?;
        Ok((source, script))
    }

    pub fn export_backup(&self) -> Result<SourceBackup, SourceStoreError> {
        let sources = self
            .config
            .sources
            .iter()
            .map(|source| {
                Ok(BackupSource {
                    origin: source.origin.clone(),
                    fallback_name: source.metadata.name.clone(),
                    updates_enabled: source.updates_enabled,
                    script: fs::read_to_string(&source.script_path)?,
                    config: source.config.clone(),
                })
            })
            .collect::<Result<Vec<_>, SourceStoreError>>()?;
        Ok(SourceBackup {
            version: 1,
            active_source_id: self.config.active_source_id.clone(),
            fallback_enabled: self.config.fallback_enabled,
            fallback_source_ids: self.config.fallback_source_ids.clone(),
            sources,
        })
    }

    pub fn restore_backup(&mut self, backup: SourceBackup) -> Result<(), SourceStoreError> {
        if backup.version != 1 {
            return Err(SourceStoreError::InvalidBackupVersion(backup.version));
        }
        let total_size = backup.sources.iter().try_fold(0usize, |total, source| {
            Ok::<_, SourceStoreError>(
                total + source.script.len() + serde_json::to_vec(&source.config)?.len(),
            )
        })?;
        if backup.sources.len() > 64 || total_size > 20 * 1024 * 1024 {
            return Err(SourceStoreError::BackupTooLarge);
        }
        for source in &backup.sources {
            validate_script(&source.script)?;
            validate_config(&source.config)?;
        }
        let mut restored = Vec::with_capacity(backup.sources.len());
        for source in backup.sources {
            let id = script_id(source.script.as_bytes());
            if restored
                .iter()
                .any(|existing: &ManagedSource| existing.id == id)
            {
                continue;
            }
            let script_path = self.root.join("scripts").join(format!("{id}.js"));
            fs::write(&script_path, source.script.as_bytes())?;
            restored.push(ManagedSource {
                id,
                script_path,
                origin: source.origin,
                imported_at_ms: unix_time_ms(),
                metadata: parse_script_metadata(&source.script, &source.fallback_name),
                updates_enabled: source.updates_enabled,
                config: source.config,
                capabilities: Value::Null,
                health_samples: Vec::new(),
                last_successful_route: None,
            });
        }
        for old in &self.config.sources {
            if old.script_path.starts_with(&self.root)
                && !restored
                    .iter()
                    .any(|source| source.script_path == old.script_path)
            {
                let _ = fs::remove_file(&old.script_path);
            }
        }
        self.config.sources = restored;
        self.config.active_source_id = backup
            .active_source_id
            .filter(|id| self.config.sources.iter().any(|source| &source.id == id))
            .or_else(|| self.config.sources.first().map(|source| source.id.clone()));
        self.config.fallback_enabled = backup.fallback_enabled;
        self.config.fallback_source_ids = backup.fallback_source_ids.map(|source_ids| {
            let valid_ids = self
                .config
                .sources
                .iter()
                .map(|source| source.id.as_str())
                .collect::<HashSet<_>>();
            let mut seen = HashSet::new();
            source_ids
                .into_iter()
                .filter(|id| valid_ids.contains(id.as_str()) && seen.insert(id.clone()))
                .collect()
        });
        self.persist()
    }

    fn persist(&self) -> Result<(), SourceStoreError> {
        let bytes = serde_json::to_vec_pretty(&self.config)?;
        let temporary = self.root.join("sources.json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, self.root.join("sources.json"))?;
        Ok(())
    }

    fn valid_active_source_id(&self) -> Option<String> {
        self.config
            .active_source_id
            .clone()
            .filter(|id| self.config.sources.iter().any(|source| source.id == *id))
    }
}

fn trim_health_samples(samples: &mut Vec<SourceHealthSample>) {
    let excess = samples.len().saturating_sub(SOURCE_HEALTH_WINDOW_SIZE);
    if excess > 0 {
        samples.drain(..excess);
    }
}

fn summarize_source_health(samples: &[SourceHealthSample]) -> SourceHealthSummary {
    let sample_count = samples.len();
    let success_count = samples.iter().filter(|sample| sample.success).count();
    let success_rate_percent =
        (sample_count > 0).then(|| ((success_count * 100) / sample_count).min(100) as u8);
    let average_latency_ms = (sample_count > 0).then(|| {
        let total = samples
            .iter()
            .map(|sample| u128::from(sample.latency_ms))
            .sum::<u128>();
        u64::try_from(total / sample_count as u128).unwrap_or(u64::MAX)
    });
    let last = samples.last();
    let recent_failures = sample_count >= SOURCE_HEALTH_MIN_SAMPLES
        && samples[sample_count - SOURCE_HEALTH_MIN_SAMPLES..]
            .iter()
            .all(|sample| !sample.success);
    let state = if sample_count < SOURCE_HEALTH_MIN_SAMPLES {
        SourceHealthState::Unknown
    } else if recent_failures || success_count * 100 < sample_count * 40 {
        SourceHealthState::Unhealthy
    } else if success_count * 100 >= sample_count * 80
        && average_latency_ms.is_some_and(|latency| latency <= SOURCE_HEALTH_FAST_MS)
    {
        SourceHealthState::Healthy
    } else {
        SourceHealthState::Degraded
    };
    SourceHealthSummary {
        state,
        sample_count,
        success_count,
        success_rate_percent,
        average_latency_ms,
        last_success: last.map(|sample| sample.success),
        last_latency_ms: last.map(|sample| sample.latency_ms),
        last_recorded_at_ms: last.map(|sample| sample.recorded_at_ms),
    }
}

fn default_true() -> bool {
    true
}

fn empty_config() -> Value {
    Value::Object(Default::default())
}

fn validate_config(config: &Value) -> Result<(), SourceStoreError> {
    if !config.is_object() {
        return Err(SourceStoreError::InvalidConfig);
    }
    if let Some(ls_config) = config.get("lsConfig")
        && !ls_config.is_object()
    {
        return Err(SourceStoreError::InvalidConfig);
    }
    if let Some(overrides) = config.get("keyOverrides") {
        let Some(overrides) = overrides.as_array() else {
            return Err(SourceStoreError::InvalidConfig);
        };
        for item in overrides {
            let Some(item) = item.as_object() else {
                return Err(SourceStoreError::InvalidConfig);
            };
            let Some(const_name) = item.get("constName").and_then(Value::as_str) else {
                return Err(SourceStoreError::InvalidConfig);
            };
            if !is_safe_const_name(const_name)
                || item.get("value").and_then(Value::as_str).is_none()
            {
                return Err(SourceStoreError::InvalidConfig);
            }
        }
    }
    let size = serde_json::to_vec(config)?.len();
    if size > MAX_CONFIG_BYTES {
        return Err(SourceStoreError::ConfigTooLarge);
    }
    Ok(())
}

fn is_safe_const_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

pub fn parse_script_metadata(script: &str, fallback_name: &str) -> ScriptMetadata {
    let mut metadata = ScriptMetadata {
        name: fallback_name.chars().take(120).collect(),
        description: String::new(),
        author: String::new(),
        homepage: String::new(),
        version: String::new(),
    };
    for line in script.lines().take(80) {
        let line = line.trim().trim_start_matches('*').trim();
        let Some(rest) = line.strip_prefix('@') else {
            continue;
        };
        let Some(split) = rest.find(char::is_whitespace) else {
            continue;
        };
        let key = &rest[..split];
        let value: String = rest[split..].trim().chars().take(500).collect();
        match key {
            "name" if !value.is_empty() => metadata.name = value,
            "description" => metadata.description = value,
            "author" => metadata.author = value,
            "homepage" => metadata.homepage = value,
            "version" => metadata.version = value,
            _ => {}
        }
    }
    metadata
}

fn validate_script(script: &str) -> Result<(), SourceStoreError> {
    if script.trim().is_empty() {
        return Err(SourceStoreError::EmptyScript);
    }
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(SourceStoreError::ScriptTooLarge);
    }
    if !(script.contains("lx.on")
        || script.contains("globalThis.lx")
        || script.contains("globalThis['lx']")
        || script.contains("globalThis[\"lx\"]")
        || script.contains("window.lx")
        || script.contains("window['lx']")
        || script.contains("window[\"lx\"]"))
    {
        return Err(SourceStoreError::InvalidScript);
    }
    Ok(())
}

fn script_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_deduplicates_activates_and_removes_scripts() {
        let root = temporary_root();
        let script_a = "/*!\n * @name Source A\n * @version 1.2.3\n * @author Tester\n */\nlx.on('request', () => 'https://example.com/a.mp3')";
        let script_b =
            "/*! @name Source B */\nglobalThis.lx.on('request', () => 'https://example.com/b.mp3')";
        let mut store = SourceStore::open(&root).unwrap();
        let first = store.import_script(script_a, "test:a", "a.js").unwrap();
        let duplicate = store.import_script(script_a, "test:a", "a.js").unwrap();
        assert_eq!(first.id, duplicate.id);
        assert_eq!(store.list().len(), 1);
        assert_eq!(first.metadata.name, "Source A");
        let second = store.import_script(script_b, "test:b", "b.js").unwrap();
        store.activate(&second.id).unwrap();
        store.set_updates_enabled(&second.id, false).unwrap();
        store
            .set_config(
                &second.id,
                serde_json::json!({ "api": { "addr": "https://example.com", "pass": "secret" } }),
            )
            .unwrap();
        assert!(!store.active_updates_enabled());
        let backup = store.export_backup().unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, second.id);
        store.remove(&second.id).unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, first.id);
        store.restore_backup(backup).unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, second.id);
        assert!(!store.active_updates_enabled());
        assert_eq!(store.config(&second.id).unwrap()["api"]["pass"], "secret");
        drop(store);
        assert_eq!(SourceStore::open(&root).unwrap().list().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_lx_and_oversized_scripts() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        assert!(matches!(
            store.import_script("console.log('no')", "test", "bad.js"),
            Err(SourceStoreError::InvalidScript)
        ));
        assert!(
            store
                .import_script(
                    "const { on, send } = globalThis['lx']; on('request', () => 1)",
                    "test:bracket-runtime",
                    "bracket.js"
                )
                .is_ok()
        );
        let source = store
            .import_script("lx.on('request', () => 1)", "test", "valid.js")
            .unwrap();
        assert!(matches!(
            store.set_config(&source.id, Value::String("secret".into())),
            Err(SourceStoreError::InvalidConfig)
        ));
        assert!(matches!(
            store.set_config(
                &source.id,
                serde_json::json!({
                    "lsConfig": {},
                    "keyOverrides": [{"constName":"bad-name","value":"secret"}]
                })
            ),
            Err(SourceStoreError::InvalidConfig)
        ));
        let oversized = format!(
            "globalThis.lx.on('request',()=>0);{}",
            " ".repeat(MAX_SCRIPT_BYTES)
        );
        assert!(matches!(
            store.import_script(&oversized, "test", "large.js"),
            Err(SourceStoreError::ScriptTooLarge)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn health_summary_uses_a_bounded_window_and_clear_states() {
        let mut samples = (0..18)
            .map(|index| SourceHealthSample {
                success: true,
                latency_ms: 1_000 + index,
                recorded_at_ms: index,
            })
            .collect::<Vec<_>>();
        trim_health_samples(&mut samples);
        assert_eq!(samples.len(), SOURCE_HEALTH_WINDOW_SIZE);
        let healthy = summarize_source_health(&samples);
        assert_eq!(healthy.state, SourceHealthState::Healthy);
        assert_eq!(healthy.sample_count, SOURCE_HEALTH_WINDOW_SIZE);
        assert_eq!(healthy.success_rate_percent, Some(100));
        assert_eq!(healthy.average_latency_ms, Some(1_009));

        samples.extend((0..3).map(|index| SourceHealthSample {
            success: false,
            latency_ms: 8_000,
            recorded_at_ms: 20 + index,
        }));
        trim_health_samples(&mut samples);
        let unhealthy = summarize_source_health(&samples);
        assert_eq!(unhealthy.state, SourceHealthState::Unhealthy);
        assert_eq!(unhealthy.success_count, SOURCE_HEALTH_WINDOW_SIZE - 3);
        assert_eq!(unhealthy.last_success, Some(false));

        let degraded = summarize_source_health(&[
            SourceHealthSample {
                success: true,
                latency_ms: 1_000,
                recorded_at_ms: 1,
            },
            SourceHealthSample {
                success: true,
                latency_ms: 1_000,
                recorded_at_ms: 2,
            },
            SourceHealthSample {
                success: false,
                latency_ms: 8_000,
                recorded_at_ms: 3,
            },
        ]);
        assert_eq!(degraded.state, SourceHealthState::Degraded);

        let unknown = summarize_source_health(&[
            SourceHealthSample {
                success: true,
                latency_ms: 1_000,
                recorded_at_ms: 1,
            },
            SourceHealthSample {
                success: true,
                latency_ms: 1_000,
                recorded_at_ms: 2,
            },
        ]);
        assert_eq!(unknown.state, SourceHealthState::Unknown);
    }

    #[test]
    fn health_samples_persist_locally_but_are_not_in_backups() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let source = store
            .import_script("lx.on('request', () => 1)", "test", "source.js")
            .unwrap();
        store.record_health_sample(&source.id, true, 1_200).unwrap();
        store.record_health_sample(&source.id, true, 1_300).unwrap();
        assert_eq!(store.list()[0].0.health_summary().sample_count, 2);

        drop(store);
        let mut reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.list()[0].0.health_summary().average_latency_ms,
            Some(1_250)
        );
        let backup = reopened.export_backup().unwrap();
        reopened.restore_backup(backup).unwrap();
        assert_eq!(
            reopened.list()[0].0.health_summary().state,
            SourceHealthState::Unknown
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_source_files_without_health_samples_still_open() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        store
            .import_script("lx.on('request', () => 1)", "test", "source.js")
            .unwrap();
        drop(store);

        let config_path = root.join("sources.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["sources"][0]
            .as_object_mut()
            .unwrap()
            .remove("healthSamples");
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.list()[0].0.health_summary().state,
            SourceHealthState::Unknown
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn route_preferences_persist_locally_but_are_not_in_backups() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let source = store
            .import_script("lx.on('request', () => 1)", "test", "source.js")
            .unwrap();
        assert_eq!(store.preferred_route(&source.id).unwrap(), None);
        store
            .record_successful_route(&source.id, NetworkRoute::SystemProxy)
            .unwrap();
        store
            .record_successful_route(&source.id, NetworkRoute::SystemProxy)
            .unwrap();

        drop(store);
        let mut reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.preferred_route(&source.id).unwrap(),
            Some(NetworkRoute::SystemProxy)
        );
        let backup = reopened.export_backup().unwrap();
        let serialized = serde_json::to_string(&backup).unwrap();
        assert!(!serialized.contains("lastSuccessfulRoute"));
        reopened.restore_backup(backup).unwrap();
        assert_eq!(reopened.preferred_route(&source.id).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_source_files_without_route_preferences_still_open() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let source = store
            .import_script("lx.on('request', () => 1)", "test", "source.js")
            .unwrap();
        drop(store);

        let config_path = root.join("sources.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["sources"][0]
            .as_object_mut()
            .unwrap()
            .remove("lastSuccessfulRoute");
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        let reopened = SourceStore::open(&root).unwrap();
        assert_eq!(reopened.preferred_route(&source.id).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imports_external_drop_ins_in_stable_order_and_skips_bad_files() {
        let root = temporary_root();
        let drop_in = root.join("external-drop-in");
        fs::create_dir_all(drop_in.join("nested.js")).unwrap();
        fs::write(
            drop_in.join("b.js"),
            "/*!\n * @name Source B\n */\nglobalThis.lx.on('request', () => 2)",
        )
        .unwrap();
        fs::write(
            drop_in.join("A.JS"),
            "/*!\n * @name Source A\n */\nlx.on('request', () => 1)",
        )
        .unwrap();
        fs::write(drop_in.join("bad.js"), "console.log('not lx')").unwrap();
        fs::write(drop_in.join("ignored.txt"), "lx.on('request', () => 3)").unwrap();

        let mut store = SourceStore::open(root.join("managed")).unwrap();
        store.config.active_source_id = Some("stale-source-id".into());
        store.persist().unwrap();
        let report = store.import_drop_in_dir(&drop_in).unwrap();

        assert_eq!(report.discovered, 3);
        assert_eq!(report.imported.len(), 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].path, drop_in.join("bad.js"));
        assert_eq!(store.list().len(), 2);
        let active = store.active_script().unwrap().unwrap().0;
        assert_eq!(active.metadata.name, "Source A");
        assert_eq!(report.active_source_id.as_deref(), Some(active.id.as_str()));

        let second = store.import_drop_in_dir(&drop_in).unwrap();
        assert!(second.imported.is_empty());
        assert_eq!(second.already_present.len(), 2);
        assert_eq!(second.failures.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drop_in_import_preserves_a_valid_active_source() {
        let root = temporary_root();
        let drop_in = root.join("drop-in");
        fs::create_dir_all(&drop_in).unwrap();
        fs::write(
            drop_in.join("a.js"),
            "/*! @name Drop In */\nlx.on('request', () => 1)",
        )
        .unwrap();
        let mut store = SourceStore::open(root.join("managed")).unwrap();
        let existing = store
            .import_script(
                "/*! @name Existing */\nwindow.lx.on('request', () => 0)",
                "test:existing",
                "existing.js",
            )
            .unwrap();

        let report = store.import_drop_in_dir(&drop_in).unwrap();

        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            report.active_source_id.as_deref(),
            Some(existing.id.as_str())
        );
        assert_eq!(store.active_script().unwrap().unwrap().0.id, existing.id);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_drop_in_directory_is_a_noop() {
        let root = temporary_root();
        let mut store = SourceStore::open(root.join("managed")).unwrap();
        let report = store
            .import_drop_in_dir(&root.join("does-not-exist"))
            .unwrap();
        assert_eq!(report, DropInImportReport::default());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fallback_chain_is_stable_configurable_and_survives_reopen() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let first = store
            .import_script("lx.on('request', () => 1)", "test:first", "First")
            .unwrap();
        let second = store
            .import_script("lx.on('request', () => 2)", "test:second", "Second")
            .unwrap();
        let third = store
            .import_script("lx.on('request', () => 3)", "test:third", "Third")
            .unwrap();

        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [first.id.clone(), second.id.clone(), third.id.clone()]
        );
        assert!(!store.fallback_config().explicitly_configured);

        store.activate(&second.id).unwrap();
        store
            .set_fallback_config(true, vec![third.id.clone(), first.id.clone()])
            .unwrap();
        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [second.id.clone(), third.id.clone(), first.id.clone()]
        );
        assert!(store.fallback_config().explicitly_configured);

        let fourth = store
            .import_script("lx.on('request', () => 4)", "test:fourth", "Fourth")
            .unwrap();
        assert!(
            !store
                .fallback_config()
                .source_ids
                .iter()
                .any(|id| id == &fourth.id)
        );

        drop(store);
        let mut reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.resolution_source_ids(None).unwrap(),
            [second.id.clone(), third.id.clone(), first.id.clone()]
        );
        reopened.set_fallback_config(false, vec![]).unwrap();
        assert_eq!(
            reopened.resolution_source_ids(Some(&third.id)).unwrap(),
            [third.id]
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn temporary_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gx-source-test-{}-{}", std::process::id(), nanos))
    }
}
