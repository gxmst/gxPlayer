use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod safe_http;

const MAX_SCRIPT_BYTES: usize = 5 * 1024 * 1024;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBackup {
    pub version: u32,
    pub active_source_id: Option<String>,
    pub sources: Vec<BackupSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSource {
    pub origin: String,
    pub fallback_name: String,
    pub updates_enabled: bool,
    pub script: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceConfig {
    active_source_id: Option<String>,
    sources: Vec<ManagedSource>,
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
    #[error("source storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source storage JSON failed: {0}")]
    Json(#[from] serde_json::Error),
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
        let config = if config_path.exists() {
            serde_json::from_slice(&fs::read(config_path)?)?
        } else {
            SourceConfig::default()
        };
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
                })
            })
            .collect::<Result<Vec<_>, SourceStoreError>>()?;
        Ok(SourceBackup {
            version: 1,
            active_source_id: self.config.active_source_id.clone(),
            sources,
        })
    }

    pub fn restore_backup(&mut self, backup: SourceBackup) -> Result<(), SourceStoreError> {
        if backup.version != 1 {
            return Err(SourceStoreError::InvalidBackupVersion(backup.version));
        }
        if backup.sources.len() > 64
            || backup
                .sources
                .iter()
                .map(|source| source.script.len())
                .sum::<usize>()
                > 20 * 1024 * 1024
        {
            return Err(SourceStoreError::BackupTooLarge);
        }
        for source in &backup.sources {
            validate_script(&source.script)?;
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
        self.persist()
    }

    fn persist(&self) -> Result<(), SourceStoreError> {
        let bytes = serde_json::to_vec_pretty(&self.config)?;
        let temporary = self.root.join("sources.json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, self.root.join("sources.json"))?;
        Ok(())
    }
}

fn default_true() -> bool {
    true
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
        || script.contains("window.lx"))
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
        assert!(!store.active_updates_enabled());
        let backup = store.export_backup().unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, second.id);
        store.remove(&second.id).unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, first.id);
        store.restore_backup(backup).unwrap();
        assert_eq!(store.active_script().unwrap().unwrap().0.id, second.id);
        assert!(!store.active_updates_enabled());
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

    fn temporary_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gx-source-test-{}-{}", std::process::id(), nanos))
    }
}
