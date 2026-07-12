use std::cmp::Ordering;
use std::time::Duration;

use gx_contracts::{MediaType, ResolvedMediaRequest};
use gx_source::safe_http::{SafeHttpRequest, execute};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
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
    let (kugou, kuwo, netease, itunes, deezer) = std::thread::scope(|scope| {
        let kugou = scope.spawn(|| search_kugou(query, limit));
        let kuwo = scope.spawn(|| search_kuwo(query, limit));
        let netease = scope.spawn(|| search_netease(query, limit));
        let itunes = scope.spawn(|| search_itunes(query, limit));
        let deezer = scope.spawn(|| search_deezer(query, limit));
        (
            kugou.join().unwrap(),
            kuwo.join().unwrap(),
            netease.join().unwrap(),
            itunes.join().unwrap(),
            deezer.join().unwrap(),
        )
    });
    let mut tracks = Vec::new();
    let mut errors = Vec::new();
    for result in [kugou, kuwo, netease, itunes, deezer] {
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

pub fn search_kugou(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MetadataError::EmptyQuery);
    }
    let mut url = Url::parse("https://songsearch.kugou.com/song_search_v2")?;
    url.query_pairs_mut()
        .append_pair("keyword", query)
        .append_pair("page", "1")
        .append_pair("pagesize", &limit.clamp(1, 50).to_string())
        .append_pair("userid", "0")
        .append_pair("clientver", "")
        .append_pair("platform", "WebFilter")
        .append_pair("filter", "2")
        .append_pair("iscorrection", "1")
        .append_pair("privilege_filter", "0")
        .append_pair("area_code", "1");
    let response: KugouResponse = request_json(url)?;
    if response.error_code != 0 {
        return Err(MetadataError::HttpStatus(response.error_code.max(0) as u16));
    }
    Ok(response
        .data
        .map(|data| data.tracks)
        .unwrap_or_default()
        .into_iter()
        .filter_map(KugouTrack::into_catalog)
        .collect())
}

pub fn search_kuwo(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MetadataError::EmptyQuery);
    }
    let mut url = Url::parse("https://search.kuwo.cn/r.s")?;
    url.query_pairs_mut()
        .append_pair("client", "kt")
        .append_pair("all", query)
        .append_pair("pn", "0")
        .append_pair("rn", &limit.clamp(1, 50).to_string())
        .append_pair("uid", "794762570")
        .append_pair("ver", "kwplayer_ar_9.2.2.1")
        .append_pair("vipver", "1")
        .append_pair("show_copyright_off", "1")
        .append_pair("newver", "1")
        .append_pair("ft", "music")
        .append_pair("cluster", "0")
        .append_pair("strategy", "2012")
        .append_pair("encoding", "utf8")
        .append_pair("rformat", "json")
        .append_pair("vermerge", "1")
        .append_pair("mobi", "1")
        .append_pair("issubtitle", "1");
    let response: KuwoResponse = request_json(url)?;
    Ok(response
        .tracks
        .into_iter()
        .filter_map(KuwoTrack::into_catalog)
        .collect())
}

pub fn search_netease(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MetadataError::EmptyQuery);
    }
    let url = Url::parse("https://music.163.com/api/search/get/web?csrf_token=")?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("s", query)
        .append_pair("type", "1")
        .append_pair("offset", "0")
        .append_pair("total", "true")
        .append_pair("limit", &limit.clamp(1, 50).to_string())
        .finish()
        .into_bytes();
    let response = execute(SafeHttpRequest {
        url,
        method: Method::POST,
        headers: vec![
            (
                "user-agent".into(),
                "Mozilla/5.0 GXPlayer/0.1 metadata".into(),
            ),
            ("referer".into(), "https://music.163.com/".into()),
            (
                "content-type".into(),
                "application/x-www-form-urlencoded; charset=UTF-8".into(),
            ),
        ],
        body: Some(body),
        timeout: Duration::from_secs(15),
        max_response_bytes: RESPONSE_LIMIT,
    })
    .map_err(|error| MetadataError::Http(error.to_string()))?;
    if !(200..300).contains(&response.status) {
        return Err(MetadataError::HttpStatus(response.status));
    }
    let response: NeteaseResponse = serde_json::from_slice(&response.body)?;
    if response.code != 200 {
        return Err(MetadataError::HttpStatus(response.code.max(0) as u16));
    }
    Ok(response
        .result
        .map(|result| result.songs)
        .unwrap_or_default()
        .into_iter()
        .map(NeteaseTrack::into_catalog)
        .collect())
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
struct KugouResponse {
    error_code: i32,
    data: Option<KugouData>,
}

