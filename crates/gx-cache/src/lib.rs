use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use gx_contracts::MediaType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const DEFAULT_LIMIT_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheKey {
    pub provider_id: String,
    pub provider_track_id: String,
    pub quality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub key: CacheKey,
    pub audio_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub media_type: MediaType,
    pub source_sample_rate: Option<u32>,
    pub source_bit_depth: Option<u32>,
    pub source_channels: Option<u16>,
    pub byte_len: u64,
    pub completed_at_ms: u64,
    pub last_accessed_at_ms: u64,
    pub pinned: bool,
    /// Display title captured at complete time (optional for older manifests).
    #[serde(default)]
    pub title: String,
    /// Display artist captured at complete time (optional for older manifests).
    #[serde(default)]
    pub artist: String,
}

/// Frontend-safe cache row — never exposes absolute disk paths.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntryView {
    pub provider_id: String,
    pub provider_track_id: String,
    pub quality: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub byte_len: u64,
    pub source_sample_rate: Option<u32>,
    pub source_bit_depth: Option<u32>,
    pub source_channels: Option<u16>,
    pub media_type: MediaType,
    pub pinned: bool,
    pub last_accessed_at_ms: u64,
    pub completed_at_ms: u64,
    /// Basename only (e.g. `abc123.flac`), never a full path.
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatus {
    pub directory: PathBuf,
    pub custom_directory: Option<PathBuf>,
    pub limit_bytes: u64,
    pub total_bytes: u64,
    pub entry_count: usize,
    pub pinned_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistentSettings {
    #[serde(default)]
    custom_directory: Option<PathBuf>,
    #[serde(default = "default_limit")]
    limit_bytes: u64,
    #[serde(default)]
    online_favorites: BTreeMap<String, Value>,
}

impl Default for PersistentSettings {
    fn default() -> Self {
        Self {
            custom_directory: None,
            limit_bytes: default_limit(),
            online_favorites: BTreeMap::new(),
        }
    }
}

struct CacheState {
    settings_path: PathBuf,
    default_root: PathBuf,
    root: PathBuf,
    settings: PersistentSettings,
    manifest: Manifest,
}

#[derive(Clone)]
pub struct CacheStore {
    inner: Arc<Mutex<CacheState>>,
}

impl CacheStore {
    pub fn open(app_data: impl AsRef<Path>, executable: Option<&Path>) -> Result<Self> {
        let app_data = app_data.as_ref();
        fs::create_dir_all(app_data)?;
        let settings_path = app_data.join("cache-settings.json");
        let mut settings: PersistentSettings = read_json(&settings_path).unwrap_or_default();
        let fallback_root = app_data.join("cache");
        let executable_root = executable
            .and_then(Path::parent)
            .map(|directory| directory.join("cache"));
        let default_root = executable_root
            .filter(|directory| ensure_writable_directory(directory).is_ok())
            .unwrap_or_else(|| fallback_root.clone());
        let root = settings
            .custom_directory
            .clone()
            .filter(|directory| ensure_writable_directory(directory).is_ok())
            .unwrap_or_else(|| {
                settings.custom_directory = None;
                default_root.clone()
            });
        ensure_writable_directory(&root)?;
        cleanup_part_files(&root);
        let manifest = load_manifest(&root);
        let store = Self {
            inner: Arc::new(Mutex::new(CacheState {
                settings_path,
                default_root,
                root,
                settings,
                manifest,
            })),
        };
        store.persist_all()?;
        Ok(store)
    }

    pub fn status(&self) -> CacheStatus {
        let state = self.inner.lock().unwrap();
        let total_bytes = state
            .manifest
            .entries
            .values()
            .map(|entry| entry.byte_len)
            .sum();
        CacheStatus {
            directory: state.root.clone(),
            custom_directory: state.settings.custom_directory.clone(),
            limit_bytes: state.settings.limit_bytes,
            total_bytes,
            entry_count: state.manifest.entries.len(),
            pinned_count: state
                .manifest
                .entries
                .values()
                .filter(|entry| entry.pinned)
                .count(),
        }
    }

    pub fn lookup(&self, key: &CacheKey) -> Option<CacheEntry> {
        let id = cache_id(key);
        let mut state = self.inner.lock().unwrap();
        let entry = state.manifest.entries.get_mut(&id)?;
        if !entry.audio_path.is_file() || !entry.sidecar_path.is_file() {
            state.manifest.entries.remove(&id);
            let _ = persist_manifest(&state);
            return None;
        }
        entry.last_accessed_at_ms = now_ms();
        let result = entry.clone();
        let _ = persist_manifest(&state);
        Some(result)
    }

    pub fn prepare(&self, key: CacheKey, media_type: MediaType) -> CacheWritePlan {
        self.prepare_with_meta(key, media_type, String::new(), String::new())
    }

    pub fn prepare_with_meta(
        &self,
        key: CacheKey,
        media_type: MediaType,
        title: impl Into<String>,
        artist: impl Into<String>,
    ) -> CacheWritePlan {
        let state = self.inner.lock().unwrap();
        let id = cache_id(&key);
        let extension = media_extension(&media_type);
        let final_path = state.root.join(format!("{id}.{extension}"));
        let sidecar_path = state.root.join(format!("{id}.json"));
        let part_path = state
            .root
            .join(format!("{id}.{}.{}.part", std::process::id(), now_ms()));
        let pinned = state
            .settings
            .online_favorites
            .contains_key(&favorite_id(&key.provider_id, &key.provider_track_id));
        drop(state);
        CacheWritePlan {
            inner: Arc::new(CacheWritePlanInner {
                store: self.clone(),
                entry: Mutex::new(CacheEntry {
                    key,
                    audio_path: final_path,
                    sidecar_path,
                    media_type,
                    source_sample_rate: None,
                    source_bit_depth: None,
                    source_channels: None,
                    byte_len: 0,
                    completed_at_ms: 0,
                    last_accessed_at_ms: 0,
                    pinned,
                    title: title.into(),
                    artist: artist.into(),
                }),
                part_path,
                invalid: AtomicBool::new(false),
            }),
        }
    }

    /// List all completed cache entries for the offline/cache UI.
    /// Absolute paths are never included — only `file_name` basenames.
    pub fn list_entries(&self) -> Vec<CacheEntryView> {
        let state = self.inner.lock().unwrap();
        let mut views = state
            .manifest
            .entries
            .values()
            .filter(|entry| entry.audio_path.is_file())
            .map(|entry| entry_to_view(entry, &state.settings.online_favorites))
            .collect::<Vec<_>>();
        views.sort_by_key(|entry| std::cmp::Reverse(entry.last_accessed_at_ms));
        views
    }

    /// Remove one cache entry: audio file + sidecar + manifest row.
    pub fn remove_entry(&self, key: &CacheKey) -> Result<CacheStatus> {
        let id = cache_id(key);
        {
            let mut state = self.inner.lock().unwrap();
            if let Some(entry) = state.manifest.entries.get(&id).cloned() {
                if remove_entry_files(&entry) {
                    state.manifest.entries.remove(&id);
                    persist_manifest(&state)?;
                } else {
                    bail!(
                        "failed to delete cache files for {}/{} {}",
                        key.provider_id,
                        key.provider_track_id,
                        key.quality
                    );
                }
            }
        }
        Ok(self.status())
    }

    pub fn set_limit_bytes(&self, limit_bytes: u64) -> Result<CacheStatus> {
        if !(1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&limit_bytes) {
            bail!("cache limit must be between 1 MiB and 1 TiB");
        }
        {
            let mut state = self.inner.lock().unwrap();
            state.settings.limit_bytes = limit_bytes;
            persist_settings(&state)?;
        }
        self.evict()?;
        Ok(self.status())
    }

    pub fn set_directory(&self, directory: impl Into<PathBuf>) -> Result<CacheStatus> {
        let directory = directory.into();
        validate_custom_directory(&directory)?;
        ensure_writable_directory(&directory)?;
        cleanup_part_files(&directory);
        let manifest = load_manifest(&directory);
        {
            let mut state = self.inner.lock().unwrap();
            state.settings.custom_directory = Some(directory.clone());
            state.root = directory;
            state.manifest = manifest;
            persist_settings(&state)?;
            persist_manifest(&state)?;
        }
        self.evict()?;
        Ok(self.status())
    }

    pub fn reset_directory(&self) -> Result<CacheStatus> {
        let default_root = {
            let state = self.inner.lock().unwrap();
            state.default_root.clone()
        };
        ensure_writable_directory(&default_root)?;
        let manifest = load_manifest(&default_root);
        {
            let mut state = self.inner.lock().unwrap();
            state.settings.custom_directory = None;
            state.root = default_root;
            state.manifest = manifest;
            persist_settings(&state)?;
            persist_manifest(&state)?;
        }
        Ok(self.status())
    }

    pub fn clear(&self, include_pinned: bool) -> Result<CacheStatus> {
        let mut state = self.inner.lock().unwrap();
        let ids = state
            .manifest
            .entries
            .iter()
            .filter(|(_, entry)| include_pinned || !entry.pinned)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            if state
                .manifest
                .entries
                .get(&id)
                .is_some_and(remove_entry_files)
            {
                state.manifest.entries.remove(&id);
            }
        }
        persist_manifest(&state)?;
        drop(state);
        Ok(self.status())
    }

    pub fn set_online_favorite(
        &self,
        provider_id: &str,
        provider_track_id: &str,
        track: Option<Value>,
        favorite: bool,
    ) -> Result<()> {
        let favorite_key = favorite_id(provider_id, provider_track_id);
        let mut state = self.inner.lock().unwrap();
        if favorite {
            let track = track.context("online favorite metadata is required")?;
            state.settings.online_favorites.insert(favorite_key, track);
        } else {
            state.settings.online_favorites.remove(&favorite_key);
        }
        for entry in state.manifest.entries.values_mut() {
            if entry.key.provider_id == provider_id
                && entry.key.provider_track_id == provider_track_id
            {
                entry.pinned = favorite;
                let _ = write_sidecar(entry);
            }
        }
        persist_settings(&state)?;
        persist_manifest(&state)?;
        Ok(())
    }

    pub fn online_favorites(&self) -> Vec<Value> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .online_favorites
            .values()
            .cloned()
            .collect()
    }

    pub fn is_online_favorite(&self, provider_id: &str, provider_track_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .settings
            .online_favorites
            .contains_key(&favorite_id(provider_id, provider_track_id))
    }

    fn record_completed(&self, mut entry: CacheEntry) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        let now = now_ms();
        entry.completed_at_ms = now;
        entry.last_accessed_at_ms = now;
        entry.pinned = state.settings.online_favorites.contains_key(&favorite_id(
            &entry.key.provider_id,
            &entry.key.provider_track_id,
        ));
        write_sidecar(&entry)?;
        state.manifest.entries.insert(cache_id(&entry.key), entry);
        persist_manifest(&state)?;
        drop(state);
        self.evict()
    }

    fn evict(&self) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        let mut total: u64 = state
            .manifest
            .entries
            .values()
            .map(|entry| entry.byte_len)
            .sum();
        if total <= state.settings.limit_bytes {
            return Ok(());
        }
        let mut candidates = state
            .manifest
            .entries
            .iter()
            .filter(|(_, entry)| !entry.pinned)
            .map(|(id, entry)| (id.clone(), entry.last_accessed_at_ms, entry.byte_len))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, accessed, _)| *accessed);
        for (id, _, size) in candidates {
            if total <= state.settings.limit_bytes {
                break;
            }
            if let Some(entry) = state.manifest.entries.get(&id)
                && remove_entry_files(entry)
            {
                state.manifest.entries.remove(&id);
                total = total.saturating_sub(size);
            }
        }
        persist_manifest(&state)
    }

    fn persist_all(&self) -> Result<()> {
        let state = self.inner.lock().unwrap();
        persist_settings(&state)?;
        persist_manifest(&state)
    }
}

