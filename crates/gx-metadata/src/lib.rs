use std::cmp::Ordering;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gx_contracts::{MediaType, ResolvedMediaRequest};
use gx_source::network_policy::source_route_attempts;
use gx_source::safe_http::{
    RequestCancellation, SafeHttpError, SafeHttpRequest, SafeHttpResponse, execute, execute_async,
    execute_on_route, execute_on_route_async,
};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const PREVIEW_LIMIT: usize = 8 * 1024 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);
const DIRECT_SEARCH_BUDGET: Duration = Duration::from_secs(3);
const MAX_CONCURRENT_REQUESTS: usize = 3;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBatch {
    pub provider_id: String,
    pub tracks: Vec<CatalogTrack>,
    pub error: Option<String>,
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
    #[error("metadata request cancelled")]
    Cancelled,
}

type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SafeHttpResponse, SafeHttpError>> + Send + 'a>>;

trait MetadataTransport: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: SafeHttpRequest,
        cancellation: &'a RequestCancellation,
    ) -> TransportFuture<'a>;

    fn execute_on_route<'a>(
        &'a self,
        request: SafeHttpRequest,
        route: gx_contracts::NetworkRoute,
        cancellation: &'a RequestCancellation,
    ) -> TransportFuture<'a>;
}

struct SafeHttpTransport;

impl MetadataTransport for SafeHttpTransport {
    fn execute<'a>(
        &'a self,
        request: SafeHttpRequest,
        cancellation: &'a RequestCancellation,
    ) -> TransportFuture<'a> {
        Box::pin(execute_async(request, cancellation))
    }

    fn execute_on_route<'a>(
        &'a self,
        request: SafeHttpRequest,
        route: gx_contracts::NetworkRoute,
        cancellation: &'a RequestCancellation,
    ) -> TransportFuture<'a> {
        Box::pin(execute_on_route_async(request, route, cancellation))
    }
}

#[derive(Clone)]
pub struct MetadataClient {
    transport: Arc<dyn MetadataTransport>,
    requests: Arc<Semaphore>,
}

impl Default for MetadataClient {
    fn default() -> Self {
        Self {
            transport: Arc::new(SafeHttpTransport),
            requests: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        }
    }
}