#[derive(Deserialize)]
struct KugouData {
    #[serde(rename = "lists", default)]
    tracks: Vec<KugouTrack>,
}

#[derive(Deserialize)]
struct KugouTrack {
    #[serde(rename = "Audioid")]
    audio_id: u64,
    #[serde(rename = "SongName")]
    song_name: String,
    #[serde(rename = "SingerName", default)]
    singer_name: String,
    #[serde(rename = "AlbumName", default)]
    album_name: String,
    #[serde(rename = "AlbumID", default)]
    album_id: String,
    #[serde(rename = "Duration", default)]
    duration: u64,
    #[serde(rename = "FileHash", default)]
    file_hash: String,
    #[serde(rename = "FileSize", default)]
    file_size: u64,
    #[serde(rename = "HQFileHash", default)]
    hq_file_hash: String,
    #[serde(rename = "HQFileSize", default)]
    hq_file_size: u64,
    #[serde(rename = "SQFileHash", default)]
    sq_file_hash: String,
    #[serde(rename = "SQFileSize", default)]
    sq_file_size: u64,
    #[serde(rename = "ResFileHash", default)]
    res_file_hash: String,
    #[serde(rename = "ResFileSize", default)]
    res_file_size: u64,
    #[serde(rename = "Image")]
    image: Option<String>,
}

impl KugouTrack {
    fn into_catalog(self) -> Option<CatalogTrack> {
        if self.audio_id == 0 || self.file_hash.trim().is_empty() {
            return None;
        }
        let mut types = Vec::new();
        let mut raw_types = Map::new();
        push_kugou_type(
            &mut types,
            &mut raw_types,
            "128k",
            self.file_size,
            &self.file_hash,
        );
        push_kugou_type(
            &mut types,
            &mut raw_types,
            "320k",
            self.hq_file_size,
            &self.hq_file_hash,
        );
        push_kugou_type(
            &mut types,
            &mut raw_types,
            "flac",
            self.sq_file_size,
            &self.sq_file_hash,
        );
        push_kugou_type(
            &mut types,
            &mut raw_types,
            "flac24bit",
            self.res_file_size,
            &self.res_file_hash,
        );
        let interval = format!("{}:{:02}", self.duration / 60, self.duration % 60);
        let artwork_text = self.image.map(|value| {
            value
                .replace("{size}", "400")
                .replacen("http://", "https://", 1)
        });
        let artwork_url = artwork_text
            .as_deref()
            .and_then(|value| Url::parse(value).ok());
        let songmid = self.audio_id.to_string();
        let music_info = json!({
            "name": self.song_name,
            "singer": self.singer_name,
            "source": "kg",
            "songmid": songmid,
            "hash": self.file_hash,
            "interval": interval,
            "albumName": self.album_name,
            "albumId": self.album_id,
            "img": artwork_text,
            "types": types,
            "_types": raw_types,
            "typeUrl": {}
        });
        Some(CatalogTrack {
            provider_id: "kg".into(),
            provider_track_id: songmid,
            title: self.song_name,
            artist: self.singer_name,
            album: self.album_name,
            duration_ms: Some(self.duration * 1000),
            artwork_url,
            resolver_payload: json!({
                "source": "kg",
                "musicInfo": music_info,
            }),
            preview: None,
        })
    }
}

fn push_kugou_type(
    types: &mut Vec<Value>,
    raw_types: &mut Map<String, Value>,
    kind: &str,
    size: u64,
    hash: &str,
) {
    if size == 0 || hash.trim().is_empty() {
        return;
    }
    let size_text = format!("{:.2} MB", size as f64 / 1024.0 / 1024.0);
    types.push(json!({ "type": kind, "size": size_text, "hash": hash }));
    raw_types.insert(kind.into(), json!({ "size": size_text, "hash": hash }));
}

#[derive(Deserialize)]
struct KuwoResponse {
    #[serde(rename = "abslist", default)]
    tracks: Vec<KuwoTrack>,
}

#[derive(Deserialize)]
struct KuwoTrack {
    #[serde(rename = "MUSICRID")]
    music_rid: String,
    #[serde(rename = "SONGNAME")]
    song_name: String,
    #[serde(rename = "ARTIST")]
    artist: String,
    #[serde(rename = "ALBUM", default)]
    album: String,
    #[serde(rename = "ALBUMID", default)]
    album_id: String,
    #[serde(rename = "DURATION", default)]
    duration: String,
    #[serde(rename = "N_MINFO", default)]
    media_info: String,
    #[serde(rename = "web_albumpic_short")]
    album_picture: Option<String>,
    #[serde(rename = "web_artistpic_short")]
    artist_picture: Option<String>,
}

