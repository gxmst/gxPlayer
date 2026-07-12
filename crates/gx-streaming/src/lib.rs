//! Bounded progressive HTTP media source for Symphonia.
//!
//! Network I/O runs on a dedicated worker. The decoder only blocks on a bounded byte channel and
//! asks the source to restart at a byte offset when Symphonia performs a seek.

use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
#[cfg(feature = "test-private-network")]
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, bounded};
use gx_cache::{CacheWritePlan, CacheWriter};
use gx_contracts::{HttpHeader, ResolvedMediaRequest};
use gx_source::safe_http::validate_and_resolve;
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT_RANGES, AUTHORIZATION, CONTENT_RANGE, COOKIE, ETAG, HeaderName, HeaderValue, IF_RANGE,
    LAST_MODIFIED, LOCATION, PROXY_AUTHORIZATION, RANGE,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const CHUNK_SIZE: usize = 64 * 1024;
const CHANNEL_CAPACITY: usize = 8;
const MAX_RECONNECTS: u64 = 3;
const MAX_MEDIA_REDIRECTS: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_MESSAGE_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Default)]
pub struct StreamMetrics {
    requests: AtomicU64,
    range_requests: AtomicU64,
    reconnects: AtomicU64,
    bytes_received: AtomicU64,
    backpressure_waits: AtomicU64,
}

impl StreamMetrics {
    pub fn snapshot(&self) -> StreamMetricsSnapshot {
        StreamMetricsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            range_requests: self.range_requests.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            backpressure_waits: self.backpressure_waits.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamMetricsSnapshot {
    pub requests: u64,
    pub range_requests: u64,
    pub reconnects: u64,
    pub bytes_received: u64,
    pub backpressure_waits: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpDecodeReport {
    pub final_url: String,
    pub total_bytes: Option<u64>,
    pub sample_rate: u32,
    pub channels: usize,
    pub decoded_frames: usize,
    pub seek_discarded_frames: usize,
    pub peak: f32,
    pub metrics: StreamMetricsSnapshot,
}

enum StreamMessage {
    Data(Vec<u8>),
    End,
    Error(String),
}

struct ReadyInfo {
    final_url: reqwest::Url,
    total_len: Option<u64>,
    supports_range: bool,
}

#[derive(Clone)]
struct EffectiveRequest {
    url: reqwest::Url,
    headers: Vec<HttpHeader>,
}

struct WorkerState {
    receiver: Receiver<StreamMessage>,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    effective_request: Arc<Mutex<EffectiveRequest>>,
}

pub struct HttpMediaSource {
    request: ResolvedMediaRequest,
    worker: Option<WorkerState>,
    current_chunk: Vec<u8>,
    chunk_offset: usize,
    position: u64,
    total_len: Option<u64>,
    supports_range: bool,
    final_url: reqwest::Url,
    metrics: Arc<StreamMetrics>,
    cache_plan: Option<CacheWritePlan>,
    reached_end: bool,
}

impl HttpMediaSource {
    pub fn new(request: ResolvedMediaRequest) -> Result<Self> {
        Self::new_with_cache(request, None)
    }

    pub fn new_with_cache(
        request: ResolvedMediaRequest,
        cache_plan: Option<CacheWritePlan>,
    ) -> Result<Self> {
        if request.is_expired_at(unix_time_ms()) {
            bail!("resolved media request has expired and must be resolved again");
        }
        let metrics = Arc::new(StreamMetrics::default());
        let placeholder_url = request.url.clone();
        let mut source = Self {
            request,
            worker: None,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            position: 0,
            total_len: None,
            supports_range: false,
            final_url: placeholder_url,
            metrics,
            cache_plan,
            reached_end: false,
        };
        source.restart(0)?;
        Ok(source)
    }

    pub fn metrics_handle(&self) -> Arc<StreamMetrics> {
        Arc::clone(&self.metrics)
    }

    pub fn final_url(&self) -> &reqwest::Url {
        &self.final_url
    }

    pub fn total_len(&self) -> Option<u64> {
        self.total_len
    }

    fn restart(&mut self, offset: u64) -> Result<()> {
        if offset > 0
            && let Some(plan) = &self.cache_plan
        {
            plan.invalidate();
        }
        self.stop_worker();

        let (sender, receiver) = bounded(CHANNEL_CAPACITY);
        let (ready_sender, ready_receiver) = bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let request = self.request_for_restart();
        let effective_request = Arc::new(Mutex::new(EffectiveRequest {
            url: request.url.clone(),
            headers: request.headers.clone(),
        }));
        let metrics = Arc::clone(&self.metrics);
        let cancel_for_worker = Arc::clone(&cancel);
        let effective_for_worker = Arc::clone(&effective_request);
        let cache_writer = (offset == 0)
            .then(|| self.cache_plan.as_ref().and_then(CacheWritePlan::begin))
            .flatten();
        let handle = thread::Builder::new()
            .name("gx-http-stream".into())
            .spawn(move || {
                network_worker(NetworkWorkerArgs {
                    request,
                    initial_offset: offset,
                    sender,
                    ready_sender,
                    cancel: cancel_for_worker,
                    metrics,
                    cache_writer,
                    effective_request: effective_for_worker,
                });
            })
            .context("failed to spawn HTTP streaming worker")?;

        let ready = match ready_receiver.recv_timeout(WORKER_MESSAGE_TIMEOUT) {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                cancel.store(true, Ordering::Release);
                self.apply_effective_request(&effective_request);
                reclaim_worker(handle);
                return Err(anyhow::Error::msg(error));
            }
            Err(error) => {
                cancel.store(true, Ordering::Release);
                reclaim_worker(handle);
                return Err(error).context("HTTP streaming worker did not become ready");
            }
        };
        self.apply_effective_request(&effective_request);
        self.final_url = ready.final_url;
        self.total_len = ready.total_len;
        self.supports_range = ready.supports_range;
        self.position = offset;
        self.current_chunk.clear();
        self.chunk_offset = 0;
        self.reached_end = false;
        self.worker = Some(WorkerState {
            receiver,
            cancel,
            handle,
            effective_request,
        });
        Ok(())
    }

    fn request_for_restart(&self) -> ResolvedMediaRequest {
        let mut request = self.request.clone();
        request.url = self.final_url.clone();
        request
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Release);
            drop(worker.receiver);
            self.apply_effective_request(&worker.effective_request);
            // Blocking reqwest reads are bounded by STREAM_IDLE_TIMEOUT. Join asynchronously so
            // Pause/Seek/Drop never stalls the engine thread while the old socket unwinds, while
            // still reclaiming every worker instead of permanently detaching it.
            reclaim_worker(worker.handle);
        }
    }

