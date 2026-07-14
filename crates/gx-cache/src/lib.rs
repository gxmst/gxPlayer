use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use gx_contracts::MediaType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const DEFAULT_LIMIT_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const CACHE_DIRECTORY_NAME: &str = "GXPlayerCache";
const CACHE_FILE_PREFIX: &str = "gx-cache-";
const MANIFEST_FILE_NAME: &str = "gx-cache-manifest.json";
const DIAGNOSTIC_CAPACITY: usize = 128;
static JSON_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDiagnostic {
    pub category: &'static str,
    pub source: &'static str,
    pub summary: String,
}

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
    pub revision: u64,
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
    epoch: u64,
    next_writer_token: u64,
    active_writers: BTreeMap<String, u64>,
}

#[derive(Clone)]
pub struct CacheStore {
    inner: Arc<Mutex<CacheState>>,
    diagnostics: Arc<Mutex<VecDeque<CacheDiagnostic>>>,
    revision: Arc<AtomicU64>,
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
            .as_ref()
            .map(|directory| dedicated_cache_root(directory))
            .filter(|directory| ensure_writable_directory(directory).is_ok())
            .unwrap_or_else(|| {
                settings.custom_directory = None;
                default_root.clone()
            });
        ensure_writable_directory(&root)?;
        let (manifest, initial_diagnostics) = load_manifest(&root);
        let diagnostics = initial_diagnostics
            .into_iter()
            .rev()
            .take(DIAGNOSTIC_CAPACITY)
            .collect::<Vec<_>>();
        let store = Self {
            inner: Arc::new(Mutex::new(CacheState {
                settings_path,
                default_root,
                root,
                settings,
                manifest,
                epoch: 1,
                next_writer_token: 0,
                active_writers: BTreeMap::new(),
            })),
            diagnostics: Arc::new(Mutex::new(diagnostics.into_iter().rev().collect())),
            revision: Arc::new(AtomicU64::new(1)),
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
            revision: self.revision(),
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
        if let Some(code) = entry_file_error_code(entry) {
            self.push_diagnostic(
                "cache_read_failed",
                "cache",
                format!("stage=lookup code={code}"),
            );
            state.manifest.entries.remove(&id);
            if persist_manifest(&state).is_err() {
                self.push_diagnostic(
                    "cache_write_failed",
                    "cache",
                    "stage=lookup_cleanup code=manifest_persist_failed".into(),
                );
            }
            self.mark_changed();
            return None;
        }
        entry.last_accessed_at_ms = now_ms();
        let result = entry.clone();
        if persist_manifest(&state).is_err() {
            self.push_diagnostic(
                "cache_write_failed",
                "cache",
                "stage=touch code=manifest_persist_failed".into(),
            );
        }
        self.mark_changed();
        Some(result)
    }

    pub fn lookup_track(
        &self,
        provider_id: &str,
        provider_track_id: &str,
        preferred_quality: Option<&str>,
    ) -> Option<CacheEntry> {
        if let Some(quality) = preferred_quality.filter(|quality| !quality.trim().is_empty()) {
            let key = CacheKey {
                provider_id: provider_id.to_owned(),
                provider_track_id: provider_track_id.to_owned(),
                quality: quality.to_owned(),
            };
            if let Some(hit) = self.lookup(&key) {
                return Some(hit);
            }
        }
        let mut candidates = {
            let state = self.inner.lock().unwrap();
            state
                .manifest
                .entries
                .values()
                .filter(|entry| {
                    entry.key.provider_id == provider_id
                        && entry.key.provider_track_id == provider_track_id
                })
                .map(|entry| (entry.key.clone(), entry.last_accessed_at_ms))
                .collect::<Vec<_>>()
        };
        candidates.sort_by_key(|(_, accessed)| std::cmp::Reverse(*accessed));
        candidates
            .into_iter()
            .find_map(|(key, _)| self.lookup(&key))
    }