impl MetadataClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn search_all_progressive<F>(
        &self,
        query: &str,
        limit: usize,
        cancellation: &RequestCancellation,
        mut on_batch: F,
    ) -> Result<Vec<CatalogTrack>, MetadataError>
    where
        F: FnMut(SearchBatch) + Send,
    {
        let query = query.trim();
        if query.is_empty() {
            return Err(MetadataError::EmptyQuery);
        }
        if cancellation.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }

        let mut tasks = JoinSet::new();
        for provider in SEARCH_PROVIDERS {
            let client = self.clone();
            let query = query.to_owned();
            let cancellation = cancellation.clone();
            tasks.spawn(async move {
                let result = client
                    .search_provider(provider, &query, limit, &cancellation)
                    .await;
                (provider.id(), result)
            });
        }

        let mut tracks = Vec::new();
        let mut errors = Vec::new();
        let mut successful_providers = 0usize;
        while !tasks.is_empty() {
            let completed = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    abort_and_drain(&mut tasks).await;
                    return Err(MetadataError::Cancelled);
                }
                completed = tasks.join_next() => completed,
            };
            if cancellation.is_cancelled() {
                abort_and_drain(&mut tasks).await;
                return Err(MetadataError::Cancelled);
            }
            match completed {
                Some(Ok((_, Err(MetadataError::Cancelled)))) => {
                    abort_and_drain(&mut tasks).await;
                    return Err(MetadataError::Cancelled);
                }
                Some(Ok((provider_id, result))) => accumulate_search_result(
                    provider_id,
                    result,
                    &mut tracks,
                    &mut errors,
                    &mut successful_providers,
                    &mut on_batch,
                ),
                Some(Err(error)) => accumulate_search_result(
                    "unknown",
                    Err(MetadataError::Http(format!(
                        "metadata search worker failed: {error}"
                    ))),
                    &mut tracks,
                    &mut errors,
                    &mut successful_providers,
                    &mut on_batch,
                ),
                None => break,
            }
        }
        finish_search_results(tracks, errors, successful_providers)
    }

    pub async fn fetch_lyrics(
        &self,
        title: &str,
        artist: &str,
        duration_ms: Option<u64>,
        cancellation: &RequestCancellation,
    ) -> Result<Option<LyricDocument>, MetadataError> {
        let url = lyrics_url(title, artist)?;
        let candidates: Vec<LrcLibItem> = self.request_json(url, cancellation).await?;
        Ok(select_lyrics(candidates, duration_ms))
    }

    async fn search_provider(
        &self,
        provider: SearchProvider,
        query: &str,
        limit: usize,
        cancellation: &RequestCancellation,
    ) -> Result<Vec<CatalogTrack>, MetadataError> {
        let request = provider.request(query, limit)?;
        let response = self.execute_search_request(request, cancellation).await?;
        provider.map_response(&response.body)
    }

    async fn execute_search_request(
        &self,
        request: SafeHttpRequest,
        cancellation: &RequestCancellation,
    ) -> Result<SafeHttpResponse, MetadataError> {
        let _permit = self.acquire(cancellation).await?;
        let routes = source_route_attempts(None);
        let deadline = tokio::time::Instant::now() + SEARCH_TIMEOUT;
        let mut last_error = None;
        for (index, route) in routes.iter().copied().enumerate() {
            if cancellation.is_cancelled() {
                return Err(MetadataError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let mut attempt = request.clone();
            attempt.timeout = search_attempt_timeout(remaining, routes.len() - index);
            let response = tokio::time::timeout(
                remaining,
                self.transport
                    .execute_on_route(attempt, route, cancellation),
            )
            .await;
            match response {
                Ok(Ok(response)) if (200..300).contains(&response.status) => return Ok(response),
                Ok(Ok(response)) => last_error = Some(MetadataError::HttpStatus(response.status)),
                Ok(Err(SafeHttpError::Cancelled)) => return Err(MetadataError::Cancelled),
                Ok(Err(error)) => last_error = Some(MetadataError::Http(error.to_string())),
                Err(_) => last_error = Some(MetadataError::Http("search request timed out".into())),
            }
        }
        Err(last_error.unwrap_or_else(|| MetadataError::Http("search request timed out".into())))
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        url: Url,
        cancellation: &RequestCancellation,
    ) -> Result<T, MetadataError> {
        let mut last_error = None;
        for attempt in 0..3 {
            if cancellation.is_cancelled() {
                return Err(MetadataError::Cancelled);
            }
            let request = SafeHttpRequest {
                url: url.clone(),
                method: Method::GET,
                headers: vec![("user-agent".into(), "GXPlayer/0.1 metadata".into())],
                body: None,
                timeout: Duration::from_secs(15),
                max_response_bytes: RESPONSE_LIMIT,
            };
            let permit = self.acquire(cancellation).await?;
            let response = self.transport.execute(request, cancellation).await;
            drop(permit);
            match response {
                Ok(response) if (200..300).contains(&response.status) => {
                    return Ok(serde_json::from_slice(&response.body)?);
                }
                Ok(response) => last_error = Some(MetadataError::HttpStatus(response.status)),
                Err(SafeHttpError::Cancelled) => return Err(MetadataError::Cancelled),
                Err(error) => last_error = Some(MetadataError::Http(error.to_string())),
            }
            if attempt < 2 {
                cancelable_sleep(Duration::from_millis(200 * (attempt + 1)), cancellation).await?;
            }
        }
        Err(last_error.unwrap_or_else(|| MetadataError::Http("request did not run".into())))
    }

    async fn acquire(
        &self,
        cancellation: &RequestCancellation,
    ) -> Result<OwnedSemaphorePermit, MetadataError> {
        let requests = self.requests.clone();
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(MetadataError::Cancelled),
            permit = requests.acquire_owned() => permit.map_err(|_| {
                MetadataError::Http("metadata request limiter closed".into())
            }),
        }
    }

    #[cfg(test)]
    fn with_transport(transport: Arc<dyn MetadataTransport>) -> Self {
        Self {
            transport,
            requests: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        }
    }
}

