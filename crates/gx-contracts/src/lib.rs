//! Stable data contracts shared by the headless core, Tauri shell, and source adapters.
//!
//! Source-specific objects (especially LX script objects) must remain opaque to this crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Mp3,
    Flac,
    Aac,
    Ogg,
    Wav,
    Hls,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMediaRequest {
    pub url: Url,
    pub headers: Vec<HttpHeader>,
    pub media_type: MediaType,
    pub quality: Option<String>,
    /// Unix timestamp in milliseconds.
    pub expires_at_ms: Option<u64>,
}

impl ResolvedMediaRequest {
    pub fn is_expired_at(&self, unix_time_ms: u64) -> bool {
        self.expires_at_ms
            .is_some_and(|expires_at| unix_time_ms >= expires_at)
    }

    /// Produces a safe diagnostic description without leaking query tokens or header values.
    pub fn redacted_for_log(&self) -> String {
        let host = self.url.host_str().unwrap_or("<no-host>");
        format!(
            "{}://{}{} [{} headers, {:?}]",
            self.url.scheme(),
            host,
            self.url.path(),
            self.headers.len(),
            self.media_type
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnlineRef {
    pub provider_id: String,
    pub source_id: Option<String>,
    /// Provider-owned resolver input. The playback core stores and forwards it but never
    /// interprets its fields.
    pub resolver_payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackSource {
    Local { path: PathBuf },
    Online { reference: OnlineRef },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub cover_url: Option<Url>,
    pub source: TrackSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Idle,
    Loading,
    Playing,
    Paused,
    Buffering,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackSnapshot {
    pub status: PlaybackStatus,
    pub current_track_id: Option<Uuid>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub queue_index: Option<usize>,
    /// Monotonic generation used to discard stale UI/network events after a track change.
    pub generation: u64,
    pub error: Option<String>,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            current_track_id: None,
            position_ms: 0,
            duration_ms: None,
            queue_index: None,
            generation: 0,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_boundary_is_inclusive() {
        let request = ResolvedMediaRequest {
            url: Url::parse("https://media.example/song.mp3?token=secret").unwrap(),
            headers: vec![HttpHeader {
                name: "Authorization".into(),
                value: "secret".into(),
            }],
            media_type: MediaType::Mp3,
            quality: Some("320k".into()),
            expires_at_ms: Some(10_000),
        };

        assert!(!request.is_expired_at(9_999));
        assert!(request.is_expired_at(10_000));
    }

    #[test]
    fn diagnostics_redact_credentials() {
        let request = ResolvedMediaRequest {
            url: Url::parse("https://media.example/song.mp3?token=secret").unwrap(),
            headers: vec![HttpHeader {
                name: "Authorization".into(),
                value: "secret".into(),
            }],
            media_type: MediaType::Mp3,
            quality: None,
            expires_at_ms: None,
        };

        let diagnostic = request.redacted_for_log();
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("Authorization"));
        assert!(diagnostic.contains("media.example/song.mp3"));
    }
}
