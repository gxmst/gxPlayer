//! Bounded progressive HTTP media source for Symphonia.
//!
//! Network I/O runs on a dedicated worker. The decoder only blocks on a bounded byte channel and
//! asks the source to restart at a byte offset when Symphonia performs a seek.

use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, SendTimeoutError, bounded};
use gx_contracts::ResolvedMediaRequest;
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, HeaderName, HeaderValue, RANGE};
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

struct WorkerState {
    receiver: Receiver<StreamMessage>,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

pub struct HttpMediaSource {
    request: ResolvedMediaRequest,
    client: Client,
    worker: Option<WorkerState>,
    current_chunk: Vec<u8>,
    chunk_offset: usize,
    position: u64,
    total_len: Option<u64>,
    supports_range: bool,
    final_url: reqwest::Url,
    metrics: Arc<StreamMetrics>,
}

impl HttpMediaSource {
    pub fn new(request: ResolvedMediaRequest) -> Result<Self> {
        if request.is_expired_at(unix_time_ms()) {
            bail!("resolved media request has expired and must be resolved again");
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build streaming HTTP client")?;
        let metrics = Arc::new(StreamMetrics::default());
        let placeholder_url = request.url.clone();
        let mut source = Self {
            request,
            client,
            worker: None,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            position: 0,
            total_len: None,
            supports_range: false,
            final_url: placeholder_url,
            metrics,
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
        self.stop_worker();

        let (sender, receiver) = bounded(CHANNEL_CAPACITY);
        let (ready_sender, ready_receiver) = bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let request = self.request_for_restart();
        let client = self.client.clone();
        let metrics = Arc::clone(&self.metrics);
        let cancel_for_worker = Arc::clone(&cancel);
        let handle = thread::Builder::new()
            .name("gx-http-stream".into())
            .spawn(move || {
                network_worker(
                    client,
                    request,
                    offset,
                    sender,
                    ready_sender,
                    cancel_for_worker,
                    metrics,
                );
            })
            .context("failed to spawn HTTP streaming worker")?;

        let ready = ready_receiver
            .recv_timeout(Duration::from_secs(10))
            .context("HTTP streaming worker did not become ready")?
            .map_err(anyhow::Error::msg)?;
        self.final_url = ready.final_url;
        self.total_len = ready.total_len;
        self.supports_range = ready.supports_range;
        self.position = offset;
        self.current_chunk.clear();
        self.chunk_offset = 0;
        self.worker = Some(WorkerState {
            receiver,
            cancel,
            handle,
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
            // Dropping a JoinHandle detaches the worker. A blocking socket read cannot be
            // synchronously cancelled by reqwest's blocking API; the worker observes cancellation
            // before the next read or bounded-channel send. Phase 0 will own workers in a runtime
            // with explicit cancellation and shutdown accounting.
            drop(worker.handle);
        }
    }
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
            match worker.receiver.recv() {
                Ok(StreamMessage::Data(chunk)) => {
                    self.current_chunk = chunk;
                    self.chunk_offset = 0;
                }
                Ok(StreamMessage::End) => return Ok(0),
                Ok(StreamMessage::Error(message)) => {
                    return Err(io::Error::other(message));
                }
                Err(_) => {
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

fn network_worker(
    client: Client,
    request: ResolvedMediaRequest,
    initial_offset: u64,
    sender: crossbeam_channel::Sender<StreamMessage>,
    ready_sender: crossbeam_channel::Sender<Result<ReadyInfo, String>>,
    cancel: Arc<AtomicBool>,
    metrics: Arc<StreamMetrics>,
) {
    let mut offset = initial_offset;
    let mut active_url = request.url.clone();
    let mut total_len = None;
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

        let response = match send_request(&client, &request, active_url.clone(), offset) {
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
        let status = response.status();
        if offset > 0 && status != StatusCode::PARTIAL_CONTENT {
            let message =
                format!("server ignored Range request at byte {offset}, returned {status}");
            if !ready_sent {
                let _ = ready_sender.send(Err(message));
            } else {
                let _ = sender.send(StreamMessage::Error(message));
            }
            return;
        }
        if !(status.is_success()) {
            let message = format!("media request returned HTTP {status}");
            if !ready_sent {
                let _ = ready_sender.send(Err(message));
            } else {
                let _ = sender.send(StreamMessage::Error(message));
            }
            return;
        }

        total_len = response_total_len(&response, offset).or(total_len);
        let supports_range = status == StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
        if !ready_sent {
            let _ = ready_sender.send(Ok(ReadyInfo {
                final_url: active_url.clone(),
                total_len,
                supports_range,
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
                    if total_len.is_some_and(|len| offset < len) {
                        break Err(format!(
                            "connection ended early at byte {offset} of {}",
                            total_len.unwrap()
                        ));
                    }
                    break Ok(());
                }
                Ok(count) => {
                    offset += count as u64;
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

fn send_request(
    client: &Client,
    request: &ResolvedMediaRequest,
    url: reqwest::Url,
    offset: u64,
) -> Result<Response, String> {
    let mut builder = client.get(url);
    for header in &request.headers {
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| format!("invalid media request header name: {error}"))?;
        let value = HeaderValue::from_str(&header.value)
            .map_err(|error| format!("invalid media request header value: {error}"))?;
        builder = builder.header(name, value);
    }
    if offset > 0 {
        builder = builder.header(RANGE, format!("bytes={offset}-"));
    }
    builder
        .send()
        .map_err(|error| format!("media request failed: {error}"))
}

fn response_total_len(response: &Response, offset: u64) -> Option<u64> {
    if let Some(total) = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range_total)
    {
        return Some(total);
    }
    response.content_length().map(|length| offset + length)
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.parse().ok()
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
    fn parses_content_range_total() {
        assert_eq!(parse_content_range_total("bytes 100-199/1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes */1000"), Some(1000));
        assert_eq!(parse_content_range_total("invalid"), None);
    }

    #[test]
    fn signed_seek_math_rejects_underflow() {
        assert_eq!(add_signed(10, -4).unwrap(), 6);
        assert!(add_signed(3, -4).is_err());
    }
}