async fn abort_and_drain(
    tasks: &mut JoinSet<(&'static str, Result<Vec<CatalogTrack>, MetadataError>)>,
) {
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn cancelable_sleep(
    duration: Duration,
    cancellation: &RequestCancellation,
) -> Result<(), MetadataError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(MetadataError::Cancelled),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

#[derive(Clone, Copy)]
enum SearchProvider {
    Kugou,
    Kuwo,
    Netease,
    Itunes,
    Deezer,
}

const SEARCH_PROVIDERS: [SearchProvider; 5] = [
    SearchProvider::Kugou,
    SearchProvider::Kuwo,
    SearchProvider::Netease,
    SearchProvider::Itunes,
    SearchProvider::Deezer,
];

impl SearchProvider {
    fn id(self) -> &'static str {
        match self {
            Self::Kugou => "kg",
            Self::Kuwo => "kw",
            Self::Netease => "wy",
            Self::Itunes => "itunes",
            Self::Deezer => "deezer",
        }
    }

    fn request(self, query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
        match self {
            Self::Kugou => kugou_search_request(query, limit),
            Self::Kuwo => kuwo_search_request(query, limit),
            Self::Netease => netease_search_request(query, limit),
            Self::Itunes => itunes_search_request(query, limit),
            Self::Deezer => deezer_search_request(query, limit),
        }
    }

    fn map_response(self, body: &[u8]) -> Result<Vec<CatalogTrack>, MetadataError> {
        match self {
            Self::Kugou => map_kugou_response(serde_json::from_slice(body)?),
            Self::Kuwo => Ok(map_kuwo_response(serde_json::from_slice(body)?)),
            Self::Netease => map_netease_response(serde_json::from_slice(body)?),
            Self::Itunes => Ok(map_itunes_response(serde_json::from_slice(body)?)),
            Self::Deezer => Ok(map_deezer_response(serde_json::from_slice(body)?)),
        }
    }
}

pub fn search_all(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_all_progressive(query, limit, |_| {})
}

pub fn search_all_progressive<F>(
    query: &str,
    limit: usize,
    on_batch: F,
) -> Result<Vec<CatalogTrack>, MetadataError>
where
    F: FnMut(SearchBatch),
{
    let query = query.trim();
    if query.is_empty() {
        return Err(MetadataError::EmptyQuery);
    }
    let limit = limit.clamp(1, 50);
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for provider in SEARCH_PROVIDERS {
            let sender = sender.clone();
            scope.spawn(move || {
                let provider_id = provider.id();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    search_provider_sync(provider, query, limit)
                }))
                .unwrap_or_else(|_| {
                    Err(MetadataError::Http(format!(
                        "{provider_id} search worker panicked"
                    )))
                });
                let _ = sender.send((provider_id, result));
            });
        }
        drop(sender);
        collect_search_results(receiver, on_batch)
    })
}