#[derive(Clone)]
pub struct CacheWritePlan {
    inner: Arc<CacheWritePlanInner>,
}

struct CacheWritePlanInner {
    store: CacheStore,
    entry: Mutex<CacheEntry>,
    part_path: PathBuf,
    invalid: AtomicBool,
}

impl CacheWritePlan {
    pub fn begin(&self) -> Option<CacheWriter> {
        if self.inner.invalid.load(Ordering::Acquire) {
            return None;
        }
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.inner.part_path)
            .ok()?;
        Some(CacheWriter {
            plan: self.clone(),
            file: Some(file),
            written: 0,
            finalized: false,
        })
    }

    pub fn invalidate(&self) {
        self.inner.invalid.store(true, Ordering::Release);
    }

    pub fn update_source_spec(&self, sample_rate: u32, bit_depth: Option<u32>, channels: u16) {
        let mut entry = self.inner.entry.lock().unwrap();
        entry.source_sample_rate = Some(sample_rate);
        entry.source_bit_depth = bit_depth;
        entry.source_channels = Some(channels);
    }

    pub fn update_media_type(&self, media_type: MediaType) {
        self.inner.entry.lock().unwrap().media_type = media_type;
    }
}

pub struct CacheWriter {
    plan: CacheWritePlan,
    file: Option<File>,
    written: u64,
    finalized: bool,
}