impl KuwoTrack {
    fn into_catalog(self) -> Option<CatalogTrack> {
        let songmid = self
            .music_rid
            .strip_prefix("MUSIC_")
            .unwrap_or(&self.music_rid)
            .trim()
            .to_owned();
        if songmid.is_empty() {
            return None;
        }
        let duration_seconds = self.duration.parse::<u64>().ok();
        let interval = duration_seconds
            .map(|seconds| format!("{}:{:02}", seconds / 60, seconds % 60))
            .unwrap_or_default();
        let (types, raw_types) = parse_kuwo_media_types(&self.media_info);
        let artwork_text = normalize_kuwo_artwork(
            self.album_picture.as_deref(),
            self.artist_picture.as_deref(),
        );
        let artwork_url = artwork_text
            .as_deref()
            .and_then(|value| Url::parse(value).ok());
        let music_info = json!({
            "name": self.song_name,
            "singer": self.artist,
            "source": "kw",
            "songmid": songmid,
            "interval": interval,
            "albumName": self.album,
            "albumId": self.album_id,
            "img": artwork_text,
            "types": types,
            "_types": raw_types,
            "typeUrl": {}
        });
        Some(CatalogTrack {
            provider_id: "kw".into(),
            provider_track_id: songmid,
            title: self.song_name,
            artist: self.artist,
            album: self.album,
            duration_ms: duration_seconds.map(|seconds| seconds * 1000),
            artwork_url,
            resolver_payload: json!({
                "source": "kw",
                "musicInfo": music_info,
            }),
            preview: None,
        })
    }
}

fn parse_kuwo_media_types(media_info: &str) -> (Vec<Value>, Map<String, Value>) {
    let mut types = Vec::new();
    let mut raw_types = Map::new();
    for entry in media_info.split(';') {
        let mut bitrate = None;
        let mut size = None;
        for field in entry.split(',') {
            if let Some(value) = field.strip_prefix("bitrate:") {
                bitrate = Some(value);
            } else if let Some(value) = field.strip_prefix("size:") {
                size = Some(value);
            }
        }
        let Some(kind) = bitrate.and_then(|value| match value {
            "4000" => Some("flac24bit"),
            "2000" => Some("flac"),
            "320" => Some("320k"),
            "128" => Some("128k"),
            _ => None,
        }) else {
            continue;
        };
        if raw_types.contains_key(kind) {
            continue;
        }
        let size = size.unwrap_or_default();
        types.push(json!({ "type": kind, "size": size }));
        raw_types.insert(kind.into(), json!({ "size": size.to_ascii_uppercase() }));
    }
    types.reverse();
    (types, raw_types)
}

fn normalize_kuwo_artwork(album: Option<&str>, artist: Option<&str>) -> Option<String> {
    let (value, root) = album
        .filter(|value| !value.trim().is_empty())
        .map(|value| (value, "albumcover"))
        .or_else(|| {
            artist
                .filter(|value| !value.trim().is_empty())
                .map(|value| (value, "starheads"))
        })?;
    if value.starts_with("http://") || value.starts_with("https://") {
        return Some(value.to_owned());
    }
    Some(format!(
        "https://img1.kuwo.cn/star/{root}/{}",
        value.trim_start_matches('/')
    ))
}

#[derive(Deserialize)]
struct NeteaseResponse {
    code: i32,
    result: Option<NeteaseResult>,
}