fn collect_search_results<I, F>(
    results: I,
    mut on_batch: F,
) -> Result<Vec<CatalogTrack>, MetadataError>
where
    I: IntoIterator<Item = (&'static str, Result<Vec<CatalogTrack>, MetadataError>)>,
    F: FnMut(SearchBatch),
{
    let mut tracks = Vec::new();
    let mut errors = Vec::new();
    let mut successful_providers = 0usize;
    for (provider_id, result) in results {
        accumulate_search_result(
            provider_id,
            result,
            &mut tracks,
            &mut errors,
            &mut successful_providers,
            &mut on_batch,
        );
    }
    finish_search_results(tracks, errors, successful_providers)
}

fn accumulate_search_result<F>(
    provider_id: &'static str,
    result: Result<Vec<CatalogTrack>, MetadataError>,
    tracks: &mut Vec<CatalogTrack>,
    errors: &mut Vec<String>,
    successful_providers: &mut usize,
    on_batch: &mut F,
) where
    F: FnMut(SearchBatch),
{
    match result {
        Ok(found) => {
            *successful_providers += 1;
            on_batch(SearchBatch {
                provider_id: provider_id.to_owned(),
                tracks: found.clone(),
                error: None,
            });
            tracks.extend(found);
        }
        Err(error) => {
            let error = error.to_string();
            on_batch(SearchBatch {
                provider_id: provider_id.to_owned(),
                tracks: Vec::new(),
                error: Some(error.clone()),
            });
            errors.push(error);
        }
    }
}

fn finish_search_results(
    tracks: Vec<CatalogTrack>,
    errors: Vec<String>,
    successful_providers: usize,
) -> Result<Vec<CatalogTrack>, MetadataError> {
    if successful_providers == 0 && !errors.is_empty() {
        return Err(MetadataError::Http(errors.join("; ")));
    }
    Ok(tracks)
}

pub fn search_kugou(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_provider_sync(SearchProvider::Kugou, query, limit)
}

pub fn search_kuwo(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_provider_sync(SearchProvider::Kuwo, query, limit)
}

pub fn search_netease(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_provider_sync(SearchProvider::Netease, query, limit)
}

pub fn search_itunes(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_provider_sync(SearchProvider::Itunes, query, limit)
}

pub fn search_deezer(query: &str, limit: usize) -> Result<Vec<CatalogTrack>, MetadataError> {
    search_provider_sync(SearchProvider::Deezer, query, limit)
}

fn search_provider_sync(
    provider: SearchProvider,
    query: &str,
    limit: usize,
) -> Result<Vec<CatalogTrack>, MetadataError> {
    let response = execute_search_request(provider.request(query, limit)?)?;
    provider.map_response(&response.body)
}

fn search_get_request(url: Url) -> SafeHttpRequest {
    SafeHttpRequest {
        url,
        method: Method::GET,
        headers: vec![("user-agent".into(), "GXPlayer/0.1 metadata".into())],
        body: None,
        timeout: SEARCH_TIMEOUT,
        max_response_bytes: RESPONSE_LIMIT,
    }
}

fn kugou_search_request(query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
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
    Ok(search_get_request(url))
}

fn map_kugou_response(response: KugouResponse) -> Result<Vec<CatalogTrack>, MetadataError> {
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

fn kuwo_search_request(query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
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
    Ok(search_get_request(url))
}

fn map_kuwo_response(response: KuwoResponse) -> Vec<CatalogTrack> {
    response
        .tracks
        .into_iter()
        .filter_map(KuwoTrack::into_catalog)
        .collect()
}

fn netease_search_request(query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
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
    Ok(SafeHttpRequest {
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
        timeout: SEARCH_TIMEOUT,
        max_response_bytes: RESPONSE_LIMIT,
    })
}

fn map_netease_response(response: NeteaseResponse) -> Result<Vec<CatalogTrack>, MetadataError> {
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

fn itunes_search_request(query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
    let mut url = Url::parse("https://itunes.apple.com/search")?;
    url.query_pairs_mut()
        .append_pair("term", query)
        .append_pair("entity", "song")
        .append_pair("country", "CN")
        .append_pair("limit", &limit.clamp(1, 50).to_string());
    Ok(search_get_request(url))
}

fn map_itunes_response(response: ItunesResponse) -> Vec<CatalogTrack> {
    response
        .results
        .into_iter()
        .filter_map(ItunesTrack::into_catalog)
        .collect()
}

fn deezer_search_request(query: &str, limit: usize) -> Result<SafeHttpRequest, MetadataError> {
    let mut url = Url::parse("https://api.deezer.com/search")?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &limit.clamp(1, 50).to_string());
    Ok(search_get_request(url))
}

fn map_deezer_response(response: DeezerResponse) -> Vec<CatalogTrack> {
    response
        .data
        .into_iter()
        .filter_map(DeezerTrack::into_catalog)
        .collect()
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
    let candidates: Vec<LrcLibItem> = request_json(lyrics_url(title, artist)?)?;
    Ok(select_lyrics(candidates, duration_ms))
}

fn lyrics_url(title: &str, artist: &str) -> Result<Url, MetadataError> {
    let mut url = Url::parse("https://lrclib.net/api/search")?;
    url.query_pairs_mut()
        .append_pair("track_name", title)
        .append_pair("artist_name", artist);
    Ok(url)
}

fn select_lyrics(candidates: Vec<LrcLibItem>, duration_ms: Option<u64>) -> Option<LyricDocument> {
    let selected = candidates.into_iter().min_by_key(|item| {
        duration_ms.map_or(0, |target| {
            target.abs_diff((item.duration.max(0.0) * 1000.0) as u64)
        })
    });
    selected.map(|item| {
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
    })
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

fn execute_search_request(request: SafeHttpRequest) -> Result<SafeHttpResponse, MetadataError> {
    let routes = source_route_attempts(None);
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let mut last_error = None;
    for (index, route) in routes.iter().copied().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let attempts_left = routes.len() - index;
        let timeout = search_attempt_timeout(remaining, attempts_left);
        let mut attempt = request.clone();
        attempt.timeout = timeout;
        match execute_on_route(attempt, route) {
            Ok(response) if (200..300).contains(&response.status) => return Ok(response),
            Ok(response) => last_error = Some(MetadataError::HttpStatus(response.status)),
            Err(error) => last_error = Some(MetadataError::Http(error.to_string())),
        }
    }
    Err(last_error.unwrap_or_else(|| MetadataError::Http("search request timed out".into())))
}

fn search_attempt_timeout(remaining: Duration, attempts_left: usize) -> Duration {
    if attempts_left > 1 {
        remaining.min(DIRECT_SEARCH_BUDGET)
    } else {
        remaining
    }
}

fn preview_request(url: Option<String>, media_type: MediaType) -> Option<ResolvedMediaRequest> {
    let url = Url::parse(url.as_deref()?).ok()?;
    Some(ResolvedMediaRequest {
        url,
        headers: Vec::new(),
        media_type,
        quality: Some("preview".into()),
        expires_at_ms: None,
        network_route: None,
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
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct FakeTransport {
        delay: Duration,
        active: AtomicUsize,
        peak: AtomicUsize,
        started: AtomicUsize,
    }

    impl FakeTransport {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                started: AtomicUsize::new(0),
            }
        }

        fn run<'a>(
            &'a self,
            request: SafeHttpRequest,
            cancellation: &'a RequestCancellation,
        ) -> TransportFuture<'a> {
            Box::pin(async move {
                self.started.fetch_add(1, AtomicOrdering::SeqCst);
                let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                self.peak.fetch_max(active, AtomicOrdering::SeqCst);
                let _active = ActiveRequest(&self.active);
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(SafeHttpError::Cancelled),
                    _ = tokio::time::sleep(self.delay) => Ok(fake_search_response(request.url)),
                }
            })
        }
    }

    impl MetadataTransport for FakeTransport {
        fn execute<'a>(
            &'a self,
            request: SafeHttpRequest,
            cancellation: &'a RequestCancellation,
        ) -> TransportFuture<'a> {
            self.run(request, cancellation)
        }

        fn execute_on_route<'a>(
            &'a self,
            request: SafeHttpRequest,
            _route: gx_contracts::NetworkRoute,
            cancellation: &'a RequestCancellation,
        ) -> TransportFuture<'a> {
            self.run(request, cancellation)
        }
    }

    struct ImmediateTransport {
        cancelled: bool,
        attempts: AtomicUsize,
    }

    impl ImmediateTransport {
        fn run(&self, request: SafeHttpRequest) -> TransportFuture<'_> {
            Box::pin(async move {
                self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
                if self.cancelled {
                    Err(SafeHttpError::Cancelled)
                } else {
                    Ok(SafeHttpResponse {
                        final_url: request.url,
                        status: 503,
                        headers: Vec::new(),
                        body: Vec::new(),
                    })
                }
            })
        }
    }

    impl MetadataTransport for ImmediateTransport {
        fn execute<'a>(
            &'a self,
            request: SafeHttpRequest,
            _cancellation: &'a RequestCancellation,
        ) -> TransportFuture<'a> {
            self.run(request)
        }

        fn execute_on_route<'a>(
            &'a self,
            request: SafeHttpRequest,
            _route: gx_contracts::NetworkRoute,
            _cancellation: &'a RequestCancellation,
        ) -> TransportFuture<'a> {
            self.run(request)
        }
    }

    struct ActiveRequest<'a>(&'a AtomicUsize);

    impl Drop for ActiveRequest<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, AtomicOrdering::SeqCst);
        }
    }

    fn fake_search_response(url: Url) -> SafeHttpResponse {
        let body = match url.host_str().unwrap_or_default() {
            "songsearch.kugou.com" => json!({ "error_code": 0, "data": { "lists": [] } }),
            "search.kuwo.cn" => json!({ "abslist": [] }),
            "music.163.com" => json!({ "code": 200, "result": { "songs": [] } }),
            "itunes.apple.com" => json!({ "results": [] }),
            "api.deezer.com" => json!({ "data": [] }),
            _ => json!([]),
        };
        SafeHttpResponse {
            final_url: url,
            status: 200,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

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
    fn async_search_shares_a_three_request_limit_across_provider_tasks() {
        test_runtime().block_on(async {
            let transport = Arc::new(FakeTransport::new(Duration::from_millis(20)));
            let client = MetadataClient::with_transport(transport.clone());
            let cancellation = RequestCancellation::new();
            let mut batches = Vec::new();

            let tracks = client
                .search_all_progressive("song", 10, &cancellation, |batch| batches.push(batch))
                .await
                .unwrap();

            assert!(tracks.is_empty());
            assert_eq!(batches.len(), SEARCH_PROVIDERS.len());
            assert_eq!(transport.started.load(AtomicOrdering::SeqCst), 5);
            assert_eq!(transport.peak.load(AtomicOrdering::SeqCst), 3);
            assert_eq!(transport.active.load(AtomicOrdering::SeqCst), 0);
        });
    }

    #[test]
    fn async_search_cancellation_emits_no_batch_and_releases_tasks() {
        test_runtime().block_on(async {
            let transport = Arc::new(FakeTransport::new(Duration::from_secs(30)));
            let client = MetadataClient::with_transport(transport.clone());
            let cancellation = RequestCancellation::new();
            let cancel_when_saturated = cancellation.clone();
            let observed_transport = transport.clone();
            let canceller = tokio::spawn(async move {
                while observed_transport.started.load(AtomicOrdering::SeqCst)
                    < MAX_CONCURRENT_REQUESTS
                {
                    tokio::task::yield_now().await;
                }
                cancel_when_saturated.cancel();
            });
            let mut batches = Vec::new();

            let result = client
                .search_all_progressive("song", 10, &cancellation, |batch| batches.push(batch))
                .await;
            canceller.await.unwrap();

            assert!(matches!(result, Err(MetadataError::Cancelled)));
            assert!(batches.is_empty());
            assert_eq!(
                transport.started.load(AtomicOrdering::SeqCst),
                MAX_CONCURRENT_REQUESTS
            );
            assert_eq!(transport.active.load(AtomicOrdering::SeqCst), 0);
        });
    }

    #[test]
    fn async_lyrics_retry_delay_is_cancellable() {
        test_runtime().block_on(async {
            let transport = Arc::new(ImmediateTransport {
                cancelled: false,
                attempts: AtomicUsize::new(0),
            });
            let client = MetadataClient::with_transport(transport.clone());
            let cancellation = RequestCancellation::new();
            let cancel_after_first = cancellation.clone();
            let observed_transport = transport.clone();
            let canceller = tokio::spawn(async move {
                while observed_transport.attempts.load(AtomicOrdering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                cancel_after_first.cancel();
            });

            let result = client
                .fetch_lyrics("Song", "Artist", None, &cancellation)
                .await;
            canceller.await.unwrap();

            assert!(matches!(result, Err(MetadataError::Cancelled)));
            assert_eq!(transport.attempts.load(AtomicOrdering::SeqCst), 1);
        });
    }

    #[test]
    fn transport_cancellation_is_not_retried() {
        test_runtime().block_on(async {
            let transport = Arc::new(ImmediateTransport {
                cancelled: true,
                attempts: AtomicUsize::new(0),
            });
            let client = MetadataClient::with_transport(transport.clone());
            let cancellation = RequestCancellation::new();

            let result = client
                .fetch_lyrics("Song", "Artist", None, &cancellation)
                .await;

            assert!(matches!(result, Err(MetadataError::Cancelled)));
            assert_eq!(transport.attempts.load(AtomicOrdering::SeqCst), 1);
        });
    }

    #[test]
    fn search_route_retry_keeps_a_single_five_second_budget() {
        assert_eq!(
            search_attempt_timeout(Duration::from_secs(5), 2),
            Duration::from_secs(3)
        );
        assert_eq!(
            search_attempt_timeout(Duration::from_secs(2), 1),
            Duration::from_secs(2)
        );
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
    fn progressive_search_emits_success_before_later_failure() {
        let expected = track("fast", "Song", "Artist", 200_000);
        let mut batches = Vec::new();
        let result = collect_search_results(
            vec![
                ("fast", Ok(vec![expected.clone()])),
                ("slow", Err(MetadataError::Http("timeout".into()))),
            ],
            |batch| batches.push(batch),
        )
        .unwrap();

        assert_eq!(result, vec![expected]);
        assert_eq!(batches[0].provider_id, "fast");
        assert_eq!(batches[0].tracks.len(), 1);
        assert_eq!(batches[1].provider_id, "slow");
        assert_eq!(
            batches[1].error.as_deref(),
            Some("metadata HTTP failed: timeout")
        );
    }

    #[test]
    fn progressive_search_only_fails_when_every_provider_fails() {
        let partial = collect_search_results(
            vec![
                ("empty", Ok(Vec::new())),
                ("failed", Err(MetadataError::Http("offline".into()))),
            ],
            |_| {},
        )
        .unwrap();
        assert!(partial.is_empty());

        let failed = collect_search_results(
            vec![
                ("one", Err(MetadataError::Http("offline".into()))),
                ("two", Err(MetadataError::HttpStatus(429))),
            ],
            |_| {},
        )
        .unwrap_err();
        assert!(failed.to_string().contains("offline"));
        assert!(failed.to_string().contains("429"));
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
