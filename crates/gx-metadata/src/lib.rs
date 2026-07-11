use std::cmp::Ordering;
use std::time::Duration;

use gx_contracts::{MediaType, ResolvedMediaRequest};
use gx_source::safe_http::{SafeHttpRequest, execute};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const PREVIEW_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTrack {
    pub provider_id: String,
    pub provider_track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub artwork_url: Option<Url>,
    pub resolver_payload: Value,
    pub preview: Option<ResolvedMediaRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub timestamp_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricDocument {
    pub instrumental: bool,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementMatch {
    pub score: f32,
    pub track: CatalogTrack,
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("metadata query is empty")]
    EmptyQuery,
    #[error("metadata HTTP failed: {0}")]
    Http(String),
    #[error("metadata service returned HTTP {0}")]
    HttpStatus(u16),
    #[error("metadata response JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("metadata URL failed: {0}")]
    Url(#[from] url::ParseError),
}

pub fn search_all(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MetadataError::EmptyQuery);
    }
    let limit = limit.clamp(1, 50);
    let (itunes, deezer) = std::thread::scope(|scope| {
        let itunes = scope.spawn(|| search_itunes(query, limit));
        let deezer = scope.spawn(|| search_deezer(query, limit));
        (itunes.join().unwrap(), deezer.join().unwrap())
    });
    let mut tracks = Vec::new();
    let mut errors = Vec::new();
    for result in [itunes, deezer] {
        match result {
            Ok(mut found) => tracks.append(&mut found),
            Err(error) => errors.push(error.to_string()),
        }
    }
    if tracks.is_empty() && !errors.is_empty() {
        return Err(MetadataError::Http(errors.join("; ")));
    }
    Ok(tracks)
}

pub fn search_itunes(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let mut url = Url::parse("https://itunes.apple.com/search")?;
    url.query_pairs_mut()
        .append_pair("term", query)
        .append_pair("entity", "song")
        .append_pair("country", "CN")
        .append_pair("limit", &limit.clamp(1, 50).to_string());
    let response: ItunesResponse = request_json(url)?;
    Ok(response
        .results
        .into_iter()
        .filter_map(ItunesTrack::into_catalog)
        .collect())
}

pub fn search_deezer(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let mut url = Url::parse("https://api.deezer.com/search")?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &limit.clamp(1, 50).to_string());
    let response: DeezerResponse = request_json(url)?;
    Ok(response
        .data
        .into_iter()
        .filter_map(DeezerTrack::into_catalog)
        .collect())
}

pub fn apple_chart(limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let limit = limit.clamp(1, 100);
    let url = Url::parse(&format!(
        "https://rss.marketingtools.apple.com/api/v2/cn/music/most-played/{limit}/songs.json"
    ))?;
    let response: AppleChartResponse = request_json(url)?;
    Ok(response
        .feed
        .results
        .into_iter()
        .map(AppleChartTrack::into_catalog)
        .collect())
}

pub fn fetch_lyrics(
    title: &str,
    artist: &str,
    duration_ms: Option<u64>,
) -> Result<Option<LyricDocument>, MetadataError> {
    let mut url = Url::parse("https://lrclib.net/api/search")?;
    url.query_pairs_mut()
        .append_pair("track_name", title)
        .append_pair("artist_name", artist);
    let candidates: Vec<LrcLibItem> = request_json(url)?;
    let selected = candidates.into_iter().min_by_key(|item| {
        duration_ms.map_or(0, |target| {
            target.abs_diff((item.duration.max(0.0) * 1000.0) as u64)
        })
    });
    Ok(selected.map(|item| {
        if let Some(synced) = item.synced_lyrics.filter(|text| !text.trim().is_empty()) {
            parse_lrc(&synced)
        } else {
            LyricDocument {
                instrumental: item.instrumental,
                lines: item
                    .plain_lyrics
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| LyricLine {
                        timestamp_ms: None,
                        text: line.trim().to_owned(),
                    })
                    .collect(),
            }
        }
    }))
}

