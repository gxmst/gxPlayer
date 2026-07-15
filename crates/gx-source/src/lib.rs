use std::collections::{HashMap, HashSet};
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
    pub enabled: bool,
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
    /// `None` keeps version-1 backups created before user ordering was introduced compatible.
    #[serde(default)]
    pub source_order: Option<Vec<String>>,
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
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    #[serde(default)]
    source_order: Vec<String>,
    sources: Vec<ManagedSource>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            active_source_id: None,
            fallback_enabled: true,
            fallback_source_ids: None,
            source_order: Vec::new(),
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
    #[error("invalid source order: {0}")]
    InvalidSourceOrder(String),
    #[error("source '{0}' is disabled")]
    SourceDisabled(String),
    #[error("reimported script conflicts with existing source '{0}'")]
    SourceIdConflict(String),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceListEntry {
    pub source: ManagedSource,
    pub preferred: bool,
    pub user_priority: usize,
    pub effective_priority: Option<usize>,
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
        let (mut config, had_source_order): (SourceConfig, bool) = if config_path.exists() {
            let bytes = fs::read(&config_path)?;
            let raw: Value = serde_json::from_slice(&bytes)?;
            let had_source_order = raw.get("sourceOrder").is_some();
            (serde_json::from_value(raw)?, had_source_order)
        } else {
            (SourceConfig::default(), true)
        };
        for source in &mut config.sources {
            trim_health_samples(&mut source.health_samples);
        }
        let mut changed = if had_source_order {
            normalize_source_order(&mut config)
        } else {
            migrate_legacy_source_preferences(&mut config)
        };
        let mut store = Self { root, config };
        let effective_preferred = store.effective_source_ids().into_iter().next();
        if store.config.active_source_id != effective_preferred {
            store.config.active_source_id = effective_preferred;
            changed = true;
        }
        if changed {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn list(&self) -> Vec<SourceListEntry> {
        let effective_ids = self.effective_source_ids();
        let effective_priorities = effective_ids
            .iter()
            .enumerate()
            .map(|(priority, id)| (id.as_str(), priority))
            .collect::<HashMap<_, _>>();
        self.ordered_sources()
            .into_iter()
            .enumerate()
            .map(|(user_priority, source)| {
                let effective_priority = effective_priorities.get(source.id.as_str()).copied();
                SourceListEntry {
                    source: source.clone(),
                    preferred: effective_priority == Some(0),
                    user_priority,
                    effective_priority,
                }
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
            enabled: true,
            updates_enabled: true,
            config: empty_config(),
            capabilities: Value::Null,
            health_samples: Vec::new(),
            last_successful_route: None,
        };
        self.config.sources.push(source.clone());
        self.config.source_order.push(id);
        self.sync_legacy_active_source_id();
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
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        source.enabled = true;
        self.config.source_order.retain(|source_id| source_id != id);
        self.config.source_order.insert(0, id.to_owned());
        self.sync_legacy_active_source_id();
        self.persist()
    }

    pub fn set_order(&mut self, source_ids: Vec<String>) -> Result<(), SourceStoreError> {
        validate_full_source_order(&self.config.sources, &source_ids)?;
        if self.config.source_order == source_ids {
            return Ok(());
        }
        self.config.source_order = source_ids;
        self.sync_legacy_active_source_id();
        self.persist()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), SourceStoreError> {
        let source = self
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        if source.enabled == enabled {
            return Ok(());
        }
        source.enabled = enabled;
        self.sync_legacy_active_source_id();
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
        {
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
        }
        self.sync_legacy_active_source_id();
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
        let source_ids = self
            .ordered_sources()
            .into_iter()
            .filter(|source| source.enabled)
            .map(|source| source.id.clone())
            .collect::<Vec<_>>();
        SourceFallbackConfig {
            enabled: source_ids.len() > 1,
            source_ids,
            explicitly_configured: true,
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
        let preferred = self.effective_source_ids().into_iter().next();
        let mut enabled_ids = HashSet::new();
        if let Some(preferred) = preferred.as_ref() {
            enabled_ids.insert(preferred.clone());
        }
        if enabled {
            enabled_ids.extend(source_ids.iter().cloned());
        }
        for source in &mut self.config.sources {
            source.enabled = enabled_ids.contains(&source.id);
        }
        let mut order = Vec::with_capacity(self.config.sources.len());
        if let Some(preferred) = preferred {
            order.push(preferred);
        }
        for id in &source_ids {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        for id in &self.config.source_order {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        self.config.source_order = order;
        self.config.fallback_enabled = enabled;
        self.config.fallback_source_ids = Some(source_ids);
        self.sync_legacy_active_source_id();
        self.persist()
    }

    /// Returns enabled sources using health buckets and user order, with an explicit request first.
    pub fn resolution_source_ids(
        &self,
        requested_source_id: Option<&str>,
    ) -> Result<Vec<String>, SourceStoreError> {
        let requested = requested_source_id
            .map(|id| {
                let source = self
                    .config
                    .sources
                    .iter()
                    .find(|source| source.id == id)
                    .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
                if !source.enabled {
                    return Err(SourceStoreError::SourceDisabled(id.into()));
                }
                Ok(id.to_owned())
            })
            .transpose()?;
        Ok(build_resolution_order(
            &self.ordered_sources(),
            requested.as_deref(),
        ))
    }

    pub fn active_updates_enabled(&self) -> bool {
        self.effective_source_ids()
            .first()
            .map(String::as_str)
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
        self.config.source_order.retain(|source_id| source_id != id);
        if let Some(source_ids) = self.config.fallback_source_ids.as_mut() {
            source_ids.retain(|source_id| source_id != id);
        }
        if removed.script_path.starts_with(&self.root) {
            let _ = fs::remove_file(removed.script_path);
        }
        self.sync_legacy_active_source_id();
        self.persist()
    }

    pub fn active_script(&self) -> Result<Option<(ManagedSource, String)>, SourceStoreError> {
        let Some(id) = self.effective_source_ids().into_iter().next() else {
            return Ok(None);
        };
        let source = self
            .config
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.clone()))?
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
        if !source.enabled {
            return Err(SourceStoreError::SourceDisabled(id.into()));
        }
        let script = fs::read_to_string(&source.script_path)?;
        Ok((source, script))
    }

    pub fn source(&self, id: &str) -> Result<ManagedSource, SourceStoreError> {
        self.config
            .sources
            .iter()
            .find(|source| source.id == id)
            .cloned()
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))
    }

    pub fn reimport_script(
        &mut self,
        id: &str,
        script: &str,
        fallback_name: &str,
    ) -> Result<ManagedSource, SourceStoreError> {
        validate_script(script)?;
        let index = self
            .config
            .sources
            .iter()
            .position(|source| source.id == id)
            .ok_or_else(|| SourceStoreError::SourceNotFound(id.into()))?;
        let old = self.config.sources[index].clone();
        let new_id = script_id(script.as_bytes());
        if new_id != id && self.config.sources.iter().any(|source| source.id == new_id) {
            return Err(SourceStoreError::SourceIdConflict(new_id));
        }

        if new_id == id {
            self.config.sources[index].metadata = parse_script_metadata(script, fallback_name);
            self.persist()?;
            return Ok(self.config.sources[index].clone());
        }

        let new_script_path = self.root.join("scripts").join(format!("{new_id}.js"));
        let temporary_script_path = self.root.join("scripts").join(format!("{new_id}.js.tmp"));
        fs::write(&temporary_script_path, script.as_bytes())?;
        if let Err(error) = fs::rename(&temporary_script_path, &new_script_path) {
            let _ = fs::remove_file(&temporary_script_path);
            return Err(error.into());
        }

        let replacement = ManagedSource {
            id: new_id.clone(),
            script_path: new_script_path.clone(),
            origin: old.origin.clone(),
            imported_at_ms: old.imported_at_ms,
            metadata: parse_script_metadata(script, fallback_name),
            enabled: old.enabled,
            updates_enabled: old.updates_enabled,
            config: old.config.clone(),
            capabilities: Value::Null,
            health_samples: Vec::new(),
            last_successful_route: old.last_successful_route,
        };
        self.config.sources[index] = replacement.clone();
        replace_source_id(&mut self.config.source_order, id, &new_id);
        if self.config.active_source_id.as_deref() == Some(id) {
            self.config.active_source_id = Some(new_id.clone());
        }
        if let Some(source_ids) = self.config.fallback_source_ids.as_mut() {
            replace_source_id(source_ids, id, &new_id);
        }
        self.sync_legacy_active_source_id();
        if let Err(error) = self.persist() {
            self.config.sources[index] = old;
            replace_source_id(&mut self.config.source_order, &new_id, id);
            if self.config.active_source_id.as_deref() == Some(new_id.as_str()) {
                self.config.active_source_id = Some(id.to_owned());
            }
            if let Some(source_ids) = self.config.fallback_source_ids.as_mut() {
                replace_source_id(source_ids, &new_id, id);
            }
            let _ = fs::remove_file(new_script_path);
            return Err(error);
        }
        if old.script_path.starts_with(&self.root) && old.script_path != replacement.script_path {
            let _ = fs::remove_file(old.script_path);
        }
        Ok(replacement)
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
                    enabled: source.enabled,
                    updates_enabled: source.updates_enabled,
                    script: fs::read_to_string(&source.script_path)?,
                    config: source.config.clone(),
                })
            })
            .collect::<Result<Vec<_>, SourceStoreError>>()?;
        Ok(SourceBackup {
            version: 1,
            active_source_id: self.effective_source_ids().into_iter().next(),
            fallback_enabled: self.fallback_config().enabled,
            fallback_source_ids: Some(self.fallback_config().source_ids),
            source_order: Some(self.config.source_order.clone()),
            sources,
        })
    }

    /// Validate every size, script and configuration constraint used by restoration
    /// without writing scripts or replacing the current source configuration.
    pub fn validate_backup(backup: &SourceBackup) -> Result<(), SourceStoreError> {
        if backup.version != 1 {
            return Err(SourceStoreError::InvalidBackupVersion(backup.version));
        }
        let total_size = backup.sources.iter().try_fold(0usize, |total, source| {
            let config_size = serde_json::to_vec(&source.config)?.len();
            total
                .checked_add(source.script.len())
                .and_then(|total| total.checked_add(config_size))
                .ok_or(SourceStoreError::BackupTooLarge)
        })?;
        if backup.sources.len() > 64 || total_size > 20 * 1024 * 1024 {
            return Err(SourceStoreError::BackupTooLarge);
        }
        for source in &backup.sources {
            validate_script(&source.script)?;
            validate_config(&source.config)?;
        }
        Ok(())
    }

    pub fn restore_backup(&mut self, backup: SourceBackup) -> Result<(), SourceStoreError> {
        Self::validate_backup(&backup)?;
        let legacy_active_source_id = backup.active_source_id.clone();
        let legacy_fallback_enabled = backup.fallback_enabled;
        let legacy_fallback_source_ids = backup.fallback_source_ids.clone();
        let backup_source_order = backup.source_order.clone();
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
                enabled: source.enabled,
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
        self.config.active_source_id = legacy_active_source_id
            .filter(|id| self.config.sources.iter().any(|source| &source.id == id))
            .or_else(|| self.config.sources.first().map(|source| source.id.clone()));
        self.config.fallback_enabled = legacy_fallback_enabled;
        self.config.fallback_source_ids = legacy_fallback_source_ids.map(|source_ids| {
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
        if let Some(source_order) = backup_source_order {
            self.config.source_order = source_order;
            normalize_source_order(&mut self.config);
        } else {
            migrate_legacy_source_preferences(&mut self.config);
        }
        self.sync_legacy_active_source_id();
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
        self.effective_source_ids().into_iter().next()
    }

    fn ordered_sources(&self) -> Vec<&ManagedSource> {
        let sources = self
            .config
            .sources
            .iter()
            .map(|source| (source.id.as_str(), source))
            .collect::<HashMap<_, _>>();
        self.config
            .source_order
            .iter()
            .filter_map(|id| sources.get(id.as_str()).copied())
            .collect()
    }

    fn effective_source_ids(&self) -> Vec<String> {
        build_resolution_order(&self.ordered_sources(), None)
    }

    fn sync_legacy_active_source_id(&mut self) {
        self.config.active_source_id = self.effective_source_ids().into_iter().next();
    }
}

fn normalize_source_order(config: &mut SourceConfig) -> bool {
    let valid_ids = config
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut normalized = config
        .source_order
        .iter()
        .filter(|id| valid_ids.contains(id.as_str()) && seen.insert((*id).clone()))
        .cloned()
        .collect::<Vec<_>>();
    normalized.extend(
        config
            .sources
            .iter()
            .filter(|source| seen.insert(source.id.clone()))
            .map(|source| source.id.clone()),
    );
    if config.source_order == normalized {
        false
    } else {
        config.source_order = normalized;
        true
    }
}

fn migrate_legacy_source_preferences(config: &mut SourceConfig) -> bool {
    let valid_ids = config
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect::<HashSet<_>>();
    let active = config
        .active_source_id
        .clone()
        .filter(|id| valid_ids.contains(id.as_str()));
    let mut order = Vec::with_capacity(config.sources.len());
    if let Some(active) = active.as_ref() {
        order.push(active.clone());
    }
    if let Some(fallback_ids) = config.fallback_source_ids.as_ref() {
        for id in fallback_ids {
            if valid_ids.contains(id.as_str()) && !order.contains(id) {
                order.push(id.clone());
            }
        }
    }
    for source in &config.sources {
        if !order.contains(&source.id) {
            order.push(source.id.clone());
        }
    }
    config.source_order = order;

    match (config.fallback_enabled, config.fallback_source_ids.as_ref()) {
        (false, _) => {
            for source in &mut config.sources {
                source.enabled = active.as_deref() == Some(source.id.as_str());
            }
        }
        (true, Some(fallback_ids)) => {
            for source in &mut config.sources {
                source.enabled = active.as_deref() == Some(source.id.as_str())
                    || fallback_ids.iter().any(|id| id == &source.id);
            }
        }
        (true, None) => {
            for source in &mut config.sources {
                source.enabled = true;
            }
        }
    }
    true
}

fn validate_full_source_order(
    sources: &[ManagedSource],
    source_ids: &[String],
) -> Result<(), SourceStoreError> {
    if source_ids.len() != sources.len() {
        return Err(SourceStoreError::InvalidSourceOrder(format!(
            "expected {} source ids, received {}",
            sources.len(),
            source_ids.len()
        )));
    }
    let valid_ids = sources
        .iter()
        .map(|source| source.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for id in source_ids {
        if !valid_ids.contains(id.as_str()) {
            return Err(SourceStoreError::SourceNotFound(id.clone()));
        }
        if !seen.insert(id.as_str()) {
            return Err(SourceStoreError::InvalidSourceOrder(format!(
                "source '{id}' appears more than once"
            )));
        }
    }
    Ok(())
}

fn build_resolution_order(
    sources: &[&ManagedSource],
    requested_source_id: Option<&str>,
) -> Vec<String> {
    let mut ordered = sources
        .iter()
        .copied()
        .filter(|source| source.enabled)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|source| health_bucket(source.health_summary().state));

    let mut result = Vec::with_capacity(ordered.len());
    if let Some(requested) = requested_source_id {
        result.push(requested.to_owned());
    }
    for source in ordered {
        if !result.iter().any(|id| id == &source.id) {
            result.push(source.id.clone());
        }
    }
    result
}

fn health_bucket(state: SourceHealthState) -> u8 {
    match state {
        SourceHealthState::Healthy => 0,
        SourceHealthState::Unknown => 1,
        SourceHealthState::Degraded => 2,
        SourceHealthState::Unhealthy => 3,
    }
}

fn replace_source_id(source_ids: &mut [String], old_id: &str, new_id: &str) {
    for id in source_ids {
        if id == old_id {
            *id = new_id.to_owned();
        }
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
    let last_success = last.map(|sample| sample.success);
    let state = if sample_count < SOURCE_HEALTH_MIN_SAMPLES {
        SourceHealthState::Unknown
    } else if recent_failures
        || (success_count * 100 < sample_count * 40 && last_success != Some(true))
    {
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
        last_success,
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
        assert_eq!(store.list()[0].source.health_summary().sample_count, 2);

        drop(store);
        let mut reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.list()[0]
                .source
                .health_summary()
                .average_latency_ms,
            Some(1_250)
        );
        let backup = reopened.export_backup().unwrap();
        reopened.restore_backup(backup).unwrap();
        assert_eq!(
            reopened.list()[0].source.health_summary().state,
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
            reopened.list()[0].source.health_summary().state,
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
    fn legacy_fallback_commands_adapt_to_order_and_enabled_sources() {
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
            store
                .fallback_config()
                .source_ids
                .iter()
                .any(|id| id == &fourth.id)
        );

        drop(store);
        let mut reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened.resolution_source_ids(None).unwrap(),
            [
                second.id.clone(),
                third.id.clone(),
                first.id.clone(),
                fourth.id.clone()
            ]
        );
        reopened.set_fallback_config(false, vec![]).unwrap();
        assert!(matches!(
            reopened.resolution_source_ids(Some(&third.id)),
            Err(SourceStoreError::SourceDisabled(_))
        ));
        assert_eq!(reopened.resolution_source_ids(None).unwrap(), [second.id]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrates_legacy_active_and_fallback_preferences_once() {
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
        let fourth = store
            .import_script("lx.on('request', () => 4)", "test:fourth", "Fourth")
            .unwrap();
        drop(store);

        let config_path = root.join("sources.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config.as_object_mut().unwrap().remove("sourceOrder");
        config["activeSourceId"] = Value::String(second.id.clone());
        config["fallbackEnabled"] = Value::Bool(true);
        config["fallbackSourceIds"] = serde_json::json!([third.id, first.id, third.id]);
        for source in config["sources"].as_array_mut().unwrap() {
            source.as_object_mut().unwrap().remove("enabled");
        }
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        let migrated = SourceStore::open(&root).unwrap();
        let listed = migrated.list();
        assert_eq!(
            listed
                .iter()
                .map(|entry| entry.source.id.as_str())
                .collect::<Vec<_>>(),
            [
                second.id.as_str(),
                third.id.as_str(),
                first.id.as_str(),
                fourth.id.as_str()
            ]
        );
        assert!(listed[0].source.enabled);
        assert!(listed[1].source.enabled);
        assert!(listed[2].source.enabled);
        assert!(!listed[3].source.enabled);
        let persisted: Value =
            serde_json::from_slice(&fs::read(root.join("sources.json")).unwrap()).unwrap();
        assert!(persisted.get("sourceOrder").is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_fallback_off_enables_only_active_and_none_enables_all() {
        for (fallback_enabled, fallback_ids, expected_enabled) in [
            (false, Some(Vec::<String>::new()), vec![false, true, false]),
            (true, None, vec![true, true, true]),
        ] {
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
            drop(store);
            let config_path = root.join("sources.json");
            let mut config: Value =
                serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
            config.as_object_mut().unwrap().remove("sourceOrder");
            config["activeSourceId"] = Value::String(second.id.clone());
            config["fallbackEnabled"] = Value::Bool(fallback_enabled);
            match fallback_ids {
                Some(ids) => config["fallbackSourceIds"] = serde_json::json!(ids),
                None => {
                    config.as_object_mut().unwrap().remove("fallbackSourceIds");
                }
            }
            for source in config["sources"].as_array_mut().unwrap() {
                source.as_object_mut().unwrap().remove("enabled");
            }
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let migrated = SourceStore::open(&root).unwrap();
            let enabled_by_import = [first.id, second.id, third.id]
                .iter()
                .map(|id| {
                    migrated
                        .list()
                        .into_iter()
                        .find(|entry| &entry.source.id == id)
                        .unwrap()
                        .source
                        .enabled
                })
                .collect::<Vec<_>>();
            assert_eq!(enabled_by_import, expected_enabled);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn effective_order_uses_health_then_user_order_and_rejects_disabled_requests() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let unhealthy = store
            .import_script("lx.on('request', () => 1)", "test:red", "Red")
            .unwrap();
        let unknown = store
            .import_script("lx.on('request', () => 2)", "test:gray", "Gray")
            .unwrap();
        let degraded = store
            .import_script("lx.on('request', () => 3)", "test:yellow", "Yellow")
            .unwrap();
        let healthy = store
            .import_script("lx.on('request', () => 4)", "test:green", "Green")
            .unwrap();
        set_test_health(
            &mut store,
            &unhealthy.id,
            &[(false, 1), (false, 2), (false, 3)],
        );
        set_test_health(
            &mut store,
            &degraded.id,
            &[(true, 1), (true, 2), (false, 3)],
        );
        set_test_health(&mut store, &healthy.id, &[(true, 1), (true, 2), (true, 3)]);

        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [
                healthy.id.clone(),
                unknown.id.clone(),
                degraded.id.clone(),
                unhealthy.id.clone()
            ]
        );
        assert_eq!(
            store.resolution_source_ids(Some(&unhealthy.id)).unwrap()[0],
            unhealthy.id
        );
        store.set_enabled(&unknown.id, false).unwrap();
        assert!(matches!(
            store.resolution_source_ids(Some(&unknown.id)),
            Err(SourceStoreError::SourceDisabled(_))
        ));
        let listed = store.list();
        assert_eq!(listed[0].user_priority, 0);
        assert_eq!(listed[0].effective_priority, Some(2));
        assert!(listed.iter().any(|entry| {
            entry.source.id == healthy.id && entry.preferred && entry.effective_priority == Some(0)
        }));
        assert!(
            listed.iter().any(|entry| {
                entry.source.id == unknown.id && entry.effective_priority.is_none()
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn order_must_be_complete_unique_and_survives_reopen() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let first = store
            .import_script("lx.on('request', () => 1)", "test:first", "First")
            .unwrap();
        let second = store
            .import_script("lx.on('request', () => 2)", "test:second", "Second")
            .unwrap();
        assert!(matches!(
            store.set_order(vec![first.id.clone()]),
            Err(SourceStoreError::InvalidSourceOrder(_))
        ));
        assert!(matches!(
            store.set_order(vec![first.id.clone(), first.id.clone()]),
            Err(SourceStoreError::InvalidSourceOrder(_))
        ));
        store
            .set_order(vec![second.id.clone(), first.id.clone()])
            .unwrap();
        drop(store);
        let reopened = SourceStore::open(&root).unwrap();
        assert_eq!(
            reopened
                .list()
                .iter()
                .map(|entry| entry.source.id.as_str())
                .collect::<Vec<_>>(),
            [second.id.as_str(), first.id.as_str()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_resolution_strictly_matches_effective_priority_and_recovers_from_real_successes() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let unhealthy = store
            .import_script("lx.on('request', () => 1)", "test:red", "Red")
            .unwrap();
        let healthy = store
            .import_script("lx.on('request', () => 2)", "test:green", "Green")
            .unwrap();
        set_test_health(
            &mut store,
            &unhealthy.id,
            &[(false, 1), (false, 2), (false, 3)],
        );
        set_test_health(&mut store, &healthy.id, &[(true, 1), (true, 2), (true, 3)]);
        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [healthy.id.clone(), unhealthy.id.clone()]
        );
        let effective_from_list = {
            let mut listed = store
                .list()
                .into_iter()
                .filter_map(|entry| {
                    entry
                        .effective_priority
                        .map(|priority| (priority, entry.source.id))
                })
                .collect::<Vec<_>>();
            listed.sort_by_key(|(priority, _)| *priority);
            listed.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        };
        assert_eq!(
            effective_from_list,
            store.resolution_source_ids(None).unwrap()
        );

        // A user may explicitly call a red source. Its real success sample moves it back through
        // the ordinary health buckets; no elapsed time or hidden front-of-chain probe is involved.
        assert_eq!(
            store.resolution_source_ids(Some(&unhealthy.id)).unwrap(),
            [unhealthy.id.clone(), healthy.id.clone()]
        );
        let source = store
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == unhealthy.id)
            .unwrap();
        source.health_samples.push(SourceHealthSample {
            success: true,
            latency_ms: 500,
            recorded_at_ms: 4,
        });
        assert_eq!(source.health_summary().state, SourceHealthState::Degraded);
        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [healthy.id.clone(), unhealthy.id.clone()]
        );

        let source = store
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == unhealthy.id)
            .unwrap();
        source
            .health_samples
            .extend((5..=15).map(|recorded_at_ms| SourceHealthSample {
                success: true,
                latency_ms: 500,
                recorded_at_ms,
            }));
        assert_eq!(source.health_summary().state, SourceHealthState::Healthy);
        assert_eq!(
            store.resolution_source_ids(None).unwrap(),
            [unhealthy.id.clone(), healthy.id.clone()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backups_preserve_order_and_enabled_but_clear_local_health_and_route() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let first = store
            .import_script("lx.on('request', () => 1)", "test:first", "First")
            .unwrap();
        let second = store
            .import_script("lx.on('request', () => 2)", "test:second", "Second")
            .unwrap();
        store.set_enabled(&first.id, false).unwrap();
        store
            .set_order(vec![second.id.clone(), first.id.clone()])
            .unwrap();
        store.record_health_sample(&second.id, true, 300).unwrap();
        store
            .record_successful_route(&second.id, NetworkRoute::SystemProxy)
            .unwrap();
        let backup = store.export_backup().unwrap();
        assert_eq!(
            backup.source_order.as_ref().unwrap(),
            &[second.id.clone(), first.id.clone()]
        );
        store.restore_backup(backup).unwrap();
        let listed = store.list();
        assert_eq!(listed[0].source.id, second.id);
        assert_eq!(listed[1].source.id, first.id);
        assert!(!listed[1].source.enabled);
        assert_eq!(listed[0].source.health_summary().sample_count, 0);
        assert_eq!(store.preferred_route(&second.id).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_one_backups_without_order_or_enabled_still_restore() {
        let root = temporary_root();
        let script_a = "lx.on('request', () => 1)";
        let script_b = "lx.on('request', () => 2)";
        let id_a = script_id(script_a.as_bytes());
        let id_b = script_id(script_b.as_bytes());
        let backup: SourceBackup = serde_json::from_value(serde_json::json!({
            "version": 1,
            "activeSourceId": id_b.clone(),
            "fallbackEnabled": false,
            "fallbackSourceIds": [id_a.clone()],
            "sources": [
                {
                    "origin": "local-a.js",
                    "fallbackName": "A",
                    "updatesEnabled": true,
                    "script": script_a,
                    "config": {}
                },
                {
                    "origin": "local-b.js",
                    "fallbackName": "B",
                    "updatesEnabled": false,
                    "script": script_b,
                    "config": {}
                }
            ]
        }))
        .unwrap();
        assert!(backup.source_order.is_none());
        let mut store = SourceStore::open(&root).unwrap();
        store.restore_backup(backup).unwrap();
        let listed = store.list();
        assert_eq!(listed[0].source.id, id_b);
        assert!(listed[0].source.enabled);
        assert_eq!(listed[1].source.id, id_a);
        assert!(!listed[1].source.enabled);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reimport_atomically_replaces_changed_scripts_and_preserves_user_state() {
        let root = temporary_root();
        let mut store = SourceStore::open(&root).unwrap();
        let old_script = "/*!\n * @name Old\n */\nlx.on('request', () => 1)";
        let new_script = "/*!\n * @name New\n */\nlx.on('request', () => 2)";
        let first = store
            .import_script(old_script, "test:first", "First")
            .unwrap();
        let second = store
            .import_script("lx.on('request', () => 3)", "test:second", "Second")
            .unwrap();
        store.set_enabled(&first.id, false).unwrap();
        store.set_updates_enabled(&first.id, false).unwrap();
        store
            .set_config(&first.id, serde_json::json!({"token":"kept"}))
            .unwrap();
        store
            .set_capabilities(&first.id, serde_json::json!({"sources": {}}))
            .unwrap();
        store.record_health_sample(&first.id, false, 5_000).unwrap();
        store
            .record_successful_route(&first.id, NetworkRoute::SystemProxy)
            .unwrap();
        let old_path = first.script_path.clone();

        let replacement = store
            .reimport_script(&first.id, new_script, "fallback")
            .unwrap();
        assert_ne!(replacement.id, first.id);
        assert_eq!(replacement.metadata.name, "New");
        assert!(!replacement.enabled);
        assert!(!replacement.updates_enabled);
        assert_eq!(replacement.config["token"], "kept");
        assert_eq!(replacement.capabilities, Value::Null);
        assert!(replacement.health_samples.is_empty());
        assert_eq!(
            replacement.last_successful_route,
            Some(NetworkRoute::SystemProxy)
        );
        assert!(!old_path.exists());
        assert!(replacement.script_path.exists());
        assert_eq!(store.list()[0].source.id, replacement.id);
        assert_eq!(store.list()[1].source.id, second.id);

        assert!(matches!(
            store.reimport_script(&replacement.id, "lx.on('request', () => 3)", "conflict"),
            Err(SourceStoreError::SourceIdConflict(_))
        ));

        let replacement_id = replacement.id.clone();
        store
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == replacement_id)
            .unwrap()
            .metadata
            .name = "Stale".into();
        let refreshed = store
            .reimport_script(&replacement.id, new_script, "fallback")
            .unwrap();
        assert_eq!(refreshed.metadata.name, "New");
        fs::remove_dir_all(root).unwrap();
    }

    fn set_test_health(store: &mut SourceStore, id: &str, samples: &[(bool, u64)]) {
        store
            .config
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .unwrap()
            .health_samples = samples
            .iter()
            .map(|(success, recorded_at_ms)| SourceHealthSample {
                success: *success,
                latency_ms: if *success { 500 } else { 5_000 },
                recorded_at_ms: *recorded_at_ms,
            })
            .collect();
    }

    fn temporary_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gx-source-test-{}-{}", std::process::id(), nanos))
    }
}
