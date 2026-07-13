use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use gx_contracts::{HttpHeader, MediaType, NetworkRoute, ResolvedMediaRequest};
use gx_source::network_policy::source_route_attempts;
use gx_source::{
    ManagedSource, ScriptMetadata, SourceBackup, SourceFallbackConfig, SourceHealthSummary,
    SourceStore, SourceStoreError,
};
use serde::Serialize;
use serde_json::Value;
use url::Url;

pub const MAX_RUNTIME_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSource {
    pub id: String,
    pub script_path: std::path::PathBuf,
    pub origin: String,
    pub imported_at_ms: u64,
    pub metadata: ScriptMetadata,
    pub updates_enabled: bool,
}

impl From<&ManagedSource> for PublicSource {
    fn from(source: &ManagedSource) -> Self {
        Self {
            id: source.id.clone(),
            script_path: source.script_path.clone(),
            origin: source.origin.clone(),
            imported_at_ms: source.imported_at_ms,
            metadata: source.metadata.clone(),
            updates_enabled: source.updates_enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedSource {
    #[serde(flatten)]
    pub source: PublicSource,
    pub active: bool,
    pub has_config: bool,
    pub capabilities: Vec<PublicSourceCapability>,
    pub health: SourceHealthSummary,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublicSourceCapability {
    pub platform: String,
    pub qualities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub generation: u64,
    pub state: RuntimeState,
    pub active_source_id: Option<String>,
    pub capabilities: Value,
    pub error: Option<String>,
    pub update_alert: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    NoSource,
    Initializing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptLaunch {
    pub generation: u64,
    pub script: String,
    pub source: ManagedSource,
}

pub struct RuntimeRequest {
    pub request_id: String,
    pub generation: u64,
    pub receiver: Receiver<Result<Value, String>>,
}

struct PendingRequest {
    generation: u64,
    sender: SyncSender<Result<Value, String>>,
}

struct RuntimeInner {
    status: RuntimeStatus,
    network_route: Option<NetworkRoute>,
    pending: HashMap<String, PendingRequest>,
}

pub struct SourceRuntime {
    store: Mutex<SourceStore>,
    inner: Mutex<RuntimeInner>,
    operation_lock: Mutex<()>,
    next_request_id: AtomicU64,
}

impl SourceRuntime {
    pub fn new(store: SourceStore) -> Self {
        Self {
            store: Mutex::new(store),
            inner: Mutex::new(RuntimeInner {
                status: RuntimeStatus {
                    generation: 0,
                    state: RuntimeState::NoSource,
                    active_source_id: None,
                    capabilities: Value::Null,
                    error: None,
                    update_alert: None,
                },
                network_route: None,
                pending: HashMap::new(),
            }),
            operation_lock: Mutex::new(()),
            next_request_id: AtomicU64::new(1),
        }
    }

    pub fn serialized<T>(&self, operation: impl FnOnce() -> T) -> T {
        let _guard = self.operation_lock.lock().unwrap();
        operation()
    }

    pub fn list(&self) -> Vec<ListedSource> {
        self.store
            .lock()
            .unwrap()
            .list()
            .into_iter()
            .map(|(source, active)| ListedSource {
                source: PublicSource::from(&source),
                active,
                has_config: source
                    .config
                    .as_object()
                    .is_some_and(|config| !config.is_empty()),
                capabilities: public_source_capabilities(&source.capabilities),
                health: source.health_summary(),
            })
            .collect()
    }

    pub fn import_script(
        &self,
        script: &str,
        origin: impl Into<String>,
        fallback_name: &str,
    ) -> Result<ManagedSource, SourceStoreError> {
        self.store
            .lock()
            .unwrap()
            .import_script(script, origin, fallback_name)
    }

    pub fn import_file(&self, path: &std::path::Path) -> Result<ManagedSource, SourceStoreError> {
        self.store.lock().unwrap().import_file(path)
    }

    pub fn activate(&self, id: &str) -> Result<(), SourceStoreError> {
        self.store.lock().unwrap().activate(id)
    }

    pub fn remove(&self, id: &str) -> Result<(), SourceStoreError> {
        self.store.lock().unwrap().remove(id)
    }

    pub fn set_updates_enabled(&self, id: &str, enabled: bool) -> Result<(), SourceStoreError> {
        self.store.lock().unwrap().set_updates_enabled(id, enabled)
    }

    pub fn config(&self, id: &str) -> Result<Value, SourceStoreError> {
        self.store.lock().unwrap().config(id)
    }

    pub fn set_config(&self, id: &str, config: Value) -> Result<(), SourceStoreError> {
        self.store.lock().unwrap().set_config(id, config)
    }

    pub fn fallback_config(&self) -> SourceFallbackConfig {
        self.store.lock().unwrap().fallback_config()
    }

    pub fn set_fallback_config(
        &self,
        enabled: bool,
        source_ids: Vec<String>,
    ) -> Result<(), SourceStoreError> {
        self.store
            .lock()
            .unwrap()
            .set_fallback_config(enabled, source_ids)
    }

    pub fn record_health_sample(
        &self,
        id: &str,
        success: bool,
        latency_ms: u64,
    ) -> Result<(), SourceStoreError> {
        self.store
            .lock()
            .unwrap()
            .record_health_sample(id, success, latency_ms)
    }

    pub fn preferred_route(&self, id: &str) -> Result<Option<NetworkRoute>, SourceStoreError> {
        self.store.lock().unwrap().preferred_route(id)
    }

    pub fn record_successful_route(
        &self,
        id: &str,
        route: NetworkRoute,
    ) -> Result<(), SourceStoreError> {
        self.store
            .lock()
            .unwrap()
            .record_successful_route(id, route)
    }

    pub fn resolution_source_ids(
        &self,
        requested_source_id: Option<&str>,
    ) -> Result<Vec<String>, SourceStoreError> {
        self.store
            .lock()
            .unwrap()
            .resolution_source_ids(requested_source_id)
    }

    pub fn export_backup(&self) -> Result<SourceBackup, SourceStoreError> {
        self.store.lock().unwrap().export_backup()
    }

    pub fn restore_backup(&self, backup: SourceBackup) -> Result<(), SourceStoreError> {
        self.store.lock().unwrap().restore_backup(backup)
    }

    pub fn record_update_alert(&self, generation: u64, alert: Value) -> Result<(), String> {
        ensure_json_size(&alert, MAX_RUNTIME_PAYLOAD_BYTES, "update alert")?;
        let source_id = {
            let inner = self.inner.lock().unwrap();
            if generation != inner.status.generation {
                return Err("stale LX update alert".into());
            }
            inner.status.active_source_id.clone()
        };
        let Some(source_id) = source_id else {
            return Ok(());
        };
        if !self
            .store
            .lock()
            .unwrap()
            .updates_enabled(&source_id)
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        let mut inner = self.inner.lock().unwrap();
        if generation != inner.status.generation {
            return Err("stale LX update alert".into());
        }
        inner.status.update_alert = Some(alert);
        Ok(())
    }

    pub fn status(&self) -> RuntimeStatus {
        self.inner.lock().unwrap().status.clone()
    }

    pub fn source_id_and_route_for_generation(
        &self,
        generation: u64,
    ) -> Result<(String, NetworkRoute), String> {
        let inner = self.inner.lock().unwrap();
        if generation != inner.status.generation {
            return Err("stale LX HTTP request generation".into());
        }
        let source_id = inner
            .status
            .active_source_id
            .clone()
            .ok_or_else(|| "LX runtime has no active source".to_owned())?;
        let route = inner
            .network_route
            .ok_or_else(|| "LX runtime has no active network route".to_owned())?;
        Ok((source_id, route))
    }

    pub fn prepare_reload(&self) -> Result<Option<ScriptLaunch>, SourceStoreError> {
        let active = self.store.lock().unwrap().active_script()?;
        let mut inner = self.inner.lock().unwrap();
        inner.status.generation = inner.status.generation.wrapping_add(1);
        let generation = inner.status.generation;
        reject_all_pending(
            &mut inner,
            "LX runtime reloaded before the request completed",
        );
        inner.status.capabilities = Value::Null;
        inner.status.error = None;
        inner.status.update_alert = None;
        let Some((source, script)) = active else {
            inner.status.state = RuntimeState::NoSource;
            inner.status.active_source_id = None;
            inner.network_route = None;
            return Ok(None);
        };
        let route = source_route_attempts(source.last_successful_route)
            .into_iter()
            .next()
            .unwrap_or(NetworkRoute::Direct);
        inner.status.state = RuntimeState::Initializing;
        inner.status.active_source_id = Some(source.id.clone());
        inner.network_route = Some(route);
        Ok(Some(ScriptLaunch {
            generation,
            script,
            source,
        }))
    }

    pub fn prepare_reload_for_route(
        &self,
        id: &str,
        route: NetworkRoute,
    ) -> Result<ScriptLaunch, SourceStoreError> {
        let (source, script) = self.store.lock().unwrap().script_by_id(id)?;
        self.prepare_launch_for_route(source, script, route)
    }

    fn prepare_launch_for_route(
        &self,
        source: ManagedSource,
        script: String,
        route: NetworkRoute,
    ) -> Result<ScriptLaunch, SourceStoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.status.generation = inner.status.generation.wrapping_add(1);
        let generation = inner.status.generation;
        reject_all_pending(
            &mut inner,
            "LX runtime switched source before the request completed",
        );
        inner.status.state = RuntimeState::Initializing;
        inner.status.active_source_id = Some(source.id.clone());
        inner.network_route = Some(route);
        inner.status.capabilities = Value::Null;
        inner.status.error = None;
        inner.status.update_alert = None;
        Ok(ScriptLaunch {
            generation,
            script,
            source,
        })
    }

    pub fn mark_ready(&self, generation: u64, capabilities: Value) -> Result<String, String> {
        ensure_json_size(
            &capabilities,
            MAX_RUNTIME_PAYLOAD_BYTES,
            "runtime capabilities",
        )?;
        let mut inner = self.inner.lock().unwrap();
        if generation != inner.status.generation {
            return Err("stale LX runtime initialization event".into());
        }
        if inner.status.state != RuntimeState::Initializing {
            return Err("LX runtime is not initializing".into());
        }
        let source_id = inner
            .status
            .active_source_id
            .clone()
            .ok_or_else(|| "LX runtime has no initializing source".to_owned())?;
        inner.status.state = RuntimeState::Ready;
        inner.status.capabilities = capabilities.clone();
        inner.status.error = None;
        drop(inner);
        if let Err(error) = self
            .store
            .lock()
            .unwrap()
            .set_capabilities(&source_id, capabilities)
        {
            eprintln!("failed to persist LX runtime capabilities for {source_id}: {error}");
        }
        Ok(source_id)
    }

    pub fn mark_failed(&self, generation: u64, error: String) {
        let mut inner = self.inner.lock().unwrap();
        if generation != inner.status.generation {
            return;
        }
        inner.status.state = RuntimeState::Failed;
        inner.status.error = Some(error.clone());
        reject_all_pending(&mut inner, &error);
    }

    pub fn fail_current(&self, error: String) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let had_pending = !inner.pending.is_empty();
        inner.status.state = RuntimeState::Failed;
        inner.status.error = Some(error.clone());
        reject_all_pending(&mut inner, &error);
        had_pending
    }

    pub fn fail_if_initializing(&self, generation: u64, error: String) {
        let mut inner = self.inner.lock().unwrap();
        if inner.status.generation == generation && inner.status.state == RuntimeState::Initializing
        {
            inner.status.state = RuntimeState::Failed;
            inner.status.error = Some(error.clone());
            reject_all_pending(&mut inner, &error);
        }
    }

    pub fn fail_if_not_started(&self, generation: u64, error: String) {
        if self.store.lock().unwrap().list().is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.status.generation == generation
            && inner.status.state != RuntimeState::Ready
            && inner.status.state != RuntimeState::Initializing
        {
            inner.status.state = RuntimeState::Failed;
            inner.status.error = Some(error.clone());
            reject_all_pending(&mut inner, &error);
        }
    }

    pub fn begin_request(&self, payload: &Value) -> Result<RuntimeRequest, String> {
        ensure_json_size(payload, MAX_RUNTIME_PAYLOAD_BYTES, "resolver payload")?;
        if payload.get("action").and_then(Value::as_str) != Some("musicUrl") {
            return Err("LX runtime only accepts the 'musicUrl' action".into());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.status.state != RuntimeState::Ready {
            return Err(format!(
                "LX runtime is not ready (state: {:?})",
                inner.status.state
            ));
        }
        let generation = inner.status.generation;
        let sequence = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let request_id = format!("{generation}-{sequence}");
        let (sender, receiver) = sync_channel(1);
        inner
            .pending
            .insert(request_id.clone(), PendingRequest { generation, sender });
        Ok(RuntimeRequest {
            request_id,
            generation,
            receiver,
        })
    }

    pub fn cancel_request(&self, request_id: &str, reason: &str) {
        if let Some(pending) = self.inner.lock().unwrap().pending.remove(request_id) {
            let _ = pending.sender.send(Err(reason.into()));
        }
    }

    pub fn complete_request(
        &self,
        request_id: &str,
        generation: u64,
        result: Result<Value, String>,
    ) -> Result<(), String> {
        if let Ok(value) = &result {
            ensure_json_size(value, MAX_RUNTIME_PAYLOAD_BYTES, "resolver result")?;
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(pending) = inner.pending.remove(request_id) else {
            return Err("unknown or expired LX runtime request".into());
        };
        if generation != pending.generation || generation != inner.status.generation {
            let _ = pending.sender.send(Err("stale LX runtime response".into()));
            return Err("stale LX runtime response".into());
        }
        pending
            .sender
            .send(result)
            .map_err(|_| "LX runtime requester was dropped".to_owned())
    }
}

fn public_source_capabilities(capabilities: &Value) -> Vec<PublicSourceCapability> {
    let Some(sources) = capabilities.get("sources").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut result = sources
        .iter()
        .filter_map(|(platform, details)| {
            let platform = public_capability_label(platform)?;
            let values = details
                .get("qualitys")
                .or_else(|| details.get("qualities"))
                .and_then(Value::as_array);
            let mut qualities = Vec::new();
            for quality in values.into_iter().flatten().filter_map(Value::as_str) {
                let Some(quality) = public_capability_label(quality) else {
                    continue;
                };
                if !qualities.contains(&quality) {
                    qualities.push(quality);
                }
                if qualities.len() == 32 {
                    break;
                }
            }
            Some(PublicSourceCapability {
                platform,
                qualities,
            })
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| left.platform.cmp(&right.platform));
    result
}

fn public_capability_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 64 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

pub fn normalize_media_request(
    raw: Value,
    requested_quality: Option<&str>,
) -> Result<ResolvedMediaRequest, String> {
    ensure_json_size(&raw, MAX_RUNTIME_PAYLOAD_BYTES, "resolver result")?;
    let (url_text, headers, media_type, quality, expires_at_ms) = match raw {
        Value::String(url) => (url, Vec::new(), MediaType::Unknown, None, None),
        Value::Object(mut object) => {
            let url = object
                .remove("url")
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| "resolver result must contain a string URL".to_owned())?;
            let headers = parse_media_headers(object.remove("headers"))?;
            let type_text = object
                .remove("type")
                .and_then(|value| value.as_str().map(str::to_owned));
            let media_type = type_text
                .as_deref()
                .map(parse_media_type)
                .unwrap_or_else(|| infer_media_type(&url));
            let quality = object
                .remove("quality")
                .and_then(|value| value.as_str().map(str::to_owned))
                .or(type_text);
            let expires_at_ms = object
                .remove("expiresAtMs")
                .or_else(|| object.remove("expires_at_ms"))
                .and_then(|value| value.as_u64());
            (url, headers, media_type, quality, expires_at_ms)
        }
        _ => return Err("resolver result must be a URL string or object".into()),
    };
    let url =
        Url::parse(&url_text).map_err(|error| format!("invalid resolved media URL: {error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("resolved media URL must use HTTP(S)".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in resolved media URLs are not allowed".into());
    }
    Ok(ResolvedMediaRequest {
        url,
        headers,
        media_type,
        quality: quality.or_else(|| requested_quality.map(str::to_owned)),
        expires_at_ms,
        network_route: None,
    })
}

fn parse_media_headers(value: Option<Value>) -> Result<Vec<HttpHeader>, String> {
    let Some(Value::Object(headers)) = value else {
        return Ok(Vec::new());
    };
    if headers.len() > 64 {
        return Err("resolved media request has too many headers".into());
    }
    headers
        .into_iter()
        .map(|(name, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| "resolved media header values must be strings".to_owned())?;
            if name.len() > 256 || value.len() > 8192 {
                return Err("resolved media header exceeds the size limit".into());
            }
            Ok(HttpHeader {
                name,
                value: value.to_owned(),
            })
        })
        .collect()
}

fn parse_media_type(value: &str) -> MediaType {
    match value.to_ascii_lowercase().as_str() {
        "mp3" | "128k" | "320k" => MediaType::Mp3,
        "flac" | "flac24bit" => MediaType::Flac,
        "aac" | "m4a" => MediaType::Aac,
        "ogg" | "vorbis" => MediaType::Ogg,
        "wav" => MediaType::Wav,
        "hls" | "m3u8" => MediaType::Hls,
        _ => MediaType::Unknown,
    }
}

fn infer_media_type(url: &str) -> MediaType {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if path.ends_with(".mp3") {
        MediaType::Mp3
    } else if path.ends_with(".flac") {
        MediaType::Flac
    } else if path.ends_with(".aac") || path.ends_with(".m4a") {
        MediaType::Aac
    } else if path.ends_with(".ogg") {
        MediaType::Ogg
    } else if path.ends_with(".wav") {
        MediaType::Wav
    } else if path.ends_with(".m3u8") {
        MediaType::Hls
    } else {
        MediaType::Unknown
    }
}

pub(crate) fn ensure_json_size(value: &Value, limit: usize, label: &str) -> Result<(), String> {
    let size = serde_json::to_vec(value)
        .map_err(|error| format!("failed to serialize {label}: {error}"))?
        .len();
    if size > limit {
        Err(format!("{label} exceeds the {limit}-byte limit"))
    } else {
        Ok(())
    }
}

fn reject_all_pending(inner: &mut RuntimeInner, reason: &str) {
    for (_, pending) in inner.pending.drain() {
        let _ = pending.sender.send(Err(reason.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn runtime() -> (SourceRuntime, std::path::PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gx-runtime-test-{nanos}"));
        let store = SourceStore::open(&root).unwrap();
        (SourceRuntime::new(store), root)
    }

    #[test]
    fn reload_rejects_pending_and_stale_responses() {
        let (runtime, root) = runtime();
        runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        let launch = runtime.prepare_reload().unwrap().unwrap();
        runtime.mark_ready(launch.generation, Value::Null).unwrap();
        let pending = runtime
            .begin_request(&serde_json::json!({"action":"musicUrl"}))
            .unwrap();
        assert!(runtime.fail_current("sandbox crashed".into()));
        assert!(pending.receiver.recv().unwrap().is_err());
        runtime.prepare_reload().unwrap();
        runtime
            .mark_ready(runtime.status().generation, Value::Null)
            .unwrap();
        let pending = runtime
            .begin_request(&serde_json::json!({"action":"musicUrl"}))
            .unwrap();
        runtime.prepare_reload().unwrap();
        assert!(pending.receiver.recv().unwrap().is_err());
        assert!(
            runtime
                .complete_request(&pending.request_id, pending.generation, Ok(Value::Null))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn correlates_responses_and_normalizes_structured_media() {
        let (runtime, root) = runtime();
        runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        let launch = runtime.prepare_reload().unwrap().unwrap();
        runtime
            .mark_ready(
                launch.generation,
                serde_json::json!({
                    "sources": { "alpha": { "qualitys": ["standard"] } }
                }),
            )
            .unwrap();
        let pending = runtime
            .begin_request(&serde_json::json!({"action":"musicUrl","id":1}))
            .unwrap();
        runtime
            .complete_request(
                &pending.request_id,
                pending.generation,
                Ok(serde_json::json!({
                    "url":"https://media.example/song.flac?token=secret",
                    "headers":{"Referer":"https://example.com"},
                    "type":"flac",
                    "quality":"lossless",
                    "expiresAtMs":123
                })),
            )
            .unwrap();
        let resolved =
            normalize_media_request(pending.receiver.recv().unwrap().unwrap(), None).unwrap();
        assert_eq!(resolved.media_type, MediaType::Flac);
        assert_eq!(resolved.quality.as_deref(), Some("lossless"));
        assert_eq!(resolved.headers.len(), 1);
        assert_eq!(resolved.expires_at_ms, Some(123));
        assert!(!resolved.redacted_for_log().contains("secret"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reported_capabilities_are_scoped_sanitized_and_persisted() {
        let (runtime, root) = runtime();
        let source = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        let launch = runtime.prepare_reload().unwrap().unwrap();
        let source_id = runtime
            .mark_ready(
                launch.generation,
                serde_json::json!({
                    "sources": {
                        " beta ": { "qualities": ["high", "high", 1] },
                        "alpha": { "qualitys": ["standard"] },
                        "\n": { "qualitys": ["hidden"] },
                        "gamma": { "qualitys": ["\u{0000}"] }
                    }
                }),
            )
            .unwrap();
        assert_eq!(source_id, source.id);

        let listed = runtime.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].capabilities,
            vec![
                PublicSourceCapability {
                    platform: "alpha".into(),
                    qualities: vec!["standard".into()],
                },
                PublicSourceCapability {
                    platform: "beta".into(),
                    qualities: vec!["high".into()],
                },
                PublicSourceCapability {
                    platform: "gamma".into(),
                    qualities: Vec::new(),
                },
            ]
        );

        drop(runtime);
        let reopened = SourceRuntime::new(SourceStore::open(&root).unwrap());
        assert_eq!(reopened.list()[0].capabilities, listed[0].capabilities);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_list_exposes_health_summary_without_raw_samples() {
        let (runtime, root) = runtime();
        let source = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        for latency_ms in [800, 1_000, 1_200] {
            runtime
                .record_health_sample(&source.id, true, latency_ms)
                .unwrap();
        }

        let listed = runtime.list();
        assert_eq!(listed[0].health.sample_count, 3);
        assert_eq!(listed[0].health.success_rate_percent, Some(100));
        let public = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(public["health"]["state"], "healthy");
        assert!(public.get("healthSamples").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn temporary_launch_does_not_change_persisted_active_source() {
        let (runtime, root) = runtime();
        let first = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test:a",
                "a",
            )
            .unwrap();
        let second = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/b.mp3')",
                "test:b",
                "b",
            )
            .unwrap();
        let temporary = runtime
            .prepare_reload_for_route(&second.id, NetworkRoute::Direct)
            .unwrap();
        assert_eq!(temporary.source.id, second.id);
        let restored = runtime.prepare_reload().unwrap().unwrap();
        assert_eq!(restored.source.id, first.id);
        assert!(
            runtime
                .begin_request(&serde_json::json!({"action":"lyric"}))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_routes_are_bound_to_the_runtime_generation() {
        let (runtime, root) = runtime();
        let source = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        let direct = runtime
            .prepare_reload_for_route(&source.id, NetworkRoute::Direct)
            .unwrap();
        assert_eq!(
            runtime
                .source_id_and_route_for_generation(direct.generation)
                .unwrap(),
            (source.id.clone(), NetworkRoute::Direct)
        );

        let proxied = runtime
            .prepare_reload_for_route(&source.id, NetworkRoute::SystemProxy)
            .unwrap();
        assert!(
            runtime
                .source_id_and_route_for_generation(direct.generation)
                .is_err()
        );
        assert_eq!(
            runtime
                .source_id_and_route_for_generation(proxied.generation)
                .unwrap(),
            (source.id, NetworkRoute::SystemProxy)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_reaches_launch_but_is_redacted_from_source_list() {
        let (runtime, root) = runtime();
        let source = runtime
            .import_script(
                "lx.on('request', () => 'https://example.com/a.mp3')",
                "test",
                "a",
            )
            .unwrap();
        runtime
            .set_config(
                &source.id,
                serde_json::json!({ "api": { "pass": "secret" } }),
            )
            .unwrap();

        let listed = serde_json::to_string(&runtime.list()).unwrap();
        assert!(!listed.contains("secret"));
        assert!(!listed.contains("config"));
        assert!(listed.contains("hasConfig"));
        let launch = runtime.prepare_reload().unwrap().unwrap();
        assert_eq!(launch.source.config["api"]["pass"], "secret");
        std::fs::remove_dir_all(root).unwrap();
    }
}