    pub fn drain_diagnostics(&self) -> Vec<CacheDiagnostic> {
        self.diagnostics.lock().unwrap().drain(..).collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn mark_changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn push_diagnostic(&self, category: &'static str, source: &'static str, summary: String) {
        let mut diagnostics = self.diagnostics.lock().unwrap();
        if diagnostics.len() == DIAGNOSTIC_CAPACITY {
            diagnostics.pop_front();
        }
        diagnostics.push_back(CacheDiagnostic {
            category,
            source,
            summary,
        });
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
        let mut state = self.inner.lock().unwrap();
        let id = cache_id(&key);
        state.next_writer_token = state.next_writer_token.wrapping_add(1).max(1);
        let writer_token = state.next_writer_token;
        state.active_writers.insert(id.clone(), writer_token);
        let epoch = state.epoch;
        let extension = media_extension(&media_type);
        let file_tag = format!("{CACHE_FILE_PREFIX}{id}-{}-{writer_token}", now_ms());
        let final_path = state.root.join(format!("{file_tag}.{extension}"));
        let sidecar_path = state.root.join(format!("{file_tag}.json"));
        let part_path = state.root.join(format!("{file_tag}.part"));
        let staged_path = state.root.join(format!("{file_tag}.ready"));
        let root = state.root.clone();
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
                id,
                writer_token,
                epoch,
                root,
                part_path,
                staged_path,
                invalid: AtomicBool::new(false),
                started: AtomicBool::new(false),
                staged: AtomicBool::new(false),
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
            state.active_writers.remove(&id);
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
        self.mark_changed();
        Ok(self.status())
    }

    /// Remove multiple entries by key (skips missing keys).
    pub fn remove_entries(&self, keys: &[CacheKey]) -> Result<CacheStatus> {
        {
            let mut state = self.inner.lock().unwrap();
            for key in keys {
                let id = cache_id(key);
                state.active_writers.remove(&id);
                if let Some(entry) = state.manifest.entries.get(&id).cloned()
                    && remove_entry_files(&entry)
                {
                    state.manifest.entries.remove(&id);
                }
            }
            persist_manifest(&state)?;
        }
        self.mark_changed();
        Ok(self.status())
    }

    /// Remove all completed cache entries of a given quality tier (e.g. `128k`).
    /// Pinned favorites are kept unless `include_pinned` is true.
    pub fn remove_by_quality(&self, quality: &str, include_pinned: bool) -> Result<CacheStatus> {
        let quality = quality.trim();
        if quality.is_empty() {
            bail!("quality is required");
        }
        {
            let mut state = self.inner.lock().unwrap();
            let ids = state
                .manifest
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.key.quality.eq_ignore_ascii_case(quality)
                        && (include_pinned || !entry.pinned)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in ids {
                state.active_writers.remove(&id);
                if let Some(entry) = state.manifest.entries.get(&id).cloned()
                    && remove_entry_files(&entry)
                {
                    state.manifest.entries.remove(&id);
                }
            }
            persist_manifest(&state)?;
        }
        self.mark_changed();
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
        self.mark_changed();
        Ok(self.status())
    }

    pub fn set_directory(&self, directory: impl Into<PathBuf>) -> Result<CacheStatus> {
        let selected_directory = directory.into();
        validate_custom_directory(&selected_directory)?;
        let directory = dedicated_cache_root(&selected_directory);
        ensure_writable_directory(&directory)?;
        cleanup_part_files(&directory);
        let (manifest, diagnostics) = load_manifest(&directory);
        for diagnostic in diagnostics {
            self.push_diagnostic(diagnostic.category, diagnostic.source, diagnostic.summary);
        }
        {
            let mut state = self.inner.lock().unwrap();
            state.epoch = state.epoch.wrapping_add(1).max(1);
            state.active_writers.clear();
            state.settings.custom_directory = Some(selected_directory);
            state.root = directory;
            state.manifest = manifest;
            persist_settings(&state)?;
            persist_manifest(&state)?;
        }
        self.evict()?;
        self.mark_changed();
        Ok(self.status())
    }

    pub fn reset_directory(&self) -> Result<CacheStatus> {
        let default_root = {
            let state = self.inner.lock().unwrap();
            state.default_root.clone()
        };
        ensure_writable_directory(&default_root)?;
        let (manifest, diagnostics) = load_manifest(&default_root);
        for diagnostic in diagnostics {
            self.push_diagnostic(diagnostic.category, diagnostic.source, diagnostic.summary);
        }
        {
            let mut state = self.inner.lock().unwrap();
            state.epoch = state.epoch.wrapping_add(1).max(1);
            state.active_writers.clear();
            state.settings.custom_directory = None;
            state.root = default_root;
            state.manifest = manifest;
            persist_settings(&state)?;
            persist_manifest(&state)?;
        }
        self.mark_changed();
        Ok(self.status())
    }

    pub fn clear(&self, include_pinned: bool) -> Result<CacheStatus> {
        let mut state = self.inner.lock().unwrap();
        // A clear is a cache-generation boundary. Existing downloaders may still own an open
        // handle, but they can no longer publish into the freshly-cleared manifest.
        state.epoch = state.epoch.wrapping_add(1).max(1);
        state.active_writers.clear();
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
        self.mark_changed();
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
                if let Err(error) = write_sidecar(entry) {
                    self.push_diagnostic(
                        "cache_write_failed",
                        "cache",
                        format!(
                            "stage=favorite_sidecar code={}",
                            anyhow_io_error_code(&error)
                        ),
                    );
                }
            }
        }
        persist_settings(&state)?;
        persist_manifest(&state)?;
        drop(state);
        self.mark_changed();
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

