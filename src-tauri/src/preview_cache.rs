use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PREVIEW_CACHE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MANIFEST_FILE: &str = "preview-cache-manifest.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheStatus {
    pub directory: PathBuf,
    pub limit_bytes: u64,
    pub total_bytes: u64,
    pub entry_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewEntry {
    file_name: String,
    byte_len: u64,
    last_accessed_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewManifest {
    entries: BTreeMap<String, PreviewEntry>,
}

struct PreviewCacheInner {
    manifest: PreviewManifest,
}

pub struct PreviewCacheStore {
    root: PathBuf,
    inner: Mutex<PreviewCacheInner>,
}

impl PreviewCacheStore {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let manifest = fs::read(root.join(MANIFEST_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PreviewManifest>(&bytes).ok())
            .unwrap_or_default();
        let store = Self {
            root,
            inner: Mutex::new(PreviewCacheInner { manifest }),
        };
        store.reconcile()?;
        Ok(store)
    }

    pub fn lookup(
        &self,
        provider_id: &str,
        provider_track_id: &str,
    ) -> Result<Option<PathBuf>, String> {
        let key = preview_key(provider_id, provider_track_id);
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.manifest.entries.get_mut(&key) else {
            return Ok(None);
        };
        let path = self.root.join(&entry.file_name);
        if !path.is_file() {
            inner.manifest.entries.remove(&key);
            persist_manifest(&self.root, &inner.manifest)?;
            return Ok(None);
        }
        entry.last_accessed_at_ms = now_ms();
        persist_manifest(&self.root, &inner.manifest)?;
        Ok(Some(path))
    }

    pub fn insert(
        &self,
        provider_id: &str,
        provider_track_id: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, String> {
        if bytes.is_empty() {
            return Err("preview response is empty".into());
        }
        if bytes.len() as u64 > PREVIEW_CACHE_LIMIT_BYTES {
            return Err("preview response exceeds the preview cache limit".into());
        }
        let key = preview_key(provider_id, provider_track_id);
        let file_name = format!("{key}.{}", safe_extension(extension));
        let destination = self.root.join(&file_name);
        let temporary = self.root.join(format!("{file_name}.tmp"));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        if destination.exists() {
            fs::remove_file(&destination).map_err(|error| error.to_string())?;
        }
        fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;

        let mut inner = self.inner.lock().unwrap();
        if let Some(previous) = inner.manifest.entries.insert(
            key,
            PreviewEntry {
                file_name,
                byte_len: bytes.len() as u64,
                last_accessed_at_ms: now_ms(),
            },
        ) && previous.file_name != destination.file_name().unwrap().to_string_lossy()
        {
            let _ = fs::remove_file(self.root.join(previous.file_name));
        }
        evict_locked(&self.root, &mut inner.manifest)?;
        persist_manifest(&self.root, &inner.manifest)?;
        Ok(destination)
    }

    pub fn clear(&self) -> Result<PreviewCacheStatus, String> {
        let mut inner = self.inner.lock().unwrap();
        for entry in inner.manifest.entries.values() {
            let _ = fs::remove_file(self.root.join(&entry.file_name));
        }
        inner.manifest.entries.clear();
        persist_manifest(&self.root, &inner.manifest)?;
        Ok(status_locked(&self.root, &inner.manifest))
    }

    pub fn status(&self) -> PreviewCacheStatus {
        let inner = self.inner.lock().unwrap();
        status_locked(&self.root, &inner.manifest)
    }

    fn reconcile(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        inner.manifest.entries.retain(|_, entry| {
            self.root
                .join(&entry.file_name)
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == entry.byte_len)
        });
        evict_locked(&self.root, &mut inner.manifest)?;
        persist_manifest(&self.root, &inner.manifest)
    }
}

fn evict_locked(root: &Path, manifest: &mut PreviewManifest) -> Result<(), String> {
    let mut total = manifest
        .entries
        .values()
        .map(|entry| entry.byte_len)
        .sum::<u64>();
    if total <= PREVIEW_CACHE_LIMIT_BYTES {
        return Ok(());
    }
    let mut lru = manifest
        .entries
        .iter()
        .map(|(key, entry)| (key.clone(), entry.last_accessed_at_ms, entry.byte_len))
        .collect::<Vec<_>>();
    lru.sort_by_key(|(_, accessed, _)| *accessed);
    for (key, _, byte_len) in lru {
        if total <= PREVIEW_CACHE_LIMIT_BYTES {
            break;
        }
        if let Some(entry) = manifest.entries.remove(&key) {
            let _ = fs::remove_file(root.join(entry.file_name));
            total = total.saturating_sub(byte_len);
        }
    }
    Ok(())
}

fn status_locked(root: &Path, manifest: &PreviewManifest) -> PreviewCacheStatus {
    PreviewCacheStatus {
        directory: root.to_path_buf(),
        limit_bytes: PREVIEW_CACHE_LIMIT_BYTES,
        total_bytes: manifest.entries.values().map(|entry| entry.byte_len).sum(),
        entry_count: manifest.entries.len(),
    }
}

fn persist_manifest(root: &Path, manifest: &PreviewManifest) -> Result<(), String> {
    let path = root.join(MANIFEST_FILE);
    let temporary = root.join(format!("{MANIFEST_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn preview_key(provider_id: &str, provider_track_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hasher.update([0]);
    hasher.update(provider_track_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn safe_extension(extension: &str) -> &str {
    match extension {
        "mp3" | "m4a" | "flac" | "ogg" | "wav" => extension,
        _ => "media",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gx-preview-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn insert_lookup_status_and_clear_are_managed() {
        let root = root();
        let store = PreviewCacheStore::open(root.clone()).unwrap();
        let path = store.insert("itunes", "track", "m4a", b"preview").unwrap();
        assert_eq!(store.lookup("itunes", "track").unwrap(), Some(path));
        assert_eq!(store.status().entry_count, 1);
        assert_eq!(store.status().total_bytes, 7);
        assert_eq!(store.clear().unwrap().entry_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keys_are_fixed_hashes_and_do_not_expose_catalog_ids() {
        let key = preview_key("provider-secret", "track-secret");
        assert_eq!(key.len(), 64);
        assert!(!key.contains("secret"));
    }

    #[test]
    fn lru_evicts_the_oldest_entries_when_the_limit_is_exceeded() {
        let root = root();
        let store = PreviewCacheStore::open(root.clone()).unwrap();
        store.insert("provider", "old", "mp3", b"old").unwrap();
        store.insert("provider", "new", "mp3", b"new").unwrap();
        let old_key = preview_key("provider", "old");
        let new_key = preview_key("provider", "new");
        {
            let mut inner = store.inner.lock().unwrap();
            inner.manifest.entries.get_mut(&old_key).unwrap().byte_len = 200 * 1024 * 1024;
            inner
                .manifest
                .entries
                .get_mut(&old_key)
                .unwrap()
                .last_accessed_at_ms = 1;
            inner.manifest.entries.get_mut(&new_key).unwrap().byte_len = 200 * 1024 * 1024;
            inner
                .manifest
                .entries
                .get_mut(&new_key)
                .unwrap()
                .last_accessed_at_ms = 2;
            evict_locked(&root, &mut inner.manifest).unwrap();
            assert!(!inner.manifest.entries.contains_key(&old_key));
            assert!(inner.manifest.entries.contains_key(&new_key));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