    fn apply_effective_request(&mut self, effective: &Mutex<EffectiveRequest>) {
        if let Ok(effective) = effective.lock() {
            self.request.url = effective.url.clone();
            self.request.headers = effective.headers.clone();
            self.final_url = effective.url.clone();
        }
    }
}

fn reclaim_worker(handle: JoinHandle<()>) {
    let _ = thread::Builder::new()
        .name("gx-http-worker-reaper".into())
        .spawn(move || {
            let _ = handle.join();
        });
}

impl Read for HttpMediaSource {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        if target.is_empty() {
            return Ok(0);
        }

        loop {
            if self.chunk_offset < self.current_chunk.len() {
                let available = &self.current_chunk[self.chunk_offset..];
                let count = available.len().min(target.len());
                target[..count].copy_from_slice(&available[..count]);
                self.chunk_offset += count;
                self.position += count as u64;
                return Ok(count);
            }

            let worker = self
                .worker
                .as_ref()
                .ok_or_else(|| io::Error::new(ErrorKind::BrokenPipe, "HTTP worker is stopped"))?;
            match worker.receiver.recv_timeout(WORKER_MESSAGE_TIMEOUT) {
                Ok(StreamMessage::Data(chunk)) => {
                    self.current_chunk = chunk;
                    self.chunk_offset = 0;
                }
                Ok(StreamMessage::End) => {
                    self.reached_end = true;
                    return Ok(0);
                }
                Ok(StreamMessage::Error(message)) => {
                    return Err(io::Error::other(message));
                }
                Err(RecvTimeoutError::Timeout) => {
                    worker.cancel.store(true, Ordering::Release);
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "HTTP worker produced no data before the idle deadline",
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "HTTP worker disconnected",
                    ));
                }
            }
        }
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => add_signed(self.position, delta)?,
            SeekFrom::End(delta) => {
                let len = self.total_len.ok_or_else(|| {
                    io::Error::new(ErrorKind::Unsupported, "HTTP content length is unknown")
                })?;
                add_signed(len, delta)?
            }
        };
        if target == self.position {
            return Ok(target);
        }
        if !self.supports_range {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                "HTTP server does not advertise byte-range support",
            ));
        }
        self.restart(target)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(target)
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        self.supports_range
    }

    fn byte_len(&self) -> Option<u64> {
        self.total_len
    }
}