    fn writer_is_current(&self, id: &str, token: u64, epoch: u64, root: &Path) -> bool {
        let state = self.inner.lock().unwrap();
        state.epoch == epoch && state.root == root && state.active_writers.get(id) == Some(&token)
    }

    fn release_writer(&self, id: &str, token: u64) {
        let mut state = self.inner.lock().unwrap();
        if state.active_writers.get(id) == Some(&token) {
            state.active_writers.remove(id);
        }
    }

    fn commit_staged(
        &self,
        id: &str,
        token: u64,
        epoch: u64,
        root: &Path,
        staged_path: &Path,
        mut entry: CacheEntry,
    ) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        if state.epoch != epoch
            || state.root != root
            || state.active_writers.get(id) != Some(&token)
        {
            bail!("cache writer belongs to an expired cache generation");
        }
        if !staged_path.is_file()
            || !entry.audio_path.starts_with(root)
            || !entry.sidecar_path.starts_with(root)
        {
            bail!("staged cache entry is unavailable or outside the active cache root");
        }
        let staged_len = match fs::metadata(staged_path) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                self.push_diagnostic(
                    "cache_commit_failed",
                    "cache",
                    format!("stage=commit_metadata code={}", io_error_code(&error)),
                );
                return Err(error.into());
            }
        };
        if staged_len != entry.byte_len {
            bail!("staged cache entry length changed before commit");
        }
        let now = now_ms();
        entry.completed_at_ms = now;
        entry.last_accessed_at_ms = now;
        entry.pinned = state.settings.online_favorites.contains_key(&favorite_id(
            &entry.key.provider_id,
            &entry.key.provider_track_id,
        ));
        if entry.audio_path.exists() {
            bail!("cache destination unexpectedly already exists");
        }
        if let Err(error) = fs::rename(staged_path, &entry.audio_path) {
            self.push_diagnostic(
                "cache_commit_failed",
                "cache",
                format!("stage=commit_rename code={}", io_error_code(&error)),
            );
            return Err(error).context("failed to promote staged cache file");
        }
        if let Err(error) = write_sidecar(&entry) {
            self.push_diagnostic(
                "cache_commit_failed",
                "cache",
                "stage=sidecar code=write_failed".into(),
            );
            let _ = fs::remove_file(&entry.audio_path);
            return Err(error);
        }
        let previous = state.manifest.entries.insert(id.to_owned(), entry.clone());
        if let Err(error) = persist_manifest(&state) {
            self.push_diagnostic(
                "cache_commit_failed",
                "cache",
                "stage=manifest code=persist_failed".into(),
            );
            if let Some(previous) = previous.clone() {
                state.manifest.entries.insert(id.to_owned(), previous);
            } else {
                state.manifest.entries.remove(id);
            }
            let _ = fs::remove_file(&entry.audio_path);
            let _ = fs::remove_file(&entry.sidecar_path);
            return Err(error);
        }
        state.active_writers.remove(id);
        drop(state);
        if let Some(previous) = previous
            && previous.audio_path != entry.audio_path
        {
            let _ = remove_entry_files(&previous);
        }
        let result = self.evict();
        if result.is_err() {
            self.push_diagnostic(
                "cache_commit_failed",
                "cache",
                "stage=evict code=persist_failed".into(),
            );
        }
        if result.is_ok() {
            self.mark_changed();
        }
        result
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

    /// Expensive filesystem reconciliation runs after startup so a GB-scale cache never blocks
    /// the first window. The manifest remains the fast-path source of truth until this completes.
    pub fn deep_validate(&self) -> Result<()> {
        let (root, epoch) = {
            let state = self.inner.lock().unwrap();
            (state.root.clone(), state.epoch)
        };
        let recovered = recover_manifest_from_sidecars(&root);
        let mut state = self.inner.lock().unwrap();
        if state.epoch != epoch || state.root != root {
            return Ok(());
        }

        let mut changed = false;
        for (id, entry) in recovered.entries {
            if let std::collections::btree_map::Entry::Vacant(slot) =
                state.manifest.entries.entry(id)
            {
                slot.insert(entry);
                changed = true;
            }
        }
        let invalid = state
            .manifest
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                let in_root =
                    entry.audio_path.starts_with(&root) && entry.sidecar_path.starts_with(&root);
                let code = if in_root {
                    entry_file_error_code(entry)
                } else {
                    Some("outside_root")
                };
                code.map(|code| (id.clone(), code))
            })
            .collect::<Vec<_>>();
        for (id, code) in invalid {
            state.manifest.entries.remove(&id);
            self.push_diagnostic(
                "cache_read_failed",
                "cache",
                format!("stage=manifest_reconcile code={code}"),
            );
            changed = true;
        }
        if changed {
            persist_manifest(&state)?;
            drop(state);
            self.mark_changed();
        }
        Ok(())
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
    id: String,
    writer_token: u64,
    epoch: u64,
    root: PathBuf,
    part_path: PathBuf,
    staged_path: PathBuf,
    invalid: AtomicBool,
    started: AtomicBool,
    staged: AtomicBool,
}