impl CacheWriter {
    pub fn append(&mut self, bytes: &[u8]) {
        if self.plan.inner.invalid.load(Ordering::Acquire) || self.file.is_none() {
            return;
        }
        let result = self.file.as_mut().unwrap().write_all(bytes);
        if result.is_err() {
            self.abandon();
        } else {
            self.written += bytes.len() as u64;
        }
    }

    pub fn invalidate(&mut self) {
        self.plan.invalidate();
        self.abandon();
    }

    pub fn finish(mut self, total_len: Option<u64>) {
        if self.plan.inner.invalid.load(Ordering::Acquire)
            || total_len.is_some_and(|expected| expected != self.written)
            || self.file.is_none()
        {
            self.abandon();
            return;
        }
        if let Some(mut file) = self.file.take()
            && file.flush().is_err()
        {
            self.abandon();
            return;
        }
        let mut entry = self.plan.inner.entry.lock().unwrap().clone();
        entry.byte_len = self.written;
        let _ = fs::remove_file(&entry.audio_path);
        if fs::rename(&self.plan.inner.part_path, &entry.audio_path).is_err() {
            self.abandon();
            return;
        }
        if self
            .plan
            .inner
            .store
            .record_completed(entry.clone())
            .is_err()
        {
            let _ = fs::remove_file(&entry.audio_path);
            let _ = fs::remove_file(&entry.sidecar_path);
            return;
        }
        self.finalized = true;
    }