#[derive(Deserialize)]
struct NeteaseResult {
    #[serde(default)]
    songs: Vec<NeteaseTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NeteaseTrack {
    id: u64,
    name: String,
    duration: u64,
    #[serde(default)]
    artists: Vec<NeteaseArtist>,
    album: NeteaseAlbum,
}

#[derive(Deserialize)]
struct NeteaseArtist {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NeteaseAlbum {
    id: u64,
    name: String,
    pic_url: Option<String>,
}

impl NeteaseTrack {
    fn into_catalog(self) -> CatalogTrack {
        let artist = self
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join("、");
        let interval_seconds = self.duration / 1000;
        let interval = format!("{}:{:02}", interval_seconds / 60, interval_seconds % 60);
        let music_info = json!({
            "name": self.name,
            "singer": artist,
            "source": "wy",
            "songmid": self.id.to_string(),
            "interval": interval,
            "albumName": self.album.name,
            "albumId": self.album.id.to_string(),
            "img": self.album.pic_url,
            "types": [
                { "type": "128k", "size": null },
                { "type": "320k", "size": null },
                { "type": "flac", "size": null }
            ],
            "_types": {},
            "typeUrl": {}
        });
        CatalogTrack {
            provider_id: "wy".into(),
            provider_track_id: self.id.to_string(),
            title: self.name,
            artist,
            album: self.album.name,
            duration_ms: Some(self.duration),
            artwork_url: self.album.pic_url.and_then(|url| Url::parse(&url).ok()),
            resolver_payload: json!({
                "source": "wy",
                "musicInfo": music_info,
            }),
            preview: None,
        }
    }
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
    fn kuwo_fixture_preserves_lx_songmid_and_quality_metadata() {
        let response: KuwoResponse = serde_json::from_value(json!({
            "abslist": [{
                "MUSICRID": "MUSIC_228908",
                "SONGNAME": "晴天",
                "ARTIST": "周杰伦",
                "ALBUM": "叶惠美",
                "ALBUMID": "1293",
                "DURATION": "269",
                "N_MINFO": "level:ff,bitrate:2000,format:flac,size:52.83Mb;level:p,bitrate:320,format:mp3,size:10.29Mb;level:h,bitrate:128,format:mp3,size:4.12Mb",
                "web_albumpic_short": "120/s3s94/93/211513640.jpg"
            }]
        }))
        .unwrap();
        let track = response
            .tracks
            .into_iter()
            .next()
            .unwrap()
            .into_catalog()
            .unwrap();
        assert_eq!(track.provider_id, "kw");
        assert_eq!(track.provider_track_id, "228908");
        assert_eq!(track.duration_ms, Some(269_000));
        assert_eq!(track.resolver_payload["source"], "kw");
        assert_eq!(track.resolver_payload["musicInfo"]["songmid"], "228908");
        assert_eq!(
            track.resolver_payload["musicInfo"]["_types"]["320k"]["size"],
            "10.29MB"
        );
    }

    #[test]
    fn kugou_fixture_preserves_base_hash_for_music_url() {
        let response: KugouResponse = serde_json::from_value(json!({
            "error_code": 0,
            "data": {
                "lists": [{
                    "Audioid": 20505418,
                    "SongName": "晴天",
                    "SingerName": "周杰伦",
                    "AlbumName": "叶惠美",
                    "AlbumID": "966846",
                    "Duration": 269,
                    "FileHash": "B3A52A7A958BF0AED0EBFBA2E9A818B7",
                    "FileSize": 4317292,
                    "HQFileHash": "1B56126A8A03924F1DD066259C095CBC",
                    "HQFileSize": 10792943,
                    "SQFileHash": "78E125D093837C463270EAC03BB9D8A9",
                    "SQFileSize": 31729524,
                    "ResFileHash": "",
                    "ResFileSize": 0,
                    "Image": "http://imge.kugou.com/stdmusic/{size}/cover.jpg"
                }]
            }
        }))
        .unwrap();
        let track = response
            .data
            .unwrap()
            .tracks
            .into_iter()
            .next()
            .unwrap()
            .into_catalog()
            .unwrap();
        assert_eq!(track.provider_id, "kg");
        assert_eq!(track.provider_track_id, "20505418");
        assert_eq!(
            track.resolver_payload["musicInfo"]["hash"],
            "B3A52A7A958BF0AED0EBFBA2E9A818B7"
        );
        assert_eq!(
            track.resolver_payload["musicInfo"]["_types"]["320k"]["hash"],
            "1B56126A8A03924F1DD066259C095CBC"
        );
        assert_eq!(
            track.artwork_url.unwrap().as_str(),
            "https://imge.kugou.com/stdmusic/400/cover.jpg"
        );
    }

    #[test]
    fn netease_fixture_preserves_lx_songmid() {
        let response: NeteaseResponse = serde_json::from_value(json!({
            "code": 200,
            "result": {
                "songs": [{
                    "id": 123,
                    "name": "Song",
                    "duration": 245000,
                    "artists": [{ "name": "Artist" }],
                    "album": { "id": 9, "name": "Album", "picUrl": "https://example.com/a.jpg" }
                }]
            }
        }))
        .unwrap();
        let track = response
            .result
            .unwrap()
            .songs
            .into_iter()
            .next()
            .unwrap()
            .into_catalog();
        assert_eq!(track.provider_id, "wy");
        assert_eq!(track.resolver_payload["musicInfo"]["songmid"], "123");
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