impl CacheWritePlan {
    pub fn begin(&self) -> Option<CacheWriter> {
        if self.inner.invalid.load(Ordering::Acquire)
            || self.inner.started.swap(true, Ordering::AcqRel)
            || !self.is_current()
        {
            return None;
        }
        let file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.inner.part_path)
        {
            Ok(file) => file,
            Err(error) => {
                self.inner.store.push_diagnostic(
                    "cache_write_failed",
                    "cache",
                    format!("stage=begin code={}", io_error_code(&error)),
                );
                self.invalidate();
                return None;
            }
        };
        Some(CacheWriter {
            plan: self.clone(),
            file: Some(file),
            written: 0,
            finalized: false,
        })
    }

    pub fn invalidate(&self) {
        self.inner.invalid.store(true, Ordering::Release);
        self.inner
            .store
            .release_writer(&self.inner.id, self.inner.writer_token);
        let _ = fs::remove_file(&self.inner.part_path);
        let _ = fs::remove_file(&self.inner.staged_path);
    }

    /// Publish a fully-downloaded entry after the decoder has consumed the media to a clean EOF.
    /// `CacheWriter::finish` intentionally does not make an entry visible on its own.
    pub fn commit(&self) -> Result<()> {
        if self.inner.invalid.load(Ordering::Acquire)
            || !self.inner.staged.load(Ordering::Acquire)
            || !self.is_current()
        {
            bail!("cache download is not staged for the active cache generation");
        }
        let entry = self.inner.entry.lock().unwrap().clone();
        let result = self.inner.store.commit_staged(
            &self.inner.id,
            self.inner.writer_token,
            self.inner.epoch,
            &self.inner.root,
            &self.inner.staged_path,
            entry,
        );
        if result.is_err() {
            self.invalidate();
        }
        result
    }

    /// Final manifest/sidecar publication can include slow filesystem syncs. Audio workers use
    /// this best-effort path so EOF processing and transport commands never wait on storage.
    pub fn commit_in_background(&self) {
        let plan = self.clone();
        if std::thread::Builder::new()
            .name("gx-cache-commit".into())
            .spawn(move || {
                let _ = plan.commit();
            })
            .is_err()
        {
            self.invalidate();
        }
    }

    fn is_current(&self) -> bool {
        self.inner.store.writer_is_current(
            &self.inner.id,
            self.inner.writer_token,
            self.inner.epoch,
            &self.inner.root,
        )
    }

    pub fn update_source_spec(&self, sample_rate: u32, bit_depth: Option<u32>, channels: u16) {
        if !self.is_current() {
            return;
        }
        let mut entry = self.inner.entry.lock().unwrap();
        entry.source_sample_rate = Some(sample_rate);
        entry.source_bit_depth = bit_depth;
        entry.source_channels = Some(channels);
    }

    pub fn update_media_type(&self, media_type: MediaType) {
        if self.is_current() {
            self.inner.entry.lock().unwrap().media_type = media_type;
        }
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
        if self.plan.inner.invalid.load(Ordering::Acquire)
            || !self.plan.is_current()
            || self.file.is_none()
        {
            self.abandon();
            return;
        }
        let result = self.file.as_mut().unwrap().write_all(bytes);
        if let Err(error) = result {
            self.plan.inner.store.push_diagnostic(
                "cache_write_failed",
                "cache",
                format!("stage=append code={}", io_error_code(&error)),
            );
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
            || !self.plan.is_current()
            || total_len.is_some_and(|expected| expected != self.written)
            || self.file.is_none()
        {
            self.abandon();
            return;
        }
        if let Some(mut file) = self.file.take() {
            if let Err(error) = file.flush() {
                self.plan.inner.store.push_diagnostic(
                    "cache_write_failed",
                    "cache",
                    format!("stage=finish_flush code={}", io_error_code(&error)),
                );
                self.abandon();
                return;
            }
            if let Err(error) = file.sync_data() {
                self.plan.inner.store.push_diagnostic(
                    "cache_write_failed",
                    "cache",
                    format!("stage=finish_sync code={}", io_error_code(&error)),
                );
                self.abandon();
                return;
            }
        }
        self.plan.inner.entry.lock().unwrap().byte_len = self.written;
        if let Err(error) = fs::rename(&self.plan.inner.part_path, &self.plan.inner.staged_path) {
            self.plan.inner.store.push_diagnostic(
                "cache_write_failed",
                "cache",
                format!("stage=finish_rename code={}", io_error_code(&error)),
            );
            self.abandon();
            return;
        }
        self.plan.inner.staged.store(true, Ordering::Release);
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

impl Drop for CacheWritePlanInner {
    fn drop(&mut self) {
        self.store.release_writer(&self.id, self.writer_token);
        let _ = fs::remove_file(&self.part_path);
        let _ = fs::remove_file(&self.staged_path);
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

fn io_error_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::BrokenPipe => "broken_pipe",
        io::ErrorKind::StorageFull => "storage_full",
        _ => "io_error",
    }
}

fn anyhow_io_error_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<io::Error>()
        .map_or("io_error", io_error_code)
}

fn entry_file_error_code(entry: &CacheEntry) -> Option<&'static str> {
    File::open(&entry.audio_path)
        .err()
        .or_else(|| File::open(&entry.sidecar_path).err())
        .as_ref()
        .map(io_error_code)
}