pub fn parse_lrc(input: &str) -> LyricDocument {
    let mut lines = Vec::new();
    for raw_line in input.lines() {
        let mut rest = raw_line.trim();
        let mut timestamps = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            let Some(timestamp) = parse_lrc_timestamp(tag) else {
                break;
            };
            timestamps.push(timestamp);
            rest = &stripped[end + 1..];
        }
        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        for timestamp_ms in timestamps {
            lines.push(LyricLine {
                timestamp_ms: Some(timestamp_ms),
                text: text.to_owned(),
            });
        }
    }
    lines.sort_by_key(|line| line.timestamp_ms);
    LyricDocument {
        instrumental: false,
        lines,
    }
}

pub fn find_replacements(
    wanted: &CatalogTrack,
    candidates: impl IntoIterator<Item = CatalogTrack>,
) -> Vec<ReplacementMatch> {
    let wanted_title = normalize(&wanted.title);
    let wanted_artist = normalize(&wanted.artist);
    let wanted_album = normalize(&wanted.album);
    let mut matches = candidates
        .into_iter()
        .filter(|candidate| candidate.provider_id != wanted.provider_id)
        .filter_map(|candidate| {
            let title = similarity(&wanted_title, &normalize(&candidate.title));
            let artist = similarity(&wanted_artist, &normalize(&candidate.artist));
            if title < 0.72 || artist < 0.45 {
                return None;
            }
            let album = similarity(&wanted_album, &normalize(&candidate.album));
            let duration = duration_score(wanted.duration_ms, candidate.duration_ms);
            let score = title * 0.5 + artist * 0.3 + album * 0.08 + duration * 0.12;
            (score >= 0.70).then_some(ReplacementMatch {
                score,
                track: candidate,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
    });
    matches
}

pub fn select_playable_with(
    wanted: &CatalogTrack,
    candidates: impl IntoIterator<Item = CatalogTrack>,
    mut is_available: impl FnMut(&ResolvedMediaRequest) -> bool,
) -> Option<(CatalogTrack, Option<String>)> {
    if let Some(request) = wanted.preview.as_ref()
        && is_available(request)
    {
        return Some((wanted.clone(), None));
    }
    find_replacements(wanted, candidates)
        .into_iter()
        .find_map(|candidate| {
            let available = candidate
                .track
                .preview
                .as_ref()
                .is_some_and(&mut is_available);
            available.then(|| (candidate.track, Some(wanted.provider_id.clone())))
        })
}

pub fn preview_is_available(request: &ResolvedMediaRequest) -> bool {
    let mut headers = request
        .headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    headers.push(("range".into(), "bytes=0-1023".into()));
    execute(SafeHttpRequest {
        url: request.url.clone(),
        method: Method::GET,
        headers,
        body: None,
        timeout: Duration::from_secs(10),
        max_response_bytes: 4096,
    })
    .is_ok_and(|response| {
        (response.status == 200 || response.status == 206)
            && response.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    && value.to_ascii_lowercase().starts_with("audio/")
            })
    })
}

pub fn fetch_preview_bytes(request: &ResolvedMediaRequest) -> Result<Vec<u8>, MetadataError> {
    let response = execute(SafeHttpRequest {
        url: request.url.clone(),
        method: Method::GET,
        headers: request
            .headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect(),
        body: None,
        timeout: Duration::from_secs(20),
        max_response_bytes: PREVIEW_LIMIT,
    })
    .map_err(|error| MetadataError::Http(error.to_string()))?;
    if !(200..300).contains(&response.status) {
        return Err(MetadataError::HttpStatus(response.status));
    }
    Ok(response.body)
}