impl Drop for HttpMediaSource {
    fn drop(&mut self) {
        if !self.reached_end
            && let Some(plan) = &self.cache_plan
        {
            plan.invalidate();
        }
        self.stop_worker();
    }
}

pub fn decode_http_window(
    request: ResolvedMediaRequest,
    start_seconds: f64,
    max_frames: usize,
) -> Result<HttpDecodeReport> {
    let source = HttpMediaSource::new(request)?;
    let metrics = source.metrics_handle();
    let final_url = source.final_url().to_string();
    let total_bytes = source.total_len();
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("failed to probe progressive HTTP media")?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .context("HTTP media contains no default audio track")?;
    if track.codec_params.codec == CODEC_TYPE_NULL {
        bail!("HTTP media default track has no supported codec");
    }
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let sample_rate = codec_params
        .sample_rate
        .context("HTTP media does not declare a sample rate")?;
    let channels = codec_params
        .channels
        .context("HTTP media does not declare a channel layout")?
        .count();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .context("failed to create HTTP media decoder")?;

    let mut seek_discard_frames = 0usize;
    let mut seek_discarded_frames = 0usize;
    if start_seconds > 0.0 {
        let seeked = format
            .seek(
                // Symphonia's MP3 accurate seek intentionally parses from the current position and
                // may download most of an online file. Coarse mode performs a byte-level seek;
                // the decoder then discards the small timestamp delta below.
                SeekMode::Coarse,
                SeekTo::Time {
                    time: start_seconds.into(),
                    track_id: Some(track_id),
                },
            )
            .with_context(|| format!("failed to Range seek HTTP media to {start_seconds:.3}s"))?;
        seek_discard_frames = seeked.required_ts.saturating_sub(seeked.actual_ts) as usize;
        seek_discarded_frames = seek_discard_frames;
        decoder.reset();
    }

    let wanted_samples = max_frames.saturating_mul(channels);
    let mut samples = Vec::with_capacity(wanted_samples);
    let mut sample_buffer = None;
    while samples.len() < wanted_samples {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error).context("failed to read progressive HTTP packet"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(error).context("failed to decode progressive HTTP packet"),
        };
        if sample_buffer.is_none() {
            sample_buffer = Some(SampleBuffer::<f32>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let buffer = sample_buffer.as_mut().expect("sample buffer initialized");
        buffer.copy_interleaved_ref(decoded);
        let mut decoded_samples = buffer.samples();
        if seek_discard_frames > 0 {
            let available_frames = decoded_samples.len() / channels;
            let discard_frames = available_frames.min(seek_discard_frames);
            decoded_samples = &decoded_samples[discard_frames * channels..];
            seek_discard_frames -= discard_frames;
        }
        let remaining = wanted_samples - samples.len();
        samples.extend_from_slice(&decoded_samples[..decoded_samples.len().min(remaining)]);
    }
    if samples.is_empty() {
        bail!("progressive HTTP decode produced no PCM samples");
    }
    let peak = samples
        .iter()
        .fold(0.0f32, |current, value| current.max(value.abs()));

    Ok(HttpDecodeReport {
        final_url,
        total_bytes,
        sample_rate,
        channels,
        decoded_frames: samples.len() / channels,
        seek_discarded_frames,
        peak,
        metrics: metrics.snapshot(),
    })
}

struct NetworkWorkerArgs {
    request: ResolvedMediaRequest,
    initial_offset: u64,
    sender: crossbeam_channel::Sender<StreamMessage>,
    ready_sender: crossbeam_channel::Sender<Result<ReadyInfo, String>>,
    cancel: Arc<AtomicBool>,
    metrics: Arc<StreamMetrics>,
    cache_writer: Option<CacheWriter>,
    effective_request: Arc<Mutex<EffectiveRequest>>,
}

fn network_worker(args: NetworkWorkerArgs) {
    let NetworkWorkerArgs {
        request,
        initial_offset,
        sender,
        ready_sender,
        cancel,
        metrics,
        mut cache_writer,
        effective_request,
    } = args;
    let mut offset = initial_offset;
    let mut active_url = request.url.clone();
    let mut active_headers = request.headers.clone();
    let mut total_len = None;
    let mut identity: Option<EntityIdentity> = None;
    let mut ready_sent = false;
    let mut reconnects = 0;

    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        metrics.requests.fetch_add(1, Ordering::Relaxed);
        if offset > 0 {
            metrics.range_requests.fetch_add(1, Ordering::Relaxed);
        }

        let response = match send_request(
            &mut active_headers,
            active_url.clone(),
            offset,
            identity.as_ref().and_then(EntityIdentity::if_range),
            &effective_request,
        ) {
            Ok(response) => response,
            Err(error) => {
                if !ready_sent {
                    let _ = ready_sender.send(Err(error));
                } else {
                    let _ = sender.send(StreamMessage::Error(error));
                }
                return;
            }
        };
        active_url = response.url().clone();
        if let Ok(mut effective) = effective_request.lock() {
            effective.url = active_url.clone();
            effective.headers = active_headers.clone();
        }
        let response_info = match inspect_response(&response, offset, total_len, identity.as_ref())
        {
            Ok(info) => info,
            Err(message) => {
                if !ready_sent {
                    let _ = ready_sender.send(Err(message));
                } else {
                    let _ = sender.send(StreamMessage::Error(message));
                }
                return;
            }
        };
        if identity.is_none() {
            identity = Some(response_info.identity.clone());
        }
        total_len = response_info.total_len.or(total_len);
        if !ready_sent {
            let _ = ready_sender.send(Ok(ReadyInfo {
                final_url: active_url.clone(),
                total_len,
                supports_range: response_info.supports_range,
            }));
            ready_sent = true;
        }

        let mut response = response;
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let outcome = loop {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            match response.read(&mut buffer) {
                Ok(0) => {
                    if response_info
                        .expected_end_exclusive
                        .is_some_and(|end| offset < end)
                    {
                        break Err(format!(
                            "connection ended early at byte {offset} of response ending at {}",
                            response_info.expected_end_exclusive.unwrap()
                        ));
                    }
                    if total_len.is_some_and(|len| offset < len) {
                        break Err(format!(
                            "connection ended early at byte {offset} of {}",
                            total_len.unwrap()
                        ));
                    }
                    break Ok(());
                }
                Ok(count) => {
                    let next_offset = offset.saturating_add(count as u64);
                    if response_info
                        .expected_end_exclusive
                        .is_some_and(|end| next_offset > end)
                        || total_len.is_some_and(|len| next_offset > len)
                    {
                        break Err(format!(
                            "HTTP body exceeded its declared byte range at byte {offset}"
                        ));
                    }
                    if let Some(writer) = cache_writer.as_mut() {
                        writer.append(&buffer[..count]);
                    }
                    offset = next_offset;
                    metrics
                        .bytes_received
                        .fetch_add(count as u64, Ordering::Relaxed);
                    let mut message = StreamMessage::Data(buffer[..count].to_vec());
                    loop {
                        match sender.send_timeout(message, Duration::from_millis(100)) {
                            Ok(()) => break,
                            Err(SendTimeoutError::Timeout(returned)) => {
                                metrics.backpressure_waits.fetch_add(1, Ordering::Relaxed);
                                if cancel.load(Ordering::Acquire) {
                                    return;
                                }
                                message = returned;
                            }
                            Err(SendTimeoutError::Disconnected(_)) => return,
                        }
                    }
                }
                Err(error) => {
                    break Err(format!("HTTP body read failed at byte {offset}: {error}"));
                }
            }
        };

        match outcome {
            Ok(()) => {
                if let Some(writer) = cache_writer.take() {
                    // This only stages the transfer. The decoder commits it after clean EOF.
                    writer.finish(total_len);
                }
                let _ = sender.send(StreamMessage::End);
                return;
            }
            Err(_message) if reconnects < MAX_RECONNECTS => {
                reconnects += 1;
                metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(100 * reconnects));
            }
            Err(message) => {
                let _ = sender.send(StreamMessage::Error(format!(
                    "{message}; reconnect budget exhausted"
                )));
                return;
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EntityIdentity {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl EntityIdentity {
    fn from_response(response: &Response) -> Self {
        Self {
            etag: header_text(response, ETAG),
            last_modified: header_text(response, LAST_MODIFIED),
        }
    }

    fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|etag| !etag.trim_start().starts_with("W/"))
            .or(self.last_modified.as_deref())
    }

    fn ensure_consistent_with(&self, current: &Self) -> Result<(), String> {
        if let (Some(expected), Some(actual)) = (&self.etag, &current.etag)
            && expected != actual
        {
            return Err("media entity ETag changed during reconnect".into());
        }
        if let (Some(expected), Some(actual)) = (&self.last_modified, &current.last_modified)
            && expected != actual
        {
            return Err("media entity Last-Modified changed during reconnect".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRangeInfo {
    start: u64,
    end: u64,
    total: Option<u64>,
}

struct ResponseInfo {
    total_len: Option<u64>,
    expected_end_exclusive: Option<u64>,
    supports_range: bool,
    identity: EntityIdentity,
}

fn inspect_response(
    response: &Response,
    offset: u64,
    known_total: Option<u64>,
    known_identity: Option<&EntityIdentity>,
) -> Result<ResponseInfo, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!("media request returned HTTP {status}"));
    }
    if offset > 0 && status != StatusCode::PARTIAL_CONTENT {
        return Err(format!(
            "server ignored Range/If-Range request at byte {offset}, returned {status}"
        ));
    }

    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range);
    let (total_len, expected_end_exclusive) = if status == StatusCode::PARTIAL_CONTENT {
        let range = content_range.ok_or_else(|| {
            format!("partial response at byte {offset} has no valid Content-Range")
        })?;
        if range.start != offset {
            return Err(format!(
                "partial response starts at byte {}, expected {offset}",
                range.start
            ));
        }
        if range.end < range.start || range.total.is_some_and(|total| range.end >= total) {
            return Err("partial response contains an impossible Content-Range".into());
        }
        let body_len = range.end - range.start + 1;
        if response
            .content_length()
            .is_some_and(|length| length != body_len)
        {
            return Err("partial response Content-Length disagrees with Content-Range".into());
        }
        (range.total, range.end.checked_add(1))
    } else {
        (response.content_length(), response.content_length())
    };

    if let (Some(expected), Some(actual)) = (known_total, total_len)
        && expected != actual
    {
        return Err(format!(
            "media entity length changed during reconnect ({expected} -> {actual})"
        ));
    }
    let identity = EntityIdentity::from_response(response);
    if let Some(known) = known_identity {
        known.ensure_consistent_with(&identity)?;
    }
    let supports_range = status == StatusCode::PARTIAL_CONTENT
        || response
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
    Ok(ResponseInfo {
        total_len,
        expected_end_exclusive,
        supports_range,
        identity,
    })
}

fn header_text(response: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn send_request(
    headers: &mut Vec<HttpHeader>,
    mut url: reqwest::Url,
    offset: u64,
    if_range: Option<&str>,
    effective_request: &Mutex<EffectiveRequest>,
) -> Result<Response, String> {
    for redirect_count in 0..=MAX_MEDIA_REDIRECTS {
        let resolved = resolve_media_destination(&url)
            .map_err(|error| format!("media destination denied: {error}"))?;
        let host = url
            .host_str()
            .ok_or_else(|| "media URL has no host".to_owned())?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            // The blocking response applies this deadline independently to each body read. It
            // therefore acts as an idle timeout without limiting the total duration of a song.
            .timeout(STREAM_IDLE_TIMEOUT)
            .resolve(host, resolved)
            .build()
            .map_err(|error| format!("failed to build pinned media client: {error}"))?;
        let mut builder = client.get(url.clone());
        for header in headers.iter().filter(|header| {
            !header.name.eq_ignore_ascii_case(RANGE.as_str())
                && !header.name.eq_ignore_ascii_case(IF_RANGE.as_str())
        }) {
            let name = HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|error| format!("invalid media request header name: {error}"))?;
            let value = HeaderValue::from_str(&header.value)
                .map_err(|error| format!("invalid media request header value: {error}"))?;
            builder = builder.header(name, value);
        }
        if offset > 0 {
            builder = builder.header(RANGE, format!("bytes={offset}-"));
            if let Some(if_range) = if_range {
                builder = builder.header(IF_RANGE, if_range);
            }
        }
        let response = builder
            .send()
            .map_err(|error| format!("media request failed: {error}"))?;
        if !response.status().is_redirection() {
            update_effective_request(effective_request, response.url(), headers);
            return Ok(response);
        }
        if redirect_count == MAX_MEDIA_REDIRECTS {
            return Err("media redirect limit exceeded".into());
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| "media redirect has no valid Location header".to_owned())?;
        let next = url
            .join(location)
            .map_err(|_| "media redirect Location is invalid".to_owned())?;
        if !same_origin(&url, &next) {
            strip_sensitive_headers(headers);
        }
        url = next;
        // Publish sanitization at the redirect boundary, before the next socket request. A
        // concurrent Seek can now only inherit the already-sanitized header set.
        update_effective_request(effective_request, &url, headers);
    }
    Err("media redirect limit exceeded".into())
}

fn update_effective_request(
    effective_request: &Mutex<EffectiveRequest>,
    url: &reqwest::Url,
    headers: &[HttpHeader],
) {
    if let Ok(mut effective) = effective_request.lock() {
        effective.url = url.clone();
        effective.headers = headers.to_vec();
    }
}

fn strip_sensitive_headers(headers: &mut Vec<HttpHeader>) {
    headers.retain(|header| {
        !header.name.eq_ignore_ascii_case(AUTHORIZATION.as_str())
            && !header
                .name
                .eq_ignore_ascii_case(PROXY_AUTHORIZATION.as_str())
            && !header.name.eq_ignore_ascii_case(COOKIE.as_str())
    });
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn resolve_media_destination(url: &reqwest::Url) -> Result<std::net::SocketAddr, String> {
    match validate_and_resolve(url) {
        Ok(address) => Ok(address),
        #[cfg(feature = "test-private-network")]
        Err(_) => {
            let host = url
                .host_str()
                .ok_or_else(|| "media URL has no host".to_owned())?;
            let ip = host
                .parse::<IpAddr>()
                .map_err(|_| "test private-network bypass only accepts IP literals".to_owned())?;
            let port = url
                .port_or_known_default()
                .ok_or_else(|| "media URL has no port".to_owned())?;
            Ok(SocketAddr::new(ip, port))
        }
        #[cfg(not(feature = "test-private-network"))]
        Err(error) => Err(error.to_string()),
    }
}

fn parse_content_range(value: &str) -> Option<ContentRangeInfo> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    if range == "*" {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    Some(ContentRangeInfo {
        start: start.parse().ok()?,
        end: end.parse().ok()?,
        total: (total != "*").then(|| total.parse().ok()).flatten(),
    })
}

fn add_signed(base: u64, delta: i64) -> io::Result<u64> {
    if delta >= 0 {
        base.checked_add(delta as u64)
    } else {
        base.checked_sub(delta.unsigned_abs())
    }
    .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "seek target is outside the stream"))
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
    fn parses_complete_content_ranges() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some(ContentRangeInfo {
                start: 100,
                end: 199,
                total: Some(1000),
            })
        );
        assert_eq!(
            parse_content_range("bytes 100-199/*"),
            Some(ContentRangeInfo {
                start: 100,
                end: 199,
                total: None,
            })
        );
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(parse_content_range("invalid"), None);
    }

    #[test]
    fn redirect_sanitization_is_sticky_for_future_requests() {
        let mut headers = vec![
            HttpHeader {
                name: "Authorization".into(),
                value: "secret".into(),
            },
            HttpHeader {
                name: "Cookie".into(),
                value: "session=secret".into(),
            },
            HttpHeader {
                name: "User-Agent".into(),
                value: "GXPlayer".into(),
            },
        ];
        strip_sensitive_headers(&mut headers);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name, "User-Agent");
    }

    #[test]
    fn entity_identity_rejects_validator_changes() {
        let original = EntityIdentity {
            etag: Some("\"one\"".into()),
            last_modified: None,
        };
        let changed = EntityIdentity {
            etag: Some("\"two\"".into()),
            last_modified: None,
        };
        assert!(original.ensure_consistent_with(&changed).is_err());
        assert_eq!(original.if_range(), Some("\"one\""));
    }

    #[test]
    fn signed_seek_math_rejects_underflow() {
        assert_eq!(add_signed(10, -4).unwrap(), 6);
        assert!(add_signed(3, -4).is_err());
    }

    #[cfg(not(feature = "test-private-network"))]
    #[test]
    fn production_media_policy_rejects_private_destinations() {
        let error = resolve_media_destination(
            &reqwest::Url::parse("http://127.0.0.1/private.mp3").unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("private"));
    }

    #[test]
    fn media_origins_include_scheme_host_and_port() {
        assert!(same_origin(
            &reqwest::Url::parse("https://example.com/a").unwrap(),
            &reqwest::Url::parse("https://EXAMPLE.com:443/b").unwrap()
        ));
        assert!(!same_origin(
            &reqwest::Url::parse("https://example.com/a").unwrap(),
            &reqwest::Url::parse("http://example.com/a").unwrap()
        ));
    }
}