fn dedicated_cache_root(selected_directory: &Path) -> PathBuf {
    selected_directory.join(CACHE_DIRECTORY_NAME)
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
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let temporary_extension = path.extension().and_then(|value| value.to_str());
        if name.starts_with(CACHE_FILE_PREFIX)
            && matches!(temporary_extension, Some("part" | "ready" | "gxpart"))
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn load_manifest(root: &Path) -> (Manifest, Vec<CacheDiagnostic>) {
    let path = root.join(MANIFEST_FILE_NAME);
    let backup = json_backup_path(&path);
    let legacy = root.join("manifest.json");
    let (mut manifest, primary_valid): (Manifest, bool) = match read_json(&path) {
        Ok(manifest) => (manifest, true),
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                quarantine_corrupt_json(&path);
            }
            (
                read_json(&backup)
                    .or_else(|_| read_json(&legacy))
                    .unwrap_or_default(),
                false,
            )
        }
    };
    if !primary_valid {
        for (id, entry) in recover_manifest_from_sidecars(root).entries {
            manifest.entries.entry(id).or_insert(entry);
        }
    }
    let mut diagnostics = Vec::new();
    let entries = manifest
        .entries
        .into_iter()
        .filter(|(_, entry)| {
            let in_root =
                entry.audio_path.starts_with(root) && entry.sidecar_path.starts_with(root);
            if !in_root {
                diagnostics.push(CacheDiagnostic {
                    category: "cache_read_failed",
                    source: "cache",
                    summary: "stage=manifest_reconcile code=outside_root".into(),
                });
            }
            in_root
        })
        .collect();
    (Manifest { entries }, diagnostics)
}