    fn abandon(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.plan.inner.part_path);
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if !self.finalized {
            self.abandon();
        }
    }
}

fn cache_id(key: &CacheKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.provider_id.as_bytes());
    hasher.update([0]);
    hasher.update(key.provider_track_id.as_bytes());
    hasher.update([0]);
    hasher.update(key.quality.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn favorite_id(provider_id: &str, provider_track_id: &str) -> String {
    format!("{provider_id}\0{provider_track_id}")
}

fn media_extension(media_type: &MediaType) -> &'static str {
    match media_type {
        MediaType::Mp3 => "mp3",
        MediaType::Flac => "flac",
        MediaType::Aac => "aac",
        MediaType::Ogg => "ogg",
        MediaType::Wav => "wav",
        MediaType::Hls | MediaType::Unknown => "media",
    }
}

fn validate_custom_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() || path.parent().is_none() {
        bail!("cache directory cannot be a filesystem root");
    }
    if let Ok(windows) = std::env::var("WINDIR") {
        let windows = PathBuf::from(windows);
        if path == windows || path.starts_with(windows.join("System32")) {
            bail!("cache directory cannot be a Windows system directory");
        }
    }
    Ok(())
}

fn ensure_writable_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create cache directory {}", path.display()))?;
    let probe = path.join(format!(
        ".gx-write-probe-{}-{}",
        std::process::id(),
        now_ms()
    ));
    File::create(&probe)
        .and_then(|mut file| file.write_all(b"gx"))
        .with_context(|| format!("cache directory is not writable: {}", path.display()))?;
    let _ = fs::remove_file(probe);
    Ok(())
}