fn request_json<T: DeserializeOwned>(url: Url) -> Result<T, MetadataError> {
    let mut last_error = None;
    for attempt in 0..3 {
        let response = execute(SafeHttpRequest {
            url: url.clone(),
            method: Method::GET,
            headers: vec![("user-agent".into(), "GXPlayer/0.1 metadata".into())],
            body: None,
            timeout: Duration::from_secs(15),
            max_response_bytes: RESPONSE_LIMIT,
        });
        match response {
            Ok(response) if (200..300).contains(&response.status) => {
                return Ok(serde_json::from_slice(&response.body)?);
            }
            Ok(response) => last_error = Some(MetadataError::HttpStatus(response.status)),
            Err(error) => last_error = Some(MetadataError::Http(error.to_string())),
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(200 * (attempt + 1)));
        }
    }
    Err(last_error.unwrap_or_else(|| MetadataError::Http("request did not run".into())))
}

fn preview_request(url: Option<String>, media_type: MediaType) -> Option<ResolvedMediaRequest> {
    let url = Url::parse(url.as_deref()?).ok()?;
    Some(ResolvedMediaRequest {
        url,
        headers: Vec::new(),
        media_type,
        quality: Some("preview".into()),
        expires_at_ms: None,
    })
}

fn parse_lrc_timestamp(tag: &str) -> Option<u64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<f64>().ok()?;
    if !seconds.is_finite() || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(minutes * 60_000 + (seconds * 1000.0).round() as u64)
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn similarity(left: &str, right: &str) -> f32 {
    if left == right && !left.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left.contains(right) || right.contains(left) {
        return left.len().min(right.len()) as f32 / left.len().max(right.len()) as f32;
    }
    let common = left
        .chars()
        .filter(|character| right.contains(*character))
        .count();
    (2 * common) as f32 / (left.chars().count() + right.chars().count()) as f32
}