fn persist_settings(state: &CacheState) -> Result<()> {
    write_json_atomic(&state.settings_path, &state.settings)
}

fn persist_manifest(state: &CacheState) -> Result<()> {
    write_json_atomic(&state.root.join(MANIFEST_FILE_NAME), &state.manifest)
}

fn write_sidecar(entry: &CacheEntry) -> Result<()> {
    write_json_atomic(&entry.sidecar_path, entry)
}

fn remove_entry_files(entry: &CacheEntry) -> bool {
    match fs::remove_file(&entry.audio_path) {
        Ok(()) => {
            let _ = fs::remove_file(&entry.sidecar_path);
            let _ = fs::remove_file(json_backup_path(&entry.sidecar_path));
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let _ = fs::remove_file(&entry.sidecar_path);
            let _ = fs::remove_file(json_backup_path(&entry.sidecar_path));
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
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("gx-cache.json");
    let temporary = parent.join(format!(
        "{CACHE_FILE_PREFIX}{name}.{}.{}.{}.gxpart",
        std::process::id(),
        now_ms(),
        JSON_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.flush()?;
    file.sync_data()?;
    drop(file);

    let backup = json_backup_path(path);
    if path.exists() {
        let _ = fs::remove_file(&backup);
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn json_backup_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.bak",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ))
}

fn quarantine_corrupt_json(path: &Path) {
    if path.exists() {
        let quarantined = path.with_extension(format!("corrupt-{}.json", now_ms()));
        let _ = fs::rename(path, quarantined);
    }
}

fn recover_manifest_from_sidecars(root: &Path) -> Manifest {
    let mut manifest = Manifest::default();
    let Ok(entries) = fs::read_dir(root) else {
        return manifest;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !name.starts_with(CACHE_FILE_PREFIX)
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let Ok(entry) = read_json::<CacheEntry>(&path) else {
            continue;
        };
        if entry.audio_path.starts_with(root)
            && entry.sidecar_path == path
            && entry.audio_path.is_file()
        {
            manifest.entries.insert(cache_id(&entry.key), entry);
        }
    }
    manifest
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
        assert!(store.lookup(&first_key).is_none());
        plan.commit().unwrap();
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
    fn startup_uses_manifest_before_background_file_validation() {
        let root = temporary_root();
        let cache_key = key("fast-manifest", "320k");
        let store = CacheStore::open(&root, None).unwrap();
        let plan = store.prepare(cache_key.clone(), MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        writer.append(b"cached audio");
        writer.finish(Some(12));
        plan.commit().unwrap();
        let audio_path = store.lookup(&cache_key).unwrap().audio_path;
        drop(store);
        fs::remove_file(audio_path).unwrap();

        let reopened = CacheStore::open(&root, None).unwrap();
        assert_eq!(reopened.status().entry_count, 1);
        assert_eq!(reopened.list_entries().len(), 1);
        let revision = reopened.revision();
        reopened.deep_validate().unwrap();
        assert_eq!(reopened.status().entry_count, 0);
        assert!(reopened.revision() > revision);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_commit_eventually_publishes_without_caller_disk_work() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let cache_key = key("background-commit", "320k");
        let plan = store.prepare(cache_key.clone(), MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        writer.append(b"audio");
        writer.finish(Some(5));
        let revision = store.revision();
        plan.commit_in_background();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while store.lookup(&cache_key).is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(store.lookup(&cache_key).is_some());
        assert!(store.revision() > revision);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stable_track_lookup_prefers_requested_quality_then_falls_back() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let low = key("stable-track", "128k");
        let high = key("stable-track", "flac");
        write_entry(&store, low.clone(), 8);
        write_entry(&store, high.clone(), 16);

        let preferred = store
            .lookup_track(&high.provider_id, &high.provider_track_id, Some("128k"))
            .unwrap();
        assert_eq!(preferred.key.quality, "128k");
        store.remove_entry(&low).unwrap();
        let fallback = store
            .lookup_track(&high.provider_id, &high.provider_track_id, Some("128k"))
            .unwrap();
        assert_eq!(fallback.key.quality, "flac");
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
        let diagnostics = store.drain_diagnostics();
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.category == "cache_write_failed"
                && diagnostic.summary.starts_with("stage=append code=")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_manifest_file_is_reported_without_exposing_its_path() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let cache_key = key("missing-file", "320k");
        assert!(store.lookup(&cache_key).is_none());
        assert!(store.drain_diagnostics().is_empty());
        write_entry(&store, cache_key.clone(), 32);
        let hit = store.lookup(&cache_key).unwrap();
        store.drain_diagnostics();
        fs::remove_file(&hit.audio_path).unwrap();

        assert!(store.lookup(&cache_key).is_none());
        let diagnostics = store.drain_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].category, "cache_read_failed");
        assert_eq!(diagnostics[0].summary, "stage=lookup code=not_found");
        assert!(!diagnostics[0].summary.contains(root.to_str().unwrap()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diagnostic_queue_drops_oldest_entries_at_its_fixed_limit() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        for index in 0..(DIAGNOSTIC_CAPACITY + 7) {
            store.push_diagnostic(
                "cache_write_failed",
                "cache",
                format!("stage=test code={index}"),
            );
        }
        let diagnostics = store.drain_diagnostics();
        assert_eq!(diagnostics.len(), DIAGNOSTIC_CAPACITY);
        assert_eq!(diagnostics[0].summary, "stage=test code=7");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn favorite_sidecar_write_failure_is_reported() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let cache_key = key("favorite-sidecar-failure", "320k");
        write_entry(&store, cache_key.clone(), 32);
        store.drain_diagnostics();

        let blocked_parent = root.join("blocked-sidecar-parent");
        fs::write(&blocked_parent, b"not a directory").unwrap();
        {
            let mut state = store.inner.lock().unwrap();
            let entry = state
                .manifest
                .entries
                .get_mut(&cache_id(&cache_key))
                .unwrap();
            entry.sidecar_path = blocked_parent.join("entry.json");
        }

        store
            .set_online_favorite(
                &cache_key.provider_id,
                &cache_key.provider_track_id,
                Some(serde_json::json!({ "title": "Favorite" })),
                true,
            )
            .unwrap();
        let diagnostics = store.drain_diagnostics();
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.category == "cache_write_failed"
                && diagnostic
                    .summary
                    .starts_with("stage=favorite_sidecar code=")
        }));
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
        plan.commit().unwrap();

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

    #[test]
    fn custom_directory_uses_product_subdirectory_and_preserves_unrelated_parts() {
        let app_data = temporary_root();
        let selected = temporary_root().with_extension("selected");
        fs::create_dir_all(&selected).unwrap();
        let unrelated = selected.join("another-download.part");
        fs::write(&unrelated, b"do not delete").unwrap();

        let store = CacheStore::open(&app_data, None).unwrap();
        let status = store.set_directory(&selected).unwrap();
        assert_eq!(status.directory, selected.join(CACHE_DIRECTORY_NAME));
        assert_eq!(status.custom_directory, Some(selected.clone()));
        assert!(unrelated.is_file());
        assert!(status.directory.is_dir());

        fs::remove_dir_all(app_data).unwrap();
        fs::remove_dir_all(selected).unwrap();
    }

    #[test]
    fn directory_epoch_invalidates_in_flight_writer() {
        let app_data = temporary_root();
        let selected = temporary_root().with_extension("new-root");
        fs::create_dir_all(&selected).unwrap();
        let store = CacheStore::open(&app_data, None).unwrap();
        let cache_key = key("moving", "320k");
        let plan = store.prepare(cache_key.clone(), MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        writer.append(b"before move");

        store.set_directory(&selected).unwrap();
        writer.append(b"after move");
        writer.finish(None);
        assert!(plan.commit().is_err());
        assert!(store.lookup(&cache_key).is_none());

        fs::remove_dir_all(app_data).unwrap();
        fs::remove_dir_all(selected).unwrap();
    }

    #[test]
    fn corrupt_manifest_recovers_from_namespaced_sidecars() {
        let root = temporary_root();
        let store = CacheStore::open(&root, None).unwrap();
        let cache_key = key("recover", "flac");
        write_entry(&store, cache_key.clone(), 32);
        let cache_root = store.status().directory;
        fs::write(cache_root.join(MANIFEST_FILE_NAME), b"{not-json").unwrap();
        drop(store);

        let reopened = CacheStore::open(&root, None).unwrap();
        assert!(reopened.lookup(&cache_key).is_some());
        assert!(
            fs::read_dir(&cache_root)
                .unwrap()
                .flatten()
                .any(|entry| { entry.file_name().to_string_lossy().contains("corrupt-") })
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn write_entry(store: &CacheStore, key: CacheKey, size: usize) {
        let plan = store.prepare(key, MediaType::Mp3);
        let mut writer = plan.begin().unwrap();
        let bytes = vec![1; size];
        writer.append(&bytes);
        writer.finish(Some(size as u64));
        plan.commit().unwrap();
    }

    fn key(track: &str, quality: &str) -> CacheKey {
        CacheKey {
            provider_id: "kg".into(),
            provider_track_id: track.into(),
            quality: quality.into(),
        }
    }

    fn temporary_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "gx-cache-test-{}-{nanos}-{sequence}",
            std::process::id()
        ))
    }
}