fn cleanup_part_files(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "part")
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn load_manifest(root: &Path) -> Manifest {
    let mut manifest: Manifest = read_json(&root.join("manifest.json")).unwrap_or_default();
    manifest.entries.retain(|_, entry| {
        entry.audio_path.starts_with(root)
            && entry.sidecar_path.starts_with(root)
            && entry.audio_path.is_file()
            && entry.sidecar_path.is_file()
    });
    manifest
}

fn persist_settings(state: &CacheState) -> Result<()> {
    write_json_atomic(&state.settings_path, &state.settings)
}

fn persist_manifest(state: &CacheState) -> Result<()> {
    write_json_atomic(&state.root.join("manifest.json"), &state.manifest)
}

fn write_sidecar(entry: &CacheEntry) -> Result<()> {
    write_json_atomic(&entry.sidecar_path, entry)
}

fn remove_entry_files(entry: &CacheEntry) -> bool {
    match fs::remove_file(&entry.audio_path) {
        Ok(()) => {
            let _ = fs::remove_file(&entry.sidecar_path);
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let _ = fs::remove_file(&entry.sidecar_path);
            true
        }
        Err(_) => false,
    }
}

fn entry_to_view(entry: &CacheEntry, favorites: &BTreeMap<String, Value>) -> CacheEntryView {
    let fav = favorites.get(&favorite_id(
        &entry.key.provider_id,
        &entry.key.provider_track_id,
    ));
    let title = non_empty(&entry.title)
        .or_else(|| fav.and_then(|v| v.get("title").and_then(Value::as_str).map(str::to_owned)))
        .unwrap_or_else(|| entry.key.provider_track_id.clone());
    let artist = non_empty(&entry.artist)
        .or_else(|| fav.and_then(|v| v.get("artist").and_then(Value::as_str).map(str::to_owned)))
        .unwrap_or_default();
    let album = fav
        .and_then(|v| v.get("album").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let file_name = entry
        .audio_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio")
        .to_owned();
    CacheEntryView {
        provider_id: entry.key.provider_id.clone(),
        provider_track_id: entry.key.provider_track_id.clone(),
        quality: entry.key.quality.clone(),
        title,
        artist,
        album,
        byte_len: entry.byte_len,
        source_sample_rate: entry.source_sample_rate,
        source_bit_depth: entry.source_bit_depth,
        source_channels: entry.source_channels,
        media_type: entry.media_type.clone(),
        pinned: entry.pinned,
        last_accessed_at_ms: entry.last_accessed_at_ms,
        completed_at_ms: entry.completed_at_ms,
        file_name,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("JSON path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!(
        "{}.part",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ));
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    let _ = fs::remove_file(path);
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn default_limit() -> u64 {
    DEFAULT_LIMIT_BYTES
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_write_hits_and_interrupted_write_leaves_no_part() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let first_key = key("one", "320k");
        let plan = store.prepare(first_key.clone(), MediaType::Mp3);
        plan.update_source_spec(44_100, Some(16), 2);
        plan.update_media_type(MediaType::Flac);
        let mut writer = plan.begin().unwrap();
        writer.append(b"complete audio");
        writer.finish(Some(14));
        let hit = store.lookup(&first_key).unwrap();
        assert_eq!(fs::read(&hit.audio_path).unwrap(), b"complete audio");
        assert_eq!(hit.source_sample_rate, Some(44_100));
        assert_eq!(hit.media_type, MediaType::Flac);

        let interrupted = store.prepare(key("two", "128k"), MediaType::Mp3);
        let mut writer = interrupted.begin().unwrap();
        writer.append(b"partial");
        writer.invalidate();
        drop(writer);
        assert!(
            !fs::read_dir(store.status().directory)
                .unwrap()
                .flatten()
                .any(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "part"))
        );
        let mismatched_key = key("three", "320k");
        let plan = store.prepare(mismatched_key.clone(), MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        writer.append(b"short");
        writer.finish(Some(99));
        assert!(store.lookup(&mismatched_key).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_failure_is_best_effort_and_never_creates_an_entry() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let cache_key = key("write-failure", "320k");
        let plan = store.prepare(cache_key.clone(), MediaType::Mp3);
        fs::write(&plan.inner.part_path, b"read only").unwrap();
        let read_only = File::open(&plan.inner.part_path).unwrap();
        let mut writer = CacheWriter {
            plan,
            file: Some(read_only),
            written: 0,
            finalized: false,
        };
        writer.append(b"audio");
        writer.finish(Some(5));
        assert!(store.lookup(&cache_key).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lru_evicts_unpinned_and_keeps_favorites() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let pinned = key("pinned", "flac");
        store
            .set_online_favorite(
                "kg",
                "pinned",
                Some(serde_json::json!({"id":"pinned"})),
                true,
            )
            .unwrap();
        write_entry(&store, pinned.clone(), 800 * 1024);
        write_entry(&store, key("old", "320k"), 800 * 1024);
        store.set_limit_bytes(1024 * 1024).unwrap();
        assert!(store.lookup(&pinned).is_some());
        assert!(store.lookup(&key("old", "320k")).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn list_entries_hides_paths_and_remove_deletes_files() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let plan = store.prepare_with_meta(key("song-a", "320k"), MediaType::Mp3, "歌A", "歌手A");
        let mut writer = plan.begin().unwrap();
        writer.append(b"audio-bytes-here");
        writer.finish(Some(16));

        let listed = store.list_entries();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "歌A");
        assert_eq!(listed[0].artist, "歌手A");
        assert_eq!(listed[0].quality, "320k");
        assert_eq!(listed[0].byte_len, 16);
        assert!(!listed[0].file_name.contains('\\'));
        assert!(!listed[0].file_name.contains('/'));
        // Absolute path must not appear in serialized view.
        let json = serde_json::to_string(&listed[0]).unwrap();
        assert!(!json.contains(root.to_str().unwrap_or("___")));

        let audio = store.lookup(&key("song-a", "320k")).unwrap().audio_path;
        let sidecar = audio.with_extension("json");
        assert!(audio.is_file());
        assert!(sidecar.is_file());

        store.remove_entry(&key("song-a", "320k")).unwrap();
        assert!(store.list_entries().is_empty());
        assert!(!audio.is_file());
        assert!(!sidecar.is_file());
        assert_eq!(store.status().entry_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    fn write_entry(store: &CacheStore, key: CacheKey, size: usize) {
        let plan = store.prepare(key, MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        let bytes = vec![1; size];
        writer.append(&bytes);
        writer.finish(Some(size as u64));
    }

    fn key(track: &str, quality: &str) -> CacheKey {
        CacheKey {
            provider_id: "kg".into(),
            provider_track_id: track.into(),
            quality: quality.into(),
        }
    }

    fn temporary_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gx-cache-test-{}-{nanos}", std::process::id()))
    }
}