fn duration_score(left: Option<u64>, right: Option<u64>) -> f32 {
    match (left, right) {
        (Some(left), Some(right)) => (1.0 - left.abs_diff(right) as f32 / 15_000.0).clamp(0.0, 1.0),
        _ => 0.5,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesResponse {
    results: Vec<ItunesTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesTrack {
    track_id: Option<u64>,
    track_name: Option<String>,
    artist_name: Option<String>,
    collection_name: Option<String>,
    track_time_millis: Option<u64>,
    artwork_url100: Option<String>,
    preview_url: Option<String>,
}

impl ItunesTrack {
    fn into_catalog(self) -> Option<CatalogTrack> {
        let id = self.track_id?;
        let title = self.track_name?;
        let artist = self.artist_name?;
        Some(CatalogTrack {
            provider_id: "itunes".into(),
            provider_track_id: id.to_string(),
            title,
            artist,
            album: self.collection_name.unwrap_or_default(),
            duration_ms: self.track_time_millis,
            artwork_url: self.artwork_url100.and_then(|url| Url::parse(&url).ok()),
            resolver_payload: json!({ "provider": "itunes", "trackId": id }),
            preview: preview_request(self.preview_url, MediaType::Aac),
        })
    }
}

#[derive(Deserialize)]
struct DeezerResponse {
    data: Vec<DeezerTrack>,
}

#[derive(Deserialize)]
struct DeezerTrack {
    id: u64,
    title: String,
    duration: u64,
    #[serde(rename = "preview")]
    _preview: Option<String>,
    artist: DeezerNamed,
    album: DeezerAlbum,
}

#[derive(Deserialize)]
struct DeezerNamed {
    name: String,
}

#[derive(Deserialize)]
struct DeezerAlbum {
    title: String,
    cover_medium: Option<String>,
}

impl DeezerTrack {
    fn into_catalog(self) -> Option<CatalogTrack> {
        Some(CatalogTrack {
            provider_id: "deezer".into(),
            provider_track_id: self.id.to_string(),
            title: self.title,
            artist: self.artist.name,
            album: self.album.title,
            duration_ms: Some(self.duration * 1000),
            artwork_url: self
                .album
                .cover_medium
                .and_then(|url| Url::parse(&url).ok()),
            resolver_payload: json!({ "provider": "deezer", "trackId": self.id }),
            // Deezer's current preview MP3 payloads fail Symphonia 0.5 probing with an
            // out-of-bounds frame error. Keep Deezer as a metadata/replacement source but do not
            // advertise an unplayable preview request.
            preview: None,
        })
    }
}

#[derive(Deserialize)]
struct AppleChartResponse {
    feed: AppleChartFeed,
}

#[derive(Deserialize)]
struct AppleChartFeed {
    results: Vec<AppleChartTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleChartTrack {
    id: String,
    name: String,
    artist_name: String,
    artwork_url100: Option<String>,
}

impl AppleChartTrack {
    fn into_catalog(self) -> CatalogTrack {
        CatalogTrack {
            provider_id: "apple_chart".into(),
            provider_track_id: self.id.clone(),
            title: self.name,
            artist: self.artist_name,
            album: String::new(),
            duration_ms: None,
            artwork_url: self.artwork_url100.and_then(|url| Url::parse(&url).ok()),
            resolver_payload: json!({ "provider": "itunes", "trackId": self.id }),
            preview: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibItem {
    duration: f64,
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(provider: &str, title: &str, artist: &str, duration: u64) -> CatalogTrack {
        CatalogTrack {
            provider_id: provider.into(),
            provider_track_id: provider.into(),
            title: title.into(),
            artist: artist.into(),
            album: "Album".into(),
            duration_ms: Some(duration),
            artwork_url: None,
            resolver_payload: Value::Null,
            preview: None,
        }
    }

    #[test]
    fn parses_multiple_lrc_timestamps_and_sorts_lines() {
        let parsed = parse_lrc("[00:02.50][00:03.000]Second\n[ar:artist]\n[00:01.00]First");
        assert_eq!(parsed.lines.len(), 3);
        assert_eq!(parsed.lines[0].timestamp_ms, Some(1000));
        assert_eq!(parsed.lines[1].timestamp_ms, Some(2500));
        assert_eq!(parsed.lines[2].timestamp_ms, Some(3000));
    }

    #[test]
    fn replacement_prefers_same_title_artist_and_duration() {
        let wanted = track("wy", "Song (Live)", "Artist A", 200_000);
        let candidates = vec![
            track("kw", "Completely Different", "Artist A", 200_000),
            track("tx", "Song Live", "Artist A", 201_000),
            track("kg", "Song Live", "Other", 201_000),
        ];
        let matches = find_replacements(&wanted, candidates);
        assert_eq!(matches[0].track.provider_id, "tx");
        assert!(matches[0].score > 0.9);
    }

    #[test]
    fn itunes_fixture_maps_preview_to_structured_request() {
        let response: ItunesResponse = serde_json::from_value(json!({
            "results": [{
                "trackId": 1,
                "trackName": "Song",
                "artistName": "Artist",
                "collectionName": "Album",
                "trackTimeMillis": 123000,
                "previewUrl": "https://audio-ssl.itunes.apple.com/preview.m4a"
            }]
        }))
        .unwrap();
        let track = response
            .results
            .into_iter()
            .next()
            .unwrap()
            .into_catalog()
            .unwrap();
        assert_eq!(track.preview.unwrap().media_type, MediaType::Aac);
    }

    #[test]
    fn unavailable_original_selects_cross_provider_preview() {
        let mut wanted = track("wy", "Song", "Artist", 200_000);
        wanted.preview = preview_request(
            Some("https://example.com/wanted.mp3".into()),
            MediaType::Mp3,
        );
        let mut replacement = track("tx", "Song", "Artist", 201_000);
        replacement.preview = preview_request(
            Some("https://example.com/replacement.mp3".into()),
            MediaType::Mp3,
        );
        let selected = select_playable_with(&wanted, vec![replacement], |request| {
            request.url.path().contains("replacement")
        })
        .unwrap();
        assert_eq!(selected.0.provider_id, "tx");
        assert_eq!(selected.1.as_deref(), Some("wy"));
    }
}
