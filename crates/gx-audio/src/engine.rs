use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};
use gx_cache::CacheWritePlan;
use gx_contracts::{MediaType, PlaybackStatus, ResolvedMediaRequest};
use gx_dsp::{CrossfeedSettings, DspChain, DspSettings, HrtfSettings, LimiterSettings};
use gx_streaming::{
    HttpMediaSource, StreamInterruption, StreamInterruptionGuard, StreamingDiagnosticQueue,
};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use serde::{Deserialize, Serialize};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{
    CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_VORBIS, CodecType,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::meta::StandardTagKey;

use super::{
    OpenedMedia, RateAdapter, choose_output_config, open_media, open_media_source, remap_channels,
    seek_media, seek_media_coarse,
};
use crate::mmcss::AudioThreadPriority;

const COMMAND_CAPACITY: usize = 64;
const RING_SECONDS: f64 = 0.75;
const PREBUFFER_SECONDS: f64 = 0.5;
const DIAGNOSTIC_CAPACITY: usize = 128;
const OUTPUT_DEVICE_EVENT_CAPACITY: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineDiagnostic {
    pub category: &'static str,
    pub source: &'static str,
    pub summary: String,
    pub generation: Option<u64>,
}

#[derive(Clone)]
struct EngineDiagnosticQueue {
    inner: Arc<Mutex<VecDeque<EngineDiagnostic>>>,
}

impl Default for EngineDiagnosticQueue {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(DIAGNOSTIC_CAPACITY))),
        }
    }
}

impl EngineDiagnosticQueue {
    fn push(&self, diagnostic: EngineDiagnostic) {
        let mut diagnostics = self.inner.lock().unwrap();
        if diagnostics.len() == DIAGNOSTIC_CAPACITY {
            diagnostics.pop_front();
        }
        diagnostics.push_back(diagnostic);
    }

    fn drain(&self) -> Vec<EngineDiagnostic> {
        self.inner.lock().unwrap().drain(..).collect()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutputDeviceFallbackEvent {
    pub unavailable_device: String,
    pub fallback_device: Option<String>,
}

#[derive(Clone)]
struct OutputDeviceEventQueue {
    inner: Arc<Mutex<VecDeque<OutputDeviceFallbackEvent>>>,
}

impl Default for OutputDeviceEventQueue {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(
                OUTPUT_DEVICE_EVENT_CAPACITY,
            ))),
        }
    }
}

impl OutputDeviceEventQueue {
    fn push(&self, event: OutputDeviceFallbackEvent) {
        let mut events = self.inner.lock().unwrap();
        if events.len() == OUTPUT_DEVICE_EVENT_CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }

    fn drain(&self) -> Vec<OutputDeviceFallbackEvent> {
        self.inner.lock().unwrap().drain(..).collect()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub location: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub online: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    #[default]
    Music,
    CinemaGame,
}

/// Playback progression mode for the engine queue.
///
/// Queue logic lives on the worker thread only — never on the cpal callback path.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    /// Play in order; stop after the last track.
    #[default]
    Sequential,
    /// Play in order; wrap to index 0 after the last track.
    RepeatAll,
    /// When a track ends, restart the same index from 0.
    /// Next/Previous still move to adjacent tracks.
    RepeatOne,
    /// Pick a random unplayed index each advance; reset after a full cycle.
    Shuffle,
}

impl AudioMode {
    fn dsp_control(self) -> DspControlState {
        match self {
            Self::Music => DspControlState::default(),
            Self::CinemaGame => DspControlState {
                settings: DspSettings {
                    enabled: true,
                    eq_enabled: false,
                    crossfeed: CrossfeedSettings {
                        enabled: true,
                        amount: 0.18,
                        delay_ms: 0.28,
                        cutoff_hz: 700.0,
                    },
                    hrtf: HrtfSettings {
                        enabled: true,
                        mix: 0.72,
                        output_gain_db: -6.0,
                    },
                    limiter: LimiterSettings {
                        enabled: true,
                        ..LimiterSettings::default()
                    },
                    ..DspSettings::default()
                },
                active_preset_id: DspPresetId::Spatial,
                intensity: 0.5,
                spatial_amount: 1.0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DspPresetId {
    #[default]
    Bypass,
    HeadphoneDaily,
    Vocal,
    Bass,
    Spatial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DspControlState {
    pub settings: DspSettings,
    pub active_preset_id: DspPresetId,
    pub intensity: f32,
    pub spatial_amount: f32,
}

impl Default for DspControlState {
    fn default() -> Self {
        Self {
            settings: DspSettings::default(),
            active_preset_id: DspPresetId::Bypass,
            intensity: 0.5,
            spatial_amount: 0.5,
        }
    }
}

impl DspControlState {
    pub fn from_audio_mode(mode: AudioMode) -> Self {
        mode.dsp_control()
    }

    pub fn from_settings(settings: DspSettings) -> Self {
        let active_preset_id = if !settings.enabled {
            DspPresetId::Bypass
        } else if settings.hrtf.enabled {
            DspPresetId::Spatial
        } else if settings.eq_enabled {
            DspPresetId::Vocal
        } else {
            DspPresetId::HeadphoneDaily
        };
        Self {
            settings,
            active_preset_id,
            intensity: 0.5,
            spatial_amount: 0.5,
        }
    }

    pub fn audio_mode(&self) -> AudioMode {
        if self.active_preset_id == DspPresetId::Spatial {
            AudioMode::CinemaGame
        } else {
            AudioMode::Music
        }
    }

    pub fn validate_product(&self) -> Result<()> {
        if !self.intensity.is_finite() || !(0.0..=1.0).contains(&self.intensity) {
            bail!("DSP intensity must be in the range 0..=1");
        }
        if !self.spatial_amount.is_finite() || !(0.0..=1.0).contains(&self.spatial_amount) {
            bail!("DSP spatial amount must be in the range 0..=1");
        }
        if self.settings.eq_bands.len() > 10 {
            bail!("product DSP supports at most 10 EQ bands");
        }
        if self
            .settings
            .eq_bands
            .iter()
            .any(|band| !band.gain_db.is_finite() || band.gain_db.abs() > 12.0)
        {
            bail!("product EQ gain must stay in the range -12..=12 dB");
        }
        match self.active_preset_id {
            DspPresetId::Bypass if self.settings.enabled => {
                bail!("bypass preset must disable the complete DSP chain")
            }
            DspPresetId::Bypass => {}
            _ if !self.settings.enabled => bail!("non-bypass preset must enable DSP"),
            DspPresetId::Spatial if !self.settings.hrtf.enabled => {
                bail!("spatial preset must enable HRTF")
            }
            DspPresetId::Spatial => {}
            _ if self.settings.hrtf.enabled => {
                bail!("only the spatial preset may enable HRTF")
            }
            _ => {}
        }
        if self.settings.hrtf.enabled && !self.settings.limiter.enabled {
            bail!("HRTF requires the limiter to remain enabled");
        }
        DspChain::new(48_000, 2, self.settings.clone())?;
        Ok(())
    }
}

fn validate_dsp_control_for_output(
    control: &DspControlState,
    output_sample_rate: Option<u32>,
) -> Result<()> {
    control.validate_product()?;
    if let Some(sample_rate) = output_sample_rate {
        DspChain::new(sample_rate, 2, control.settings.clone()).with_context(|| {
            format!("DSP settings are invalid at the active {sample_rate} Hz output rate")
        })?;
    }
    Ok(())
}

#[derive(Clone)]
enum PlaybackSource {
    Local(PathBuf),
    Online {
        request: ResolvedMediaRequest,
        cache_plan: Option<CacheWritePlan>,
    },
}

fn playback_source_label(source: &PlaybackSource, online: bool) -> &'static str {
    match source {
        PlaybackSource::Online { .. } => "online",
        PlaybackSource::Local(_) if online => "cache",
        PlaybackSource::Local(_) => "local",
    }
}

fn playback_error_code(error: &anyhow::Error) -> &'static str {
    let error = error.to_string().to_ascii_lowercase();
    if error.contains("timed out") || error.contains("timeout") {
        "timeout"
    } else if error.contains("output device") || error.contains("default audio output") {
        "output_device"
    } else if error.contains("http") || error.contains("media request") {
        "network"
    } else if error.contains("decode") || error.contains("codec") {
        "decode"
    } else if error.contains("probe") || error.contains("format") {
        "media_format"
    } else if error.contains("sample rate") || error.contains("channel") {
        "media_spec"
    } else if error.contains("dsp") || error.contains("resampl") {
        "audio_processing"
    } else if error.contains("permission") || error.contains("not found") {
        "io"
    } else {
        "failed"
    }
}

fn is_output_device_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("output device")
        || message.contains("default audio output")
        || error
            .chain()
            .any(|cause| cause.downcast_ref::<cpal::BuildStreamError>().is_some())
}

fn is_stream_interruption(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::ConnectionAborted)
            || cause.downcast_ref::<SymphoniaError>().is_some_and(|error| {
                matches!(
                    error,
                    SymphoniaError::IoError(error)
                        if error.kind() == ErrorKind::ConnectionAborted
                )
            })
    })
}

fn is_expected_stream_interruption(
    error: &anyhow::Error,
    interruption: &StreamInterruption,
) -> bool {
    interruption.is_pending() && is_stream_interruption(error)
}

#[derive(Clone)]
struct EngineQueueItem {
    public: QueueItem,
    source: PlaybackSource,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EngineSnapshot {
    pub status: PlaybackStatus,
    pub queue: Vec<QueueItem>,
    pub queue_index: Option<usize>,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub volume: f32,
    pub audio_mode: AudioMode,
    pub play_mode: PlayMode,
    pub dsp_settings: DspSettings,
    pub active_preset_id: DspPresetId,
    pub intensity: f32,
    pub spatial_amount: f32,
    pub generation: u64,
    pub underrun_callbacks: u64,
    pub output_sample_rate: Option<u32>,
    pub source_sample_rate: Option<u32>,
    pub source_bit_depth: Option<u32>,
    pub source_channels: Option<u16>,
    pub error: Option<String>,
    pub output_device: Option<String>,
}

impl Default for EngineSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            queue: Vec::new(),
            queue_index: None,
            position_seconds: 0.0,
            duration_seconds: None,
            volume: 1.0,
            audio_mode: AudioMode::Music,
            play_mode: PlayMode::Sequential,
            dsp_settings: DspSettings::default(),
            active_preset_id: DspPresetId::Bypass,
            intensity: 0.5,
            spatial_amount: 0.5,
            generation: 0,
            underrun_callbacks: 0,
            output_sample_rate: None,
            source_sample_rate: None,
            source_bit_depth: None,
            source_channels: None,
            error: None,
            output_device: None,
        }
    }
}

enum EngineCommand {
    Load {
        items: Vec<EngineQueueItem>,
        start_index: usize,
    },
    Enqueue(Vec<EngineQueueItem>),
    ReplaceCurrent(EngineQueueItem),
    Jump(usize),
    Remove(usize),
    Reorder {
        from: usize,
        to: usize,
    },
    ClearQueue,
    Play,
    Pause,
    Seek(f64),
    SetVolume(f32),
    SetPlayMode(PlayMode),
    SetDspControl {
        control: DspControlState,
        reply: Sender<std::result::Result<(), String>>,
    },
    SetOutputDevice(Option<String>),
    Next,
    Previous,
    Shutdown,
}

impl EngineCommand {
    fn interrupts_stream(&self) -> bool {
        matches!(
            self,
            Self::Load { .. }
                | Self::ReplaceCurrent(_)
                | Self::Jump(_)
                | Self::ClearQueue
                | Self::Pause
                | Self::Seek(_)
                | Self::SetDspControl { .. }
                | Self::SetOutputDevice(_)
                | Self::Next
                | Self::Previous
                | Self::Shutdown
        )
    }
}

struct QueuedEngineCommand {
    command: EngineCommand,
    interruption: Option<StreamInterruptionGuard>,
}

pub struct LocalAudioEngine {
    commands: Sender<QueuedEngineCommand>,
    stream_interruption: StreamInterruption,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    diagnostics: EngineDiagnosticQueue,
    streaming_diagnostics: StreamingDiagnosticQueue,
    output_device_events: OutputDeviceEventQueue,
    ab_dry_active: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LocalAudioEngine {
    pub fn new() -> Result<Self> {
        let (commands, receiver) = bounded(COMMAND_CAPACITY);
        let snapshot = Arc::new(Mutex::new(EngineSnapshot::default()));
        let snapshot_for_worker = Arc::clone(&snapshot);
        let diagnostics = EngineDiagnosticQueue::default();
        let diagnostics_for_worker = diagnostics.clone();
        let streaming_diagnostics = StreamingDiagnosticQueue::default();
        let streaming_diagnostics_for_worker = streaming_diagnostics.clone();
        let stream_interruption = StreamInterruption::default();
        let interruption_for_worker = stream_interruption.clone();
        let output_device_events = OutputDeviceEventQueue::default();
        let output_device_events_for_worker = output_device_events.clone();
        let ab_dry_active = Arc::new(AtomicBool::new(false));
        let ab_dry_active_for_worker = Arc::clone(&ab_dry_active);
        let worker = thread::Builder::new()
            .name("gx-local-audio-engine".into())
            .spawn(move || {
                run_worker(
                    receiver,
                    snapshot_for_worker,
                    diagnostics_for_worker,
                    streaming_diagnostics_for_worker,
                    interruption_for_worker,
                    output_device_events_for_worker,
                    ab_dry_active_for_worker,
                )
            })
            .context("failed to spawn local audio engine worker")?;
        Ok(Self {
            commands,
            stream_interruption,
            snapshot,
            diagnostics,
            streaming_diagnostics,
            output_device_events,
            ab_dry_active,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn load(&self, paths: Vec<PathBuf>) -> Result<()> {
        self.load_at(paths, 0)
    }

    pub fn load_at(&self, paths: Vec<PathBuf>, start_index: usize) -> Result<()> {
        if paths.is_empty() {
            bail!("at least one local audio path is required");
        }
        if start_index >= paths.len() {
            bail!(
                "start_index {start_index} is out of range for {} paths",
                paths.len()
            );
        }
        self.send(EngineCommand::Load {
            items: paths.into_iter().map(local_queue_item).collect(),
            start_index,
        })
    }

    pub fn enqueue(&self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("at least one local audio path is required to enqueue");
        }
        self.send(EngineCommand::Enqueue(
            paths.into_iter().map(local_queue_item).collect(),
        ))
    }

    pub fn load_resolved(&self, request: ResolvedMediaRequest, title: String) -> Result<()> {
        self.load_resolved_cached(request, title, None)
    }

    pub fn load_resolved_cached(
        &self,
        request: ResolvedMediaRequest,
        title: String,
        cache_plan: Option<CacheWritePlan>,
    ) -> Result<()> {
        if request.media_type == MediaType::Hls {
            bail!("HLS playback is not supported in v1");
        }
        let location = request.redacted_for_log();
        self.send(EngineCommand::Load {
            items: vec![EngineQueueItem {
                public: QueueItem {
                    location,
                    title,
                    duration_seconds: None,
                    online: true,
                },
                source: PlaybackSource::Online {
                    request,
                    cache_plan,
                },
            }],
            start_index: 0,
        })
    }

    pub fn load_cached_online(&self, path: PathBuf, title: String) -> Result<()> {
        self.send(EngineCommand::Load {
            items: vec![EngineQueueItem {
                public: QueueItem {
                    location: path.display().to_string(),
                    title,
                    duration_seconds: None,
                    online: true,
                },
                source: PlaybackSource::Local(path),
            }],
            start_index: 0,
        })
    }

    pub fn replace_current_resolved_cached(
        &self,
        request: ResolvedMediaRequest,
        title: String,
        cache_plan: Option<CacheWritePlan>,
    ) -> Result<()> {
        if request.media_type == MediaType::Hls {
            bail!("HLS playback is not supported in v1");
        }
        if self.snapshot().queue_index.is_none() {
            bail!("there is no current track to replace");
        }
        let location = request.redacted_for_log();
        self.send(EngineCommand::ReplaceCurrent(EngineQueueItem {
            public: QueueItem {
                location,
                title,
                duration_seconds: None,
                online: true,
            },
            source: PlaybackSource::Online {
                request,
                cache_plan,
            },
        }))
    }

    pub fn replace_current_cached_online(&self, path: PathBuf, title: String) -> Result<()> {
        if self.snapshot().queue_index.is_none() {
            bail!("there is no current track to replace");
        }
        self.send(EngineCommand::ReplaceCurrent(EngineQueueItem {
            public: QueueItem {
                location: path.display().to_string(),
                title,
                duration_seconds: None,
                online: true,
            },
            source: PlaybackSource::Local(path),
        }))
    }

    pub fn jump(&self, index: usize) -> Result<()> {
        self.send(EngineCommand::Jump(index))
    }

    pub fn remove_queue_item(&self, index: usize) -> Result<()> {
        self.send(EngineCommand::Remove(index))
    }

    pub fn reorder_queue(&self, from: usize, to: usize) -> Result<()> {
        self.send(EngineCommand::Reorder { from, to })
    }

    pub fn clear_queue(&self) -> Result<()> {
        self.send(EngineCommand::ClearQueue)
    }

    pub fn play(&self) -> Result<()> {
        self.send(EngineCommand::Play)
    }

    pub fn pause(&self) -> Result<()> {
        self.send(EngineCommand::Pause)
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            bail!("seek position must be a finite non-negative number");
        }
        self.send(EngineCommand::Seek(seconds))
    }

    pub fn set_volume(&self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be finite");
        }
        self.send(EngineCommand::SetVolume(volume.clamp(0.0, 1.0)))
    }

    pub fn set_dsp_settings(&self, settings: DspSettings) -> Result<()> {
        self.set_dsp_control(DspControlState::from_settings(settings))
    }

    pub fn set_dsp_control(&self, control: DspControlState) -> Result<()> {
        self.set_dsp_control_confirmed(control)
    }

    /// Applies a DSP control transaction on the worker before returning.
    ///
    /// `Ok(())` means either the active DSP chain was updated successfully, or the desired state
    /// was accepted and an output-topology/session rebuild was queued. An active-chain validation
    /// failure is returned to the caller and leaves the prior worker state authoritative.
    pub fn set_dsp_control_confirmed(&self, control: DspControlState) -> Result<()> {
        let output_sample_rate = self.snapshot.lock().unwrap().output_sample_rate;
        validate_dsp_control_for_output(&control, output_sample_rate)?;
        let (reply, confirmation) = bounded(1);
        self.send(EngineCommand::SetDspControl { control, reply })?;
        confirmation
            .recv()
            .map_err(|_| anyhow!("audio engine stopped before confirming DSP settings"))?
            .map_err(anyhow::Error::msg)
    }

    pub fn set_audio_mode(&self, mode: AudioMode) -> Result<()> {
        self.set_dsp_control_confirmed(DspControlState::from_audio_mode(mode))
    }

    pub fn set_ab_dry(&self, enabled: bool) {
        self.ab_dry_active.store(enabled, Ordering::Relaxed);
    }

    pub fn set_play_mode(&self, mode: PlayMode) -> Result<()> {
        self.send(EngineCommand::SetPlayMode(mode))
    }

    pub fn next(&self) -> Result<()> {
        self.send(EngineCommand::Next)
    }

    pub fn previous(&self) -> Result<()> {
        self.send(EngineCommand::Previous)
    }

    pub fn snapshot(&self) -> EngineSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn drain_diagnostics(&self) -> Vec<EngineDiagnostic> {
        let mut diagnostics = self.diagnostics.drain();
        diagnostics.extend(
            self.streaming_diagnostics
                .drain()
                .into_iter()
                .map(|diagnostic| EngineDiagnostic {
                    category: diagnostic.category,
                    source: diagnostic.source,
                    summary: diagnostic.summary,
                    generation: None,
                }),
        );
        diagnostics
    }

    pub fn drain_output_device_events(&self) -> Vec<OutputDeviceFallbackEvent> {
        self.output_device_events.drain()
    }

    pub fn output_devices(&self) -> Result<Vec<String>> {
        let host = cpal::default_host();
        let mut names = host
            .output_devices()
            .context("failed to enumerate output devices")?
            .filter_map(|device| device.name().ok())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn default_output_device_name(&self) -> Result<Option<String>> {
        let host = cpal::default_host();
        host.default_output_device()
            .map(|device| {
                device
                    .name()
                    .context("failed to read default output device name")
            })
            .transpose()
    }

    pub fn set_output_device(&self, name: Option<String>) -> Result<()> {
        if name.as_ref().is_some_and(|name| name.len() > 500) {
            bail!("output device name exceeds the size limit");
        }
        self.send(EngineCommand::SetOutputDevice(name))
    }

    fn send(&self, command: EngineCommand) -> Result<()> {
        let interruption = command
            .interrupts_stream()
            .then(|| self.stream_interruption.register());
        self.commands
            .send(QueuedEngineCommand {
                command,
                interruption,
            })
            .map_err(|_| anyhow!("local audio engine is not running"))
    }
}

impl Drop for LocalAudioEngine {
    fn drop(&mut self) {
        let _ = self.send(EngineCommand::Shutdown);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

struct WorkerModel {
    queue: Vec<EngineQueueItem>,
    index: Option<usize>,
    status: PlaybackStatus,
    intent_playing: bool,
    reload_requested: bool,
    start_seconds: f64,
    volume: f32,
    audio_mode: AudioMode,
    play_mode: PlayMode,
    /// Parallel to `queue`: whether each index has been played in the current shuffle cycle.
    /// Resized/rebuilt whenever the queue length changes or indices are remapped.
    shuffle_played: Vec<bool>,
    /// LCG state for shuffle — no external RNG dependency.
    shuffle_rng: u64,
    dsp_control: DspControlState,
    generation: u64,
    error: Option<String>,
    output_device: Option<String>,
    pending_output_device_fallback: Option<String>,
}

impl Default for WorkerModel {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            index: None,
            status: PlaybackStatus::Idle,
            intent_playing: false,
            reload_requested: false,
            start_seconds: 0.0,
            volume: 1.0,
            audio_mode: AudioMode::Music,
            play_mode: PlayMode::Sequential,
            shuffle_played: Vec::new(),
            // Mix process/thread entropy lightly without pulling in the rand crate.
            shuffle_rng: default_shuffle_seed(),
            dsp_control: DspControlState::default(),
            generation: 0,
            error: None,
            output_device: None,
            pending_output_device_fallback: None,
        }
    }
}

fn default_shuffle_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC0FFEE);
    nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(std::process::id() as u64)
        .max(1)
}

/// Numerical Recipes LCG — worker-thread only, never on the audio callback path.
fn lcg_next(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    *state
}

fn lcg_index(state: &mut u64, len: usize) -> usize {
    debug_assert!(len > 0);
    (lcg_next(state) as usize) % len
}

fn reset_shuffle_cycle(model: &mut WorkerModel) {
    model.shuffle_played.clear();
    model.shuffle_played.resize(model.queue.len(), false);
}

fn sync_shuffle_len(model: &mut WorkerModel) {
    let n = model.queue.len();
    if model.shuffle_played.len() != n {
        // Queue mutated mid-cycle (enqueue/remove/load) — invalidate the played set.
        reset_shuffle_cycle(model);
    }
}

fn mark_shuffle_played(model: &mut WorkerModel, index: usize) {
    sync_shuffle_len(model);
    if index < model.shuffle_played.len() {
        model.shuffle_played[index] = true;
    }
}

/// Pick the next shuffle index. Prefers unplayed tracks; resets the cycle when exhausted.
/// When `prefer_not` is set and the queue has more than one track, avoid immediately
/// re-picking it after a full-cycle reset.
fn pick_shuffle_index(model: &mut WorkerModel, prefer_not: Option<usize>) -> Option<usize> {
    let n = model.queue.len();
    if n == 0 {
        return None;
    }
    sync_shuffle_len(model);

    let mut available: Vec<usize> = (0..n).filter(|&i| !model.shuffle_played[i]).collect();
    if available.is_empty() {
        reset_shuffle_cycle(model);
        available = (0..n).collect();
        if let Some(skip) = prefer_not
            && n > 1
            && let Some(pos) = available.iter().position(|&i| i == skip)
        {
            available.swap_remove(pos);
        }
    }
    if available.is_empty() {
        return Some(0);
    }
    let choice = available[lcg_index(&mut model.shuffle_rng, available.len())];
    Some(choice)
}

/// Pure selection helper mirroring the frontend algorithm (tests only).
/// Production Ended never auto-advances — the frontend owns next-track choice.
#[cfg(test)]
fn next_index_on_ended(model: &mut WorkerModel) -> Option<usize> {
    let current = model.index?;
    let n = model.queue.len();
    if n == 0 {
        return None;
    }
    match model.play_mode {
        PlayMode::Sequential => {
            let next = current + 1;
            if next < n { Some(next) } else { None }
        }
        PlayMode::RepeatAll => {
            if n == 1 {
                Some(0)
            } else {
                Some((current + 1) % n)
            }
        }
        PlayMode::RepeatOne => Some(current),
        PlayMode::Shuffle => {
            mark_shuffle_played(model, current);
            pick_shuffle_index(model, Some(current))
        }
    }
}

/// Decide the next queue index for an explicit Next command.
/// Returns `None` when the command is a no-op (e.g. sequential at last track).
fn next_index_on_next(model: &mut WorkerModel) -> Option<usize> {
    let current = model.index?;
    let n = model.queue.len();
    if n == 0 {
        return None;
    }
    match model.play_mode {
        PlayMode::Sequential | PlayMode::RepeatOne => {
            let next = current + 1;
            if next < n { Some(next) } else { None }
        }
        PlayMode::RepeatAll => {
            if n == 1 {
                // Restart the only track.
                Some(0)
            } else {
                Some((current + 1) % n)
            }
        }
        PlayMode::Shuffle => {
            mark_shuffle_played(model, current);
            pick_shuffle_index(model, Some(current))
        }
    }
}

/// Decide the previous queue index for an explicit Previous command.
fn next_index_on_previous(model: &mut WorkerModel) -> Option<usize> {
    let current = model.index?;
    let n = model.queue.len();
    if n == 0 {
        return None;
    }
    match model.play_mode {
        PlayMode::Sequential | PlayMode::RepeatOne => {
            if current > 0 {
                Some(current - 1)
            } else {
                // Restart current from the beginning.
                Some(0)
            }
        }
        PlayMode::RepeatAll => {
            if n == 1 {
                Some(0)
            } else if current == 0 {
                Some(n - 1)
            } else {
                Some(current - 1)
            }
        }
        PlayMode::Shuffle => {
            mark_shuffle_played(model, current);
            pick_shuffle_index(model, Some(current))
        }
    }
}

fn request_track_change(
    model: &mut WorkerModel,
    index: usize,
    session: &mut Option<PlaybackSession>,
) {
    model.index = Some(index);
    model.start_seconds = 0.0;
    model.reload_requested = true;
    model.status = PlaybackStatus::Loading;
    model.error = None;
    model.generation = model.generation.wrapping_add(1);
    *session = None;
}

fn run_worker(
    commands: Receiver<QueuedEngineCommand>,
    shared_snapshot: Arc<Mutex<EngineSnapshot>>,
    diagnostics: EngineDiagnosticQueue,
    streaming_diagnostics: StreamingDiagnosticQueue,
    stream_interruption: StreamInterruption,
    output_device_events: OutputDeviceEventQueue,
    ab_dry_active: Arc<AtomicBool>,
) {
    let mut model = WorkerModel::default();
    let mut session: Option<PlaybackSession> = None;

    loop {
        let mut backpressured = false;
        let mut shutdown = false;
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if handle_queued_command(command, &mut model, &mut session) {
                        shutdown = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
            }
        }
        if shutdown {
            break;
        }

        if model.reload_requested {
            session = None;
            model.reload_requested = false;
            if let Some(item) = model.index.and_then(|index| model.queue.get(index)) {
                model.status = PlaybackStatus::Loading;
                publish_snapshot(&model, None, &shared_snapshot);
                match PlaybackSession::new(
                    &item.source,
                    model.start_seconds,
                    model.volume,
                    model.dsp_control.settings.clone(),
                    model.output_device.as_deref(),
                    PlaybackSessionRuntime {
                        streaming_diagnostics: streaming_diagnostics.clone(),
                        stream_interruption: stream_interruption.clone(),
                        ab_dry_active: Arc::clone(&ab_dry_active),
                    },
                ) {
                    Ok(mut next_session) => {
                        if let Some(index) = model.index
                            && let Some(item) = model.queue.get_mut(index)
                        {
                            item.public.duration_seconds = next_session.duration_seconds();
                            if let Some(title) = next_session.discovered_title() {
                                item.public.title = title.to_owned();
                            }
                        }
                        if !model.intent_playing {
                            next_session.pause();
                            model.status = PlaybackStatus::Paused;
                        }
                        session = Some(next_session);
                        model.error = None;
                        if let Some(unavailable_device) =
                            model.pending_output_device_fallback.take()
                        {
                            output_device_events.push(OutputDeviceFallbackEvent {
                                unavailable_device,
                                fallback_device: session
                                    .as_ref()
                                    .map(|active| active.output_device_name.clone()),
                            });
                        }
                    }
                    Err(error) => {
                        if is_expected_stream_interruption(&error, &stream_interruption) {
                            if let PlaybackSource::Online {
                                cache_plan: Some(plan),
                                ..
                            } = &item.source
                            {
                                plan.invalidate();
                            }
                            // The command carrying the interruption guard is still queued. Keep a
                            // retry armed so invalid/no-op commands can resume the same source;
                            // Pause and destructive commands explicitly clear or replace it.
                            model.reload_requested = true;
                            model.error = None;
                        } else if let Some(unavailable_device) = model.output_device.take()
                            && is_output_device_error(&error)
                        {
                            model.pending_output_device_fallback = Some(unavailable_device);
                            model.reload_requested = true;
                            model.status = PlaybackStatus::Loading;
                            model.error = None;
                        } else {
                            if let PlaybackSource::Online {
                                cache_plan: Some(plan),
                                ..
                            } = &item.source
                            {
                                plan.invalidate();
                            }
                            if let Some(unavailable_device) =
                                model.pending_output_device_fallback.take()
                            {
                                output_device_events.push(OutputDeviceFallbackEvent {
                                    unavailable_device,
                                    fallback_device: None,
                                });
                            }
                            diagnostics.push(EngineDiagnostic {
                                category: "playback_start_failed",
                                source: playback_source_label(&item.source, item.public.online),
                                summary: format!(
                                    "stage=session_new code={} generation={}",
                                    playback_error_code(&error),
                                    model.generation
                                ),
                                generation: Some(model.generation),
                            });
                            model.status = PlaybackStatus::Failed;
                            model.error = Some(error.to_string());
                        }
                    }
                }
            }
        }

        if let Some(active) = session.as_mut() {
            if active.output_device_failed.load(Ordering::Acquire) {
                model.start_seconds = active.position_seconds();
                if let Some(unavailable_device) = model.output_device.take() {
                    model.pending_output_device_fallback = Some(unavailable_device);
                    model.reload_requested = true;
                    model.status = PlaybackStatus::Loading;
                    model.error = None;
                } else {
                    if let Some(unavailable_device) = model.pending_output_device_fallback.take() {
                        output_device_events.push(OutputDeviceFallbackEvent {
                            unavailable_device,
                            fallback_device: None,
                        });
                    }
                    model.status = PlaybackStatus::Failed;
                    model.error = Some("没有可用的音频输出设备".into());
                }
                session = None;
                publish_snapshot(&model, None, &shared_snapshot);
                continue;
            }
            if model.intent_playing {
                active.resume();
            } else {
                active.pause();
            }

            match active.pump() {
                Ok(result @ (PumpResult::Progress | PumpResult::Backpressure)) => {
                    backpressured = matches!(result, PumpResult::Backpressure);
                    model.status = if model.intent_playing && active.has_started() {
                        PlaybackStatus::Playing
                    } else if model.intent_playing {
                        PlaybackStatus::Loading
                    } else {
                        PlaybackStatus::Paused
                    };
                }
                Ok(PumpResult::Ended) => {
                    // Frontend is the sole authority for next-track selection (scheme-1 queues).
                    // The engine only reports natural end as Stopped; it never auto-advances.
                    // Explicit Next/Previous/Jump still change tracks when the UI commands them.
                    model.status = PlaybackStatus::Stopped;
                    model.intent_playing = false;
                    model.start_seconds = active.position_seconds();
                    session = None;
                }
                Err(error) if is_expected_stream_interruption(&error, &stream_interruption) => {
                    model.start_seconds = active.position_seconds();
                    model.reload_requested = true;
                    model.status = if model.intent_playing {
                        PlaybackStatus::Loading
                    } else {
                        PlaybackStatus::Paused
                    };
                    model.error = None;
                    active.invalidate_cache();
                    session = None;
                }
                Err(_error) if active.output_device_failed.load(Ordering::Acquire) => {
                    model.start_seconds = active.position_seconds();
                    active.invalidate_cache();
                    if let Some(unavailable_device) = model.output_device.take() {
                        model.pending_output_device_fallback = Some(unavailable_device);
                        model.reload_requested = true;
                        model.status = PlaybackStatus::Loading;
                        model.error = None;
                    } else {
                        if let Some(unavailable_device) =
                            model.pending_output_device_fallback.take()
                        {
                            output_device_events.push(OutputDeviceFallbackEvent {
                                unavailable_device,
                                fallback_device: None,
                            });
                        }
                        model.status = PlaybackStatus::Failed;
                        model.error = Some("没有可用的音频输出设备".into());
                    }
                    session = None;
                }
                Err(error) => {
                    let source = model
                        .index
                        .and_then(|index| model.queue.get(index))
                        .map_or("unknown", |item| {
                            playback_source_label(&item.source, item.public.online)
                        });
                    diagnostics.push(EngineDiagnostic {
                        category: "playback_runtime_failed",
                        source,
                        summary: format!(
                            "stage=pump code={} generation={}",
                            playback_error_code(&error),
                            model.generation
                        ),
                        generation: Some(model.generation),
                    });
                    active.invalidate_cache();
                    model.status = PlaybackStatus::Failed;
                    model.error = Some(error.to_string());
                    session = None;
                }
            }
        }

        publish_snapshot(&model, session.as_ref(), &shared_snapshot);

        if session.is_some() {
            if backpressured {
                if let Ok(command) = commands.recv_timeout(Duration::from_millis(4))
                    && handle_queued_command(command, &mut model, &mut session)
                {
                    break;
                }
            } else if let Ok(command) = commands.try_recv()
                && handle_queued_command(command, &mut model, &mut session)
            {
                break;
            }
        } else {
            match commands.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => {
                    if handle_queued_command(command, &mut model, &mut session) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn handle_queued_command(
    queued: QueuedEngineCommand,
    model: &mut WorkerModel,
    session: &mut Option<PlaybackSession>,
) -> bool {
    let QueuedEngineCommand {
        command,
        interruption,
    } = queued;
    let shutdown = handle_command(command, model, session);
    drop(interruption);
    shutdown
}

fn handle_command(
    command: EngineCommand,
    model: &mut WorkerModel,
    session: &mut Option<PlaybackSession>,
) -> bool {
    match command {
        EngineCommand::Load { items, start_index } => {
            let start = if items.is_empty() {
                None
            } else {
                Some(start_index.min(items.len() - 1))
            };
            model.queue = items;
            model.index = start;
            model.start_seconds = 0.0;
            model.intent_playing = start.is_some();
            model.reload_requested = start.is_some();
            model.status = if start.is_some() {
                PlaybackStatus::Loading
            } else {
                PlaybackStatus::Idle
            };
            model.error = None;
            model.generation = model.generation.wrapping_add(1);
            reset_shuffle_cycle(model);
            if let Some(idx) = start {
                mark_shuffle_played(model, idx);
            }
            *session = None;
        }
        EngineCommand::Enqueue(items) => {
            if items.is_empty() {
                return false;
            }
            let was_empty = model.queue.is_empty();
            // Preserve shuffle progress for existing indices; new tail is unplayed.
            sync_shuffle_len(model);
            let old_len = model.queue.len();
            model.queue.extend(items);
            model.shuffle_played.resize(model.queue.len(), false);
            // If nothing was playing, start the first enqueued item.
            if was_empty {
                model.index = Some(0);
                model.start_seconds = 0.0;
                model.intent_playing = true;
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation = model.generation.wrapping_add(1);
                mark_shuffle_played(model, 0);
                *session = None;
            } else {
                let _ = old_len;
            }
        }
        EngineCommand::ReplaceCurrent(item) => {
            let Some(index) = model.index else {
                return false;
            };
            if let Some(active) = session.as_ref() {
                model.start_seconds = active.position_seconds();
            }
            model.queue[index] = item;
            model.reload_requested = true;
            model.status = PlaybackStatus::Loading;
            model.error = None;
            model.generation = model.generation.wrapping_add(1);
            *session = None;
        }
        EngineCommand::Jump(index) => {
            if index < model.queue.len() {
                mark_shuffle_played(model, index);
                request_track_change(model, index, session);
                model.intent_playing = true;
            }
        }
        EngineCommand::Remove(index) => {
            if index >= model.queue.len() {
                return false;
            }
            model.queue.remove(index);
            if model.shuffle_played.len() > index {
                model.shuffle_played.remove(index);
            } else {
                reset_shuffle_cycle(model);
            }
            match model.index {
                None => {}
                Some(_) if model.queue.is_empty() => {
                    model.index = None;
                    model.status = PlaybackStatus::Idle;
                    model.intent_playing = false;
                    model.start_seconds = 0.0;
                    model.reload_requested = false;
                    model.generation = model.generation.wrapping_add(1);
                    *session = None;
                }
                Some(current) if current == index => {
                    // Removed the playing track — land on the same slot (next item) or last.
                    let next = index.min(model.queue.len() - 1);
                    request_track_change(model, next, session);
                    model.intent_playing = true;
                    mark_shuffle_played(model, next);
                }
                Some(current) if current > index => {
                    model.index = Some(current - 1);
                }
                Some(_) => {}
            }
        }
        EngineCommand::Reorder { from, to } => {
            if from >= model.queue.len() || to >= model.queue.len() || from == to {
                return false;
            }
            let item = model.queue.remove(from);
            model.queue.insert(to, item);
            if model.shuffle_played.len() == model.queue.len() {
                let played = model.shuffle_played.remove(from);
                model.shuffle_played.insert(to, played);
            } else {
                reset_shuffle_cycle(model);
            }
            model.index = model.index.map(|index| remap_moved_index(index, from, to));
        }
        EngineCommand::ClearQueue => {
            model.queue.clear();
            model.index = None;
            model.start_seconds = 0.0;
            model.intent_playing = false;
            model.reload_requested = false;
            model.status = PlaybackStatus::Idle;
            model.error = None;
            model.generation = model.generation.wrapping_add(1);
            reset_shuffle_cycle(model);
            *session = None;
        }
        EngineCommand::Play => {
            if model.index.is_some() {
                if model.status == PlaybackStatus::Stopped {
                    model.start_seconds = 0.0;
                    model.status = PlaybackStatus::Loading;
                    model.generation = model.generation.wrapping_add(1);
                }
                model.intent_playing = true;
                if session.is_none() {
                    model.reload_requested = true;
                }
            }
        }
        EngineCommand::Pause => {
            model.intent_playing = false;
            // If a blocked online read was cancelled, keep the saved position but do not rebuild
            // until the user explicitly resumes. Otherwise Pause would immediately block again.
            model.reload_requested = false;
            if let Some(active) = session.as_mut() {
                active.pause();
            }
            if model.index.is_some() {
                model.status = PlaybackStatus::Paused;
            }
        }
        EngineCommand::Seek(seconds) => {
            if let Some(index) = model.index {
                let duration = model.queue[index].public.duration_seconds;
                model.start_seconds = duration.map_or(seconds, |value| seconds.min(value));
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation = model.generation.wrapping_add(1);
                *session = None;
            }
        }
        EngineCommand::SetVolume(volume) => {
            apply_volume_change(
                model,
                session.as_ref().map(|active| active.volume_bits.as_ref()),
                volume,
            );
        }
        EngineCommand::SetPlayMode(mode) => {
            let previous = model.play_mode;
            model.play_mode = mode;
            if mode == PlayMode::Shuffle && previous != PlayMode::Shuffle {
                // Fresh shuffle cycle when entering shuffle; mark current as already heard.
                reset_shuffle_cycle(model);
                if let Some(idx) = model.index {
                    mark_shuffle_played(model, idx);
                }
            }
        }
        EngineCommand::SetDspControl { control, reply } => {
            let result =
                apply_dsp_change(model, session, control).map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        EngineCommand::SetOutputDevice(name) => {
            model.output_device = name;
            model.pending_output_device_fallback = None;
            if model.index.is_some() {
                if let Some(active) = session.as_ref() {
                    model.start_seconds = active.position_seconds();
                }
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation = model.generation.wrapping_add(1);
                *session = None;
            }
        }
        EngineCommand::Next => {
            if let Some(next) = next_index_on_next(model) {
                // RepeatAll with single track: still restart.
                let same = model.index == Some(next);
                request_track_change(model, next, session);
                if same {
                    // restart current
                }
                model.intent_playing = true;
            }
        }
        EngineCommand::Previous => {
            if let Some(next) = next_index_on_previous(model) {
                request_track_change(model, next, session);
                model.intent_playing = true;
            }
        }
        EngineCommand::Shutdown => return true,
    }
    false
}

fn remap_moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < to && (from + 1..=to).contains(&index) {
        index - 1
    } else if to < from && (to..from).contains(&index) {
        index + 1
    } else {
        index
    }
}

fn apply_volume_change(model: &mut WorkerModel, volume_bits: Option<&AtomicU32>, volume: f32) {
    model.volume = volume;
    if let Some(volume_bits) = volume_bits {
        volume_bits.store(volume.to_bits(), Ordering::Relaxed);
    }
}

fn apply_dsp_change(
    model: &mut WorkerModel,
    session: &mut Option<PlaybackSession>,
    control: DspControlState,
) -> Result<()> {
    if let Some(active) = session.as_ref()
        && dsp_change_requires_output_rebuild(
            active.dsp_chain.settings(),
            &control.settings,
            active.source_channels,
            active.output_channels,
        )
    {
        model.start_seconds = active.position_seconds();
        model.audio_mode = control.audio_mode();
        model.dsp_control = control;
        model.reload_requested = true;
        model.status = PlaybackStatus::Loading;
        model.generation = model.generation.wrapping_add(1);
        model.error = None;
        *session = None;
        return Ok(());
    }
    if let Some(active) = session.as_mut()
        && let Err(error) = active.set_dsp_settings(control.settings.clone())
    {
        let error = format!("failed to update DSP settings: {error}");
        model.error = Some(error.clone());
        return Err(anyhow!(error));
    }
    model.audio_mode = control.audio_mode();
    model.dsp_control = control;
    model.error = None;
    Ok(())
}

fn dsp_change_requires_output_rebuild(
    current: &DspSettings,
    next: &DspSettings,
    source_channels: usize,
    output_channels: usize,
) -> bool {
    let current_requires_stereo = dsp_requires_stereo(current);
    let next_requires_stereo = dsp_requires_stereo(next);
    (next_requires_stereo && output_channels != 2)
        || (current_requires_stereo && !next_requires_stereo && source_channels != 2)
}

fn dsp_requires_stereo(settings: &DspSettings) -> bool {
    settings.crossfeed.enabled || settings.hrtf.enabled
}

fn local_queue_item(path: PathBuf) -> EngineQueueItem {
    let title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    EngineQueueItem {
        public: QueueItem {
            location: path.display().to_string(),
            title,
            duration_seconds: None,
            online: false,
        },
        source: PlaybackSource::Local(path),
    }
}

fn publish_snapshot(
    model: &WorkerModel,
    session: Option<&PlaybackSession>,
    destination: &Mutex<EngineSnapshot>,
) {
    let duration_seconds = model
        .index
        .and_then(|index| model.queue.get(index))
        .and_then(|item| item.public.duration_seconds)
        .or_else(|| session.and_then(PlaybackSession::duration_seconds));
    let position_seconds = session
        .map(PlaybackSession::position_seconds)
        .unwrap_or(model.start_seconds);
    let underruns = session.map_or(0, PlaybackSession::underruns);
    let output_sample_rate = session.map(|session| session.output_sample_rate);
    let source_sample_rate = session.map(|session| session.source_sample_rate);
    let source_bit_depth = session.and_then(|session| session.source_bit_depth);
    let source_channels = session.map(|session| session.source_channels as u16);
    *destination.lock().unwrap() = EngineSnapshot {
        status: model.status,
        queue: model.queue.iter().map(|item| item.public.clone()).collect(),
        queue_index: model.index,
        position_seconds,
        duration_seconds,
        volume: model.volume,
        audio_mode: model.audio_mode,
        play_mode: model.play_mode,
        dsp_settings: model.dsp_control.settings.clone(),
        active_preset_id: model.dsp_control.active_preset_id,
        intensity: model.dsp_control.intensity,
        spatial_amount: model.dsp_control.spatial_amount,
        generation: model.generation,
        underrun_callbacks: underruns,
        output_sample_rate,
        source_sample_rate,
        source_bit_depth,
        source_channels,
        error: model.error.clone(),
        output_device: model.output_device.clone(),
    };
}

enum PumpResult {
    Progress,
    Backpressure,
    Ended,
}

#[derive(Debug, Clone, Copy, Default)]
struct AbSample {
    processed: f32,
    dry: f32,
}

struct PlaybackSessionRuntime {
    streaming_diagnostics: StreamingDiagnosticQueue,
    stream_interruption: StreamInterruption,
    ab_dry_active: Arc<AtomicBool>,
}

#[cfg(test)]
impl AbSample {
    fn same(sample: f32) -> Self {
        Self {
            processed: sample,
            dry: sample,
        }
    }
}

struct PlaybackSession {
    media: OpenedMedia,
    rate_adapter: RateAdapter,
    dsp_chain: DspChain,
    sample_buffer: Option<SampleBuffer<f32>>,
    producer: HeapProd<AbSample>,
    stream: Stream,
    played_samples: Arc<AtomicU64>,
    underrun_callbacks: Arc<AtomicU64>,
    callback_enabled: Arc<AtomicBool>,
    volume_bits: Arc<AtomicU32>,
    output_device_failed: Arc<AtomicBool>,
    output_device_name: String,
    source_channels: usize,
    output_channels: usize,
    source_sample_rate: u32,
    source_bit_depth: Option<u32>,
    output_sample_rate: u32,
    start_seconds: f64,
    duration_seconds: Option<f64>,
    discovered_title: Option<String>,
    cache_plan: Option<CacheWritePlan>,
    cache_committed: bool,
    prebuffer_samples: usize,
    pending: Vec<f32>,
    pending_ab_dry: Vec<f32>,
    pending_offset: usize,
    eof: bool,
    flushed: bool,
    intent_playing: bool,
    stream_playing: bool,
    seek_discard_frames: usize,
}

impl PlaybackSession {
    fn new(
        source: &PlaybackSource,
        start_seconds: f64,
        volume: f32,
        dsp_settings: DspSettings,
        output_device_name: Option<&str>,
        runtime: PlaybackSessionRuntime,
    ) -> Result<Self> {
        let cache_plan = match source {
            PlaybackSource::Online { cache_plan, .. } => cache_plan.clone(),
            PlaybackSource::Local(_) => None,
        };
        let (mut media, seek_discard_frames) = match source {
            PlaybackSource::Local(path) => {
                let mut media = open_media(path)?;
                seek_media(&mut media, start_seconds)?;
                (media, 0)
            }
            PlaybackSource::Online {
                request,
                cache_plan,
            } => {
                let extension = media_extension(&request.media_type);
                let description = request.redacted_for_log();
                let http = HttpMediaSource::new_with_cache_diagnostics_and_interruption(
                    request.clone(),
                    cache_plan.clone(),
                    runtime.streaming_diagnostics,
                    runtime.stream_interruption,
                )?;
                let mut media = open_media_source(Box::new(http), extension, &description)?;
                let discard = seek_media_coarse(&mut media, start_seconds)?;
                (media, discard)
            }
        };
        let sample_rate = media
            .codec_params
            .sample_rate
            .context("audio track does not declare a sample rate")?;
        let channels = media
            .codec_params
            .channels
            .context("audio track does not declare a channel layout")?
            .count();
        let source_bit_depth = media.codec_params.bits_per_sample;
        if let Some(plan) = &cache_plan {
            plan.update_source_spec(sample_rate, source_bit_depth, channels as u16);
            if let Some(media_type) = decoded_media_type(media.codec_params.codec) {
                plan.update_media_type(media_type);
            }
        }
        let duration_seconds = media
            .codec_params
            .n_frames
            .map(|frames| frames as f64 / sample_rate as f64);
        let discovered_title = matches!(source, PlaybackSource::Local(_))
            .then(|| {
                media.format.metadata().current().and_then(|revision| {
                    revision
                        .tags()
                        .iter()
                        .find(|tag| tag.std_key == Some(StandardTagKey::TrackTitle))
                        .map(|tag| tag.value.to_string())
                        .filter(|title| !title.trim().is_empty())
                })
            })
            .flatten();

        let host = cpal::default_host();
        let device = match output_device_name {
            Some(name) => host
                .output_devices()
                .context("failed to enumerate output devices")?
                .find(|device| device.name().ok().as_deref() == Some(name))
                .with_context(|| format!("output device '{name}' is unavailable"))?,
            None => host
                .default_output_device()
                .context("no default audio output device is available")?,
        };
        let preferred_channels = if dsp_requires_stereo(&dsp_settings) {
            2
        } else {
            channels as u16
        };
        let output_device_name = device.name().unwrap_or_else(|_| "系统默认设备".to_owned());
        let supported = choose_output_config(&device, sample_rate, preferred_channels)
            .context("failed to configure output device")?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let output_sample_rate = config.sample_rate.0;
        let output_channels = config.channels as usize;
        let ring_capacity = ((output_sample_rate as f64 * output_channels as f64 * RING_SECONDS)
            as usize)
            .max(4096);
        let ring = HeapRb::<AbSample>::new(ring_capacity);
        let (producer, consumer) = ring.split();
        let played_samples = Arc::new(AtomicU64::new(0));
        let underrun_callbacks = Arc::new(AtomicU64::new(0));
        let callback_enabled = Arc::new(AtomicBool::new(false));
        let volume_bits = Arc::new(AtomicU32::new(volume.to_bits()));
        let output_device_failed = Arc::new(AtomicBool::new(false));
        let stream = build_engine_output_stream(
            &device,
            &config,
            sample_format,
            consumer,
            OutputCallbackCounters {
                played_samples: Arc::clone(&played_samples),
                underruns: Arc::clone(&underrun_callbacks),
                enabled: Arc::clone(&callback_enabled),
                volume_bits: Arc::clone(&volume_bits),
                ab_dry_active: runtime.ab_dry_active,
                output_sample_rate,
                output_channels,
            },
            Arc::clone(&output_device_failed),
        )
        .context("failed to build output device stream")?;
        let mut rate_adapter = RateAdapter::new(sample_rate, output_sample_rate, channels)?;
        let mut dsp_chain = DspChain::new(output_sample_rate, output_channels, dsp_settings)?;
        let prefetched = std::mem::take(&mut media.prefetched_samples);
        let mut pending = if prefetched.is_empty() {
            Vec::new()
        } else {
            rate_adapter.process(&prefetched)?
        };
        pending = remap_channels(&pending, channels, output_channels)?;
        let mut pending_ab_dry = vec![0.0; pending.len()];
        dsp_chain.process_interleaved_with_ab_dry(&mut pending, &mut pending_ab_dry)?;

        Ok(Self {
            media,
            rate_adapter,
            dsp_chain,
            sample_buffer: None,
            producer,
            stream,
            played_samples,
            underrun_callbacks,
            callback_enabled,
            volume_bits,
            output_device_failed,
            output_device_name,
            source_channels: channels,
            output_channels,
            source_sample_rate: sample_rate,
            source_bit_depth,
            output_sample_rate,
            start_seconds,
            duration_seconds,
            discovered_title,
            cache_plan,
            cache_committed: false,
            prebuffer_samples: (output_sample_rate as f64
                * output_channels as f64
                * PREBUFFER_SECONDS) as usize,
            pending,
            pending_ab_dry,
            pending_offset: 0,
            eof: false,
            flushed: false,
            intent_playing: true,
            stream_playing: false,
            seek_discard_frames,
        })
    }

    fn pump(&mut self) -> Result<PumpResult> {
        if self.pending_offset < self.pending.len() {
            while self.pending_offset < self.pending.len() {
                let sample = AbSample {
                    processed: self.pending[self.pending_offset],
                    dry: self.pending_ab_dry[self.pending_offset],
                };
                match self.producer.try_push(sample) {
                    Ok(()) => self.pending_offset += 1,
                    Err(_) => {
                        self.maybe_start()?;
                        return Ok(PumpResult::Backpressure);
                    }
                }
            }
            self.pending.clear();
            self.pending_ab_dry.clear();
            self.pending_offset = 0;
            self.maybe_start()?;
            return Ok(PumpResult::Progress);
        }

        if self.eof {
            if !self.flushed {
                self.pending = self.rate_adapter.finish()?;
                self.pending =
                    remap_channels(&self.pending, self.source_channels, self.output_channels)?;
                self.pending_ab_dry.resize(self.pending.len(), 0.0);
                self.dsp_chain
                    .process_interleaved_with_ab_dry(&mut self.pending, &mut self.pending_ab_dry)?;
                self.flushed = true;
                if !self.pending.is_empty() {
                    return Ok(PumpResult::Progress);
                }
            }
            self.maybe_start()?;
            if self.producer.is_empty() {
                return Ok(PumpResult::Ended);
            }
            return Ok(PumpResult::Backpressure);
        }

        let packet = match self.media.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                self.commit_cache_after_decode();
                self.eof = true;
                return Ok(PumpResult::Progress);
            }
            Err(error) => return Err(error).context("failed to read media packet"),
        };
        if packet.track_id() != self.media.track_id {
            return Ok(PumpResult::Progress);
        }
        let decoded = match self.media.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => return Ok(PumpResult::Progress),
            Err(error) => return Err(error).context("failed to decode media packet"),
        };
        if self.sample_buffer.is_none() {
            self.sample_buffer = Some(SampleBuffer::<f32>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let buffer = self
            .sample_buffer
            .as_mut()
            .expect("sample buffer initialized");
        buffer.copy_interleaved_ref(decoded);
        let mut samples = buffer.samples();
        if self.seek_discard_frames > 0 {
            let frames = samples.len() / self.source_channels;
            let discard = frames.min(self.seek_discard_frames);
            samples = &samples[discard * self.source_channels..];
            self.seek_discard_frames -= discard;
            if samples.is_empty() {
                return Ok(PumpResult::Progress);
            }
        }
        self.pending = self.rate_adapter.process(samples)?;
        self.pending = remap_channels(&self.pending, self.source_channels, self.output_channels)?;
        self.pending_ab_dry.resize(self.pending.len(), 0.0);
        self.dsp_chain
            .process_interleaved_with_ab_dry(&mut self.pending, &mut self.pending_ab_dry)?;
        Ok(PumpResult::Progress)
    }

    fn maybe_start(&mut self) -> Result<()> {
        let enough = self.producer.occupied_len() >= self.prebuffer_samples;
        if self.intent_playing && !self.stream_playing && (enough || self.eof) {
            self.callback_enabled.store(true, Ordering::Release);
            if let Err(error) = self.stream.play() {
                self.output_device_failed.store(true, Ordering::Release);
                return Err(error).context("failed to start output device stream");
            }
            self.stream_playing = true;
        }
        Ok(())
    }

    fn pause(&mut self) {
        self.intent_playing = false;
        if self.stream_playing {
            self.callback_enabled.store(false, Ordering::Release);
            let _ = self.stream.pause();
            self.stream_playing = false;
        }
    }

    fn resume(&mut self) {
        self.intent_playing = true;
        let _ = self.maybe_start();
    }

    fn has_started(&self) -> bool {
        self.stream_playing
    }

    fn set_dsp_settings(&mut self, settings: DspSettings) -> Result<()> {
        self.dsp_chain.set_settings(settings)?;
        Ok(())
    }

    fn position_seconds(&self) -> f64 {
        let played_frames =
            self.played_samples.load(Ordering::Relaxed) as f64 / self.output_channels as f64;
        self.start_seconds + played_frames / self.output_sample_rate as f64
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
    }

    fn discovered_title(&self) -> Option<&str> {
        self.discovered_title.as_deref()
    }

    fn commit_cache_after_decode(&mut self) {
        if self.cache_committed {
            return;
        }
        if let Some(plan) = &self.cache_plan {
            plan.commit_in_background();
        }
        self.cache_committed = true;
    }

    fn invalidate_cache(&mut self) {
        if !self.cache_committed {
            if let Some(plan) = &self.cache_plan {
                plan.invalidate();
            }
            self.cache_committed = true;
        }
    }

    fn underruns(&self) -> u64 {
        self.underrun_callbacks.load(Ordering::Relaxed)
    }
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        self.invalidate_cache();
    }
}

fn media_extension(media_type: &MediaType) -> Option<&'static str> {
    match media_type {
        MediaType::Mp3 => Some("mp3"),
        MediaType::Flac => Some("flac"),
        MediaType::Aac => Some("aac"),
        MediaType::Ogg => Some("ogg"),
        MediaType::Wav => Some("wav"),
        MediaType::Hls | MediaType::Unknown => None,
    }
}

fn decoded_media_type(codec: CodecType) -> Option<MediaType> {
    match codec {
        CODEC_TYPE_MP3 => Some(MediaType::Mp3),
        CODEC_TYPE_FLAC => Some(MediaType::Flac),
        CODEC_TYPE_AAC => Some(MediaType::Aac),
        CODEC_TYPE_VORBIS => Some(MediaType::Ogg),
        _ => None,
    }
}

#[derive(Clone)]
struct OutputCallbackCounters {
    played_samples: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    enabled: Arc<AtomicBool>,
    volume_bits: Arc<AtomicU32>,
    ab_dry_active: Arc<AtomicBool>,
    output_sample_rate: u32,
    output_channels: usize,
}

struct AbDryRamp {
    position_frames: u64,
    ramp_frames: u64,
    initialized: bool,
}

impl AbDryRamp {
    fn new(sample_rate: u32) -> Self {
        // Integer rounding keeps the transition at exactly one hundredth of a second without
        // accumulating an extra frame at rates such as 8 or 96 kHz. Widen before adding so even a
        // defensive u32::MAX input cannot overflow.
        let ramp_frames = ((u64::from(sample_rate) + 50) / 100).max(1);
        Self {
            position_frames: 0,
            ramp_frames,
            initialized: false,
        }
    }

    #[inline]
    fn advance(&mut self, dry: bool) -> f32 {
        // A newly built stream may sit in prebuffering while the momentary A/B state changes. Its
        // first audible frame must start at the current state rather than ramping from the stale
        // state observed while the stream was constructed.
        if !self.initialized {
            self.position_frames = if dry { self.ramp_frames } else { 0 };
            self.initialized = true;
        } else if dry {
            self.position_frames = (self.position_frames + 1).min(self.ramp_frames);
        } else {
            self.position_frames = self.position_frames.saturating_sub(1);
        }
        self.position_frames as f32 / self.ramp_frames as f32
    }
}

fn build_engine_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<AbSample>,
    counters: OutputCallbackCounters,
    output_device_failed: Arc<AtomicBool>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::I8 => build_typed_engine_stream::<i8>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::F32 => build_typed_engine_stream::<f32>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::I16 => build_typed_engine_stream::<i16>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::U16 => build_typed_engine_stream::<u16>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::I32 => build_typed_engine_stream::<i32>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::I64 => build_typed_engine_stream::<i64>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::U8 => build_typed_engine_stream::<u8>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::U32 => build_typed_engine_stream::<u32>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::U64 => build_typed_engine_stream::<u64>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        SampleFormat::F64 => build_typed_engine_stream::<f64>(
            device,
            config,
            consumer,
            counters,
            output_device_failed,
        ),
        other => bail!("unsupported output sample format: {other}"),
    }
}

fn build_typed_engine_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: HeapCons<AbSample>,
    counters: OutputCallbackCounters,
    output_device_failed: Arc<AtomicBool>,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    let mut callback_thread_priority = AudioThreadPriority::default();
    let mut ab_dry_ramp = AbDryRamp::new(counters.output_sample_rate);
    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], _| {
            callback_thread_priority.ensure_registered();
            render_output_callback(output, &mut consumer, &counters, &mut ab_dry_ramp);
        },
        move |error| {
            output_device_failed.store(true, Ordering::Release);
            eprintln!("audio output stream error: {error}");
        },
        None,
    )?;
    Ok(stream)
}

#[inline]
fn render_output_callback<T>(
    output: &mut [T],
    consumer: &mut HeapCons<AbSample>,
    counters: &OutputCallbackCounters,
    ab_dry_ramp: &mut AbDryRamp,
) where
    T: Sample + SizedSample + FromSample<f32>,
{
    let enabled = counters.enabled.load(Ordering::Acquire);
    let volume = f32::from_bits(counters.volume_bits.load(Ordering::Relaxed));
    let mut starved = false;
    let mut consumed = 0u64;
    let output_channels = counters.output_channels.max(1);
    for frame in output.chunks_mut(output_channels) {
        let dry = counters.ab_dry_active.load(Ordering::Relaxed);
        let dry_mix = ab_dry_ramp.advance(dry);
        // Never consume a partial interleaved frame. A producer can publish one channel just
        // before backpressure; popping it alone would shift every channel after the underrun.
        if frame.len() != output_channels || consumer.occupied_len() < output_channels {
            starved |= enabled;
            for target in frame {
                *target = T::from_sample(0.0);
            }
            continue;
        }
        for target in frame {
            let value = consumer
                .try_pop()
                .expect("complete output frame was checked before popping");
            let sample = (value.processed + (value.dry - value.processed) * dry_mix) * volume;
            *target = T::from_sample(sample);
        }
        consumed += output_channels as u64;
    }
    counters
        .played_samples
        .fetch_add(consumed, Ordering::Relaxed);
    if starved {
        counters.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::fs;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use crossbeam_channel::{Receiver as TestReceiver, Sender as TestSender};
    use gx_cache::{CacheKey, CacheStore};
    use gx_contracts::NetworkRoute;

    use super::*;

    thread_local! {
        static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            TRACK_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            TRACK_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    struct BlackholeServer {
        url: String,
        accepted: TestReceiver<usize>,
        release: TestSender<()>,
        handle: Option<JoinHandle<()>>,
    }

    impl BlackholeServer {
        fn start(expected_connections: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let (accepted_sender, accepted) = bounded(expected_connections.max(1));
            let (release, release_receiver) = bounded(1);
            let handle = thread::spawn(move || {
                let mut streams: Vec<TcpStream> = Vec::with_capacity(expected_connections);
                while streams.len() < expected_connections {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            streams.push(stream);
                            let _ = accepted_sender.send(streams.len());
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
                let _ = release_receiver.recv_timeout(Duration::from_secs(3));
                drop(streams);
            });
            Self {
                url: format!("http://{address}/blackhole.mp3"),
                accepted,
                release,
                handle: Some(handle),
            }
        }

        fn wait_for_connection(&self, count: usize) -> Duration {
            let started = Instant::now();
            loop {
                let accepted = self
                    .accepted
                    .recv_timeout(Duration::from_secs(1))
                    .expect("blackhole server did not receive a connection");
                if accepted == count {
                    return started.elapsed();
                }
            }
        }

        fn unblock(&mut self) {
            let _ = self.release.try_send(());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    impl Drop for BlackholeServer {
        fn drop(&mut self) {
            let _ = self.release.try_send(());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn blackhole_request(url: &str) -> ResolvedMediaRequest {
        ResolvedMediaRequest {
            url: url.parse().unwrap(),
            headers: Vec::new(),
            media_type: MediaType::Mp3,
            quality: Some("test".into()),
            expires_at_ms: None,
            network_route: Some(NetworkRoute::Direct),
        }
    }

    fn online_item(request: ResolvedMediaRequest, title: &str) -> EngineQueueItem {
        EngineQueueItem {
            public: QueueItem {
                location: request.redacted_for_log(),
                title: title.into(),
                duration_seconds: None,
                online: true,
            },
            source: PlaybackSource::Online {
                request,
                cache_plan: None,
            },
        }
    }

    fn wait_for_engine(
        engine: &LocalAudioEngine,
        predicate: impl Fn(&EngineSnapshot) -> bool,
    ) -> Duration {
        let started = Instant::now();
        loop {
            if predicate(&engine.snapshot()) {
                return started.elapsed();
            }
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "engine state did not converge: {:?}",
                engine.snapshot()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn temporary_cache_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "gx-audio-interruption-test-{}-{nonce}",
            std::process::id()
        ))
    }

    fn dummy_item(name: &str) -> EngineQueueItem {
        EngineQueueItem {
            public: QueueItem {
                location: format!("{name}.wav"),
                title: name.to_owned(),
                duration_seconds: Some(120.0),
                online: false,
            },
            source: PlaybackSource::Local(PathBuf::from(format!("{name}.wav"))),
        }
    }

    fn model_with_queue(n: usize, index: usize, mode: PlayMode) -> WorkerModel {
        let mut model = WorkerModel {
            queue: (0..n).map(|i| dummy_item(&format!("t{i}"))).collect(),
            index: Some(index),
            status: PlaybackStatus::Playing,
            intent_playing: true,
            play_mode: mode,
            generation: 1,
            ..WorkerModel::default()
        };
        reset_shuffle_cycle(&mut model);
        if mode == PlayMode::Shuffle {
            mark_shuffle_played(&mut model, index);
        }
        model
    }

    #[test]
    fn volume_hot_update_changes_atomic_without_reloading() {
        let mut model = WorkerModel {
            status: PlaybackStatus::Playing,
            generation: 7,
            ..WorkerModel::default()
        };
        let volume_bits = AtomicU32::new(1.0f32.to_bits());

        apply_volume_change(&mut model, Some(&volume_bits), 0.35);

        assert_eq!(model.volume, 0.35);
        assert_eq!(f32::from_bits(volume_bits.load(Ordering::Relaxed)), 0.35);
        assert!(!model.reload_requested);
        assert_eq!(model.status, PlaybackStatus::Playing);
        assert_eq!(model.generation, 7);
    }

    #[test]
    fn confirmed_dsp_control_updates_worker_state_before_returning() {
        let engine = LocalAudioEngine::new().unwrap();
        let spatial = DspControlState::from_audio_mode(AudioMode::CinemaGame);

        engine.set_dsp_control_confirmed(spatial.clone()).unwrap();
        wait_for_engine(&engine, |snapshot| {
            snapshot.active_preset_id == DspPresetId::Spatial
        });
        assert_eq!(engine.snapshot().dsp_settings, spatial.settings);

        engine
            .set_dsp_control_confirmed(DspControlState::default())
            .unwrap();
        wait_for_engine(&engine, |snapshot| {
            snapshot.active_preset_id == DspPresetId::Bypass
        });
    }

    #[test]
    fn confirmed_dsp_control_validates_the_active_output_sample_rate() {
        let control = DspControlState {
            settings: DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![gx_dsp::EqBand::peak(16_000.0, 3.0, 0.7)],
                ..DspSettings::default()
            },
            active_preset_id: DspPresetId::Vocal,
            intensity: 0.5,
            spatial_amount: 0.5,
        };
        control.validate_product().unwrap();

        let error = validate_dsp_control_for_output(&control, Some(32_000)).unwrap_err();

        assert!(error.to_string().contains("active 32000 Hz output rate"));
    }

    #[test]
    fn spatial_dsp_change_rebuilds_only_incompatible_output_topologies() {
        let spatial = DspControlState::from_audio_mode(AudioMode::CinemaGame);
        let bypass = DspControlState::default();
        assert!(dsp_change_requires_output_rebuild(
            &bypass.settings,
            &spatial.settings,
            1,
            1,
        ));
        assert!(!dsp_change_requires_output_rebuild(
            &bypass.settings,
            &spatial.settings,
            1,
            2,
        ));
        assert!(dsp_change_requires_output_rebuild(
            &bypass.settings,
            &spatial.settings,
            6,
            6,
        ));
        assert!(dsp_change_requires_output_rebuild(
            &spatial.settings,
            &bypass.settings,
            6,
            2,
        ));
        assert!(!dsp_change_requires_output_rebuild(
            &spatial.settings,
            &bypass.settings,
            2,
            2,
        ));
    }

    #[test]
    fn local_queue_item_is_built_without_media_probe() {
        let item = local_queue_item(PathBuf::from("definitely-missing/queued-song.flac"));
        assert_eq!(item.public.title, "queued-song");
        assert_eq!(item.public.duration_seconds, None);
        assert!(!item.public.online);
    }

    #[test]
    fn connection_aborted_requires_a_pending_command_to_be_expected() {
        let error = anyhow!(io::Error::new(
            ErrorKind::ConnectionAborted,
            "transport aborted without a player command",
        ));
        let interruption = StreamInterruption::default();
        assert!(!is_expected_stream_interruption(&error, &interruption));

        let guard = interruption.register();
        assert!(is_expected_stream_interruption(&error, &interruption));
        drop(guard);
        assert!(!is_expected_stream_interruption(&error, &interruption));
    }

    #[test]
    fn interrupted_pause_defers_range_reload_until_play_and_keeps_position() {
        let mut model = model_with_queue(1, 0, PlayMode::Sequential);
        model.start_seconds = 42.25;
        model.reload_requested = true;
        model.status = PlaybackStatus::Loading;
        let mut session = None;

        assert!(!handle_command(
            EngineCommand::Pause,
            &mut model,
            &mut session,
        ));
        assert_eq!(model.start_seconds, 42.25);
        assert_eq!(model.status, PlaybackStatus::Paused);
        assert!(!model.reload_requested);
        assert!(!model.intent_playing);

        assert!(!handle_command(
            EngineCommand::Play,
            &mut model,
            &mut session,
        ));
        assert_eq!(model.start_seconds, 42.25);
        assert!(model.reload_requested);
        assert!(model.intent_playing);
    }

    #[test]
    fn pause_interrupts_blackhole_ready_wait_without_publishing_cache_or_diagnostics() {
        let mut server = BlackholeServer::start(1);
        let cache_root = temporary_cache_root();
        let store = CacheStore::open(&cache_root, None).unwrap();
        let key = CacheKey {
            provider_id: "test".into(),
            provider_track_id: "pause-blackhole".into(),
            quality: "320k".into(),
        };
        let plan = store.prepare(key.clone(), MediaType::Mp3);
        let engine = LocalAudioEngine::new().unwrap();
        engine
            .load_resolved_cached(
                blackhole_request(&server.url),
                "Pause blackhole".into(),
                Some(plan),
            )
            .unwrap();
        server.wait_for_connection(1);

        let started = Instant::now();
        engine.pause().unwrap();
        wait_for_engine(&engine, |snapshot| {
            snapshot.status == PlaybackStatus::Paused
        });
        let elapsed = started.elapsed();

        assert!(elapsed < Duration::from_millis(200), "elapsed={elapsed:?}");
        assert!(store.lookup(&key).is_none());
        let cache_directory = store.status().directory;

        server.unblock();
        thread::sleep(Duration::from_millis(30));
        assert!(engine.drain_diagnostics().is_empty());
        drop(engine);
        drop(server);
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let has_incomplete = fs::read_dir(&cache_directory)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    matches!(
                        entry.path().extension().and_then(|value| value.to_str()),
                        Some("part" | "ready")
                    )
                });
            if !has_incomplete || Instant::now() >= cleanup_deadline {
                assert!(
                    !has_incomplete,
                    "cancelled stream left an incomplete cache file"
                );
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = fs::remove_dir_all(cache_root);
    }

    #[test]
    fn confirmed_dsp_control_interrupts_a_blocked_stream_and_replies_within_budget() {
        let mut server = BlackholeServer::start(2);
        let engine = LocalAudioEngine::new().unwrap();
        engine
            .load_resolved(blackhole_request(&server.url), "DSP blackhole".into())
            .unwrap();
        server.wait_for_connection(1);

        let started = Instant::now();
        engine
            .set_dsp_control_confirmed(DspControlState::from_audio_mode(AudioMode::CinemaGame))
            .unwrap();
        let elapsed = started.elapsed();

        assert!(elapsed < Duration::from_millis(200), "elapsed={elapsed:?}");
        wait_for_engine(&engine, |snapshot| {
            snapshot.active_preset_id == DspPresetId::Spatial
        });
        server.wait_for_connection(2);

        engine.clear_queue().unwrap();
        wait_for_engine(&engine, |snapshot| snapshot.queue.is_empty());
        server.unblock();
        thread::sleep(Duration::from_millis(30));
        assert!(engine.drain_diagnostics().is_empty());
    }

    #[test]
    fn next_and_clear_queue_interrupt_blackhole_ready_waits_within_budget() {
        let mut server = BlackholeServer::start(2);
        let request = blackhole_request(&server.url);
        let engine = LocalAudioEngine::new().unwrap();
        engine
            .send(EngineCommand::Load {
                items: vec![
                    online_item(request.clone(), "First"),
                    online_item(request, "Second"),
                ],
                start_index: 0,
            })
            .unwrap();
        server.wait_for_connection(1);

        let next_started = Instant::now();
        engine.next().unwrap();
        wait_for_engine(&engine, |snapshot| snapshot.queue_index == Some(1));
        assert!(
            next_started.elapsed() < Duration::from_millis(200),
            "next elapsed={:?}",
            next_started.elapsed()
        );
        server.wait_for_connection(2);

        let clear_started = Instant::now();
        engine.clear_queue().unwrap();
        wait_for_engine(&engine, |snapshot| {
            snapshot.status == PlaybackStatus::Idle && snapshot.queue.is_empty()
        });
        assert!(
            clear_started.elapsed() < Duration::from_millis(200),
            "clear elapsed={:?}",
            clear_started.elapsed()
        );
        server.unblock();
        thread::sleep(Duration::from_millis(30));
        assert!(engine.drain_diagnostics().is_empty());
        drop(engine);
        drop(server);
    }

    #[test]
    fn invalid_jump_and_end_of_queue_next_resume_the_interrupted_source() {
        let mut server = BlackholeServer::start(3);
        let engine = LocalAudioEngine::new().unwrap();
        engine
            .load_resolved(blackhole_request(&server.url), "Only item".into())
            .unwrap();
        server.wait_for_connection(1);

        let jump_started = Instant::now();
        engine.jump(99).unwrap();
        server.wait_for_connection(2);
        assert!(
            jump_started.elapsed() < Duration::from_millis(200),
            "invalid jump recovery elapsed={:?}",
            jump_started.elapsed()
        );

        let next_started = Instant::now();
        engine.next().unwrap();
        server.wait_for_connection(3);
        assert!(
            next_started.elapsed() < Duration::from_millis(200),
            "end-of-queue next recovery elapsed={:?}",
            next_started.elapsed()
        );
        assert_eq!(engine.snapshot().queue_index, Some(0));
        engine.clear_queue().unwrap();
        wait_for_engine(&engine, |snapshot| snapshot.queue.is_empty());
        server.unblock();
        thread::sleep(Duration::from_millis(30));
        assert!(engine.drain_diagnostics().is_empty());
        drop(engine);
        drop(server);
    }

    #[test]
    fn reorder_remaps_current_and_shuffle_without_reloading() {
        let mut model = model_with_queue(4, 1, PlayMode::Shuffle);
        model.status = PlaybackStatus::Playing;
        model.start_seconds = 37.5;
        model.generation = 9;
        model.reload_requested = false;
        model.shuffle_played = vec![true, false, true, false];
        let mut session = None;

        assert!(!handle_command(
            EngineCommand::Reorder { from: 1, to: 3 },
            &mut model,
            &mut session,
        ));

        assert_eq!(
            model
                .queue
                .iter()
                .map(|item| item.public.title.as_str())
                .collect::<Vec<_>>(),
            vec!["t0", "t2", "t3", "t1"]
        );
        assert_eq!(model.index, Some(3));
        assert_eq!(model.shuffle_played, vec![true, true, false, false]);
        assert_eq!(model.status, PlaybackStatus::Playing);
        assert_eq!(model.start_seconds, 37.5);
        assert_eq!(model.generation, 9);
        assert!(!model.reload_requested);
        assert!(session.is_none());
    }

    #[test]
    fn reorder_remaps_indices_shifted_by_another_item() {
        assert_eq!(remap_moved_index(2, 0, 3), 1);
        assert_eq!(remap_moved_index(1, 3, 0), 2);
        assert_eq!(remap_moved_index(0, 2, 3), 0);
    }

    #[test]
    fn stopped_play_restarts_from_zero() {
        let mut model = WorkerModel {
            queue: vec![dummy_item("Dummy")],
            index: Some(0),
            status: PlaybackStatus::Stopped,
            start_seconds: 120.0,
            generation: 4,
            ..WorkerModel::default()
        };
        let mut session = None;

        assert!(!handle_command(
            EngineCommand::Play,
            &mut model,
            &mut session
        ));

        assert_eq!(model.start_seconds, 0.0);
        assert_eq!(model.status, PlaybackStatus::Loading);
        assert!(model.intent_playing);
        assert!(model.reload_requested);
        assert_eq!(model.generation, 5);
    }

    #[test]
    fn sequential_ended_advances_then_stops_at_end() {
        let mut model = model_with_queue(3, 0, PlayMode::Sequential);
        assert_eq!(next_index_on_ended(&mut model), Some(1));
        model.index = Some(1);
        assert_eq!(next_index_on_ended(&mut model), Some(2));
        model.index = Some(2);
        assert_eq!(next_index_on_ended(&mut model), None);
    }

    #[test]
    fn sequential_next_does_not_wrap() {
        let mut model = model_with_queue(3, 2, PlayMode::Sequential);
        assert_eq!(next_index_on_next(&mut model), None);
        model.index = Some(0);
        assert_eq!(next_index_on_next(&mut model), Some(1));
    }

    #[test]
    fn repeat_all_ended_wraps_to_zero() {
        let mut model = model_with_queue(3, 2, PlayMode::RepeatAll);
        assert_eq!(next_index_on_ended(&mut model), Some(0));
        model.index = Some(0);
        assert_eq!(next_index_on_ended(&mut model), Some(1));
    }

    #[test]
    fn repeat_all_next_and_previous_wrap() {
        let mut model = model_with_queue(3, 2, PlayMode::RepeatAll);
        assert_eq!(next_index_on_next(&mut model), Some(0));
        model.index = Some(0);
        assert_eq!(next_index_on_previous(&mut model), Some(2));
    }

    #[test]
    fn repeat_one_ended_stays_on_current() {
        let mut model = model_with_queue(3, 1, PlayMode::RepeatOne);
        assert_eq!(next_index_on_ended(&mut model), Some(1));
        // Explicit Next still advances.
        assert_eq!(next_index_on_next(&mut model), Some(2));
    }

    #[test]
    fn shuffle_ended_and_next_cover_all_then_reset() {
        let mut model = model_with_queue(4, 0, PlayMode::Shuffle);
        model.shuffle_rng = 42;

        let mut seen = [false; 4];
        seen[0] = true; // starting track counted as played
        for _ in 0..3 {
            let next = next_index_on_ended(&mut model).expect("should pick next");
            assert!(
                !seen[next],
                "shuffle should not repeat within a cycle: {next}"
            );
            seen[next] = true;
            model.index = Some(next);
        }
        assert!(seen.iter().all(|&v| v), "all tracks should be covered once");

        // Full cycle exhausted — next advance resets and still returns a valid index.
        let after_reset = next_index_on_ended(&mut model).expect("shuffle must reset, not stall");
        assert!(after_reset < 4);

        // Next command also uses shuffle path.
        model.index = Some(after_reset);
        let via_next = next_index_on_next(&mut model).expect("shuffle next");
        assert!(via_next < 4);
    }

    #[test]
    fn shuffle_survives_mid_cycle_enqueue_and_remove() {
        let mut model = model_with_queue(3, 0, PlayMode::Shuffle);
        model.shuffle_rng = 7;
        // Play through one advance so played set is non-trivial.
        let first = next_index_on_ended(&mut model).unwrap();
        model.index = Some(first);

        let mut session = None;
        assert!(!handle_command(
            EngineCommand::Enqueue(vec![dummy_item("extra")]),
            &mut model,
            &mut session,
        ));
        // Length changed — sync path must not panic; further picks stay in range.
        assert_eq!(model.queue.len(), 4);
        let next = next_index_on_next(&mut model).unwrap();
        assert!(next < model.queue.len());
        model.index = Some(next);

        // Remove an item before current index and ensure index/played set stay consistent.
        let remove_at = 0;
        assert!(!handle_command(
            EngineCommand::Remove(remove_at),
            &mut model,
            &mut session,
        ));
        assert_eq!(model.queue.len(), 3);
        if let Some(idx) = model.index {
            assert!(idx < model.queue.len());
        }
        let again = next_index_on_ended(&mut model);
        if let Some(idx) = again {
            assert!(idx < model.queue.len());
        }
    }

    #[test]
    fn load_respects_start_index_and_enqueue_does_not_interrupt() {
        let mut model = WorkerModel::default();
        let mut session = None;
        assert!(!handle_command(
            EngineCommand::Load {
                items: vec![dummy_item("a"), dummy_item("b"), dummy_item("c")],
                start_index: 2,
            },
            &mut model,
            &mut session,
        ));
        assert_eq!(model.index, Some(2));
        assert!(model.intent_playing);
        assert!(model.reload_requested);

        model.reload_requested = false;
        model.status = PlaybackStatus::Playing;
        let generation = model.generation;
        assert!(!handle_command(
            EngineCommand::Enqueue(vec![dummy_item("d")]),
            &mut model,
            &mut session,
        ));
        assert_eq!(model.queue.len(), 4);
        assert_eq!(model.index, Some(2));
        assert!(!model.reload_requested);
        assert_eq!(model.generation, generation);
    }

    #[test]
    fn replace_current_preserves_position_and_play_intent() {
        let mut model = model_with_queue(2, 1, PlayMode::Sequential);
        model.start_seconds = 42.5;
        model.intent_playing = false;
        model.status = PlaybackStatus::Paused;
        let generation = model.generation;
        let mut session = None;

        assert!(!handle_command(
            EngineCommand::ReplaceCurrent(dummy_item("replacement")),
            &mut model,
            &mut session,
        ));
        assert_eq!(model.index, Some(1));
        assert_eq!(model.queue[1].public.title, "replacement");
        assert_eq!(model.start_seconds, 42.5);
        assert!(!model.intent_playing);
        assert!(model.reload_requested);
        assert_eq!(model.status, PlaybackStatus::Loading);
        assert_eq!(model.generation, generation.wrapping_add(1));
    }

    #[test]
    fn jump_and_clear_queue() {
        let mut model = model_with_queue(3, 0, PlayMode::Sequential);
        let mut session = None;
        assert!(!handle_command(
            EngineCommand::Jump(2),
            &mut model,
            &mut session,
        ));
        assert_eq!(model.index, Some(2));
        assert!(model.reload_requested);

        assert!(!handle_command(
            EngineCommand::ClearQueue,
            &mut model,
            &mut session,
        ));
        assert!(model.queue.is_empty());
        assert_eq!(model.index, None);
        assert_eq!(model.status, PlaybackStatus::Idle);
    }

    #[test]
    fn set_play_mode_command() {
        let mut model = model_with_queue(2, 0, PlayMode::Sequential);
        let mut session = None;
        assert!(!handle_command(
            EngineCommand::SetPlayMode(PlayMode::RepeatOne),
            &mut model,
            &mut session,
        ));
        assert_eq!(model.play_mode, PlayMode::RepeatOne);
    }

    #[test]
    fn audio_callback_path_allocates_nothing_and_uses_only_atomics() {
        let ring = HeapRb::<AbSample>::new(256);
        let (mut producer, mut consumer) = ring.split();
        for _ in 0..128 {
            producer.try_push(AbSample::same(1.0)).unwrap();
        }
        let volume_bits = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let counters = OutputCallbackCounters {
            played_samples: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
            volume_bits: Arc::clone(&volume_bits),
            ab_dry_active: Arc::new(AtomicBool::new(false)),
            output_sample_rate: 48_000,
            output_channels: 2,
        };
        let mut ab_dry_ramp = AbDryRamp::new(48_000);
        let mut output = [0.0f32; 128];
        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
        render_output_callback(&mut output, &mut consumer, &counters, &mut ab_dry_ramp);
        for sample in &output {
            assert_eq!(*sample, 0.5);
        }
        for _ in 0..128 {
            producer.try_push(AbSample::same(1.0)).unwrap();
        }
        volume_bits.store(0.25f32.to_bits(), Ordering::Relaxed);
        render_output_callback(&mut output, &mut consumer, &counters, &mut ab_dry_ramp);
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        assert_eq!(ALLOCATION_COUNT.with(Cell::get), 0);
        for sample in &output {
            assert_eq!(*sample, 0.25);
        }
        assert_eq!(counters.played_samples.load(Ordering::Relaxed), 256);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ab_dry_crossfade_is_stereo_linked_and_completes_in_ten_milliseconds() {
        let ring = HeapRb::<AbSample>::new(64);
        let (mut producer, mut consumer) = ring.split();
        for _ in 0..20 {
            producer
                .try_push(AbSample {
                    processed: 1.0,
                    dry: 0.0,
                })
                .unwrap();
        }
        let ab_dry_active = Arc::new(AtomicBool::new(false));
        let counters = OutputCallbackCounters {
            played_samples: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            ab_dry_active: Arc::clone(&ab_dry_active),
            output_sample_rate: 1_000,
            output_channels: 2,
        };
        let mut ramp = AbDryRamp::new(1_000);
        assert_eq!(ramp.advance(false), 0.0);
        ab_dry_active.store(true, Ordering::Release);
        let mut output = [0.0f32; 20];
        render_output_callback(&mut output, &mut consumer, &counters, &mut ramp);
        for frame in output.chunks_exact(2) {
            assert_eq!(frame[0], frame[1]);
        }
        assert!((output[0] - 0.9).abs() < 1.0e-6);
        assert_eq!(output[18], 0.0);
        assert_eq!(output[19], 0.0);
    }

    #[test]
    fn ab_dry_ramp_hits_endpoints_at_exact_device_rate_frame_counts() {
        for sample_rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
            let ramp_frames = (u64::from(sample_rate) + 50) / 100;
            let mut ramp = AbDryRamp::new(sample_rate);
            assert_eq!(ramp.advance(false), 0.0);

            for _ in 1..ramp_frames {
                assert!(ramp.advance(true) < 1.0, "sample_rate={sample_rate}");
            }
            assert_eq!(ramp.advance(true), 1.0, "sample_rate={sample_rate}");

            for _ in 1..ramp_frames {
                assert!(ramp.advance(false) > 0.0, "sample_rate={sample_rate}");
            }
            assert_eq!(ramp.advance(false), 0.0, "sample_rate={sample_rate}");
        }

        assert_eq!(AbDryRamp::new(0).ramp_frames, 1);
        assert_eq!(AbDryRamp::new(u32::MAX).ramp_frames, 42_949_673);
    }

    #[test]
    fn rebuilt_callback_uses_current_ab_state_on_its_first_frame() {
        let ring = HeapRb::<AbSample>::new(4);
        let (mut producer, mut consumer) = ring.split();
        for _ in 0..2 {
            producer
                .try_push(AbSample {
                    processed: 1.0,
                    dry: 0.0,
                })
                .unwrap();
        }
        let ab_dry_active = Arc::new(AtomicBool::new(false));
        let counters = OutputCallbackCounters {
            played_samples: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            ab_dry_active: Arc::clone(&ab_dry_active),
            output_sample_rate: 48_000,
            output_channels: 2,
        };
        let mut ramp = AbDryRamp::new(48_000);

        // Model a stream constructed while wet and prebuffered until after A/B was pressed.
        ab_dry_active.store(true, Ordering::Relaxed);
        let mut output = [1.0f32; 2];
        render_output_callback(&mut output, &mut consumer, &counters, &mut ramp);

        assert_eq!(output, [0.0, 0.0]);
    }

    #[test]
    fn callback_does_not_consume_a_partial_interleaved_frame() {
        let ring = HeapRb::<AbSample>::new(8);
        let (mut producer, mut consumer) = ring.split();
        producer.try_push(AbSample::same(0.25)).unwrap();
        let counters = OutputCallbackCounters {
            played_samples: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            ab_dry_active: Arc::new(AtomicBool::new(false)),
            output_sample_rate: 48_000,
            output_channels: 2,
        };
        let mut ramp = AbDryRamp::new(48_000);
        let mut output = [1.0f32; 2];

        render_output_callback(&mut output, &mut consumer, &counters, &mut ramp);
        assert_eq!(output, [0.0, 0.0]);
        assert_eq!(counters.played_samples.load(Ordering::Relaxed), 0);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 1);

        producer.try_push(AbSample::same(0.75)).unwrap();
        render_output_callback(&mut output, &mut consumer, &counters, &mut ramp);
        assert_eq!(output, [0.25, 0.75]);
        assert_eq!(counters.played_samples.load(Ordering::Relaxed), 2);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn engine_diagnostics_are_bounded_and_never_include_error_details() {
        let diagnostics = EngineDiagnosticQueue::default();
        for generation in 0..(DIAGNOSTIC_CAPACITY + 5) {
            diagnostics.push(EngineDiagnostic {
                category: "playback_start_failed",
                source: "online",
                summary: format!("stage=session_new code=network generation={generation}"),
                generation: Some(generation as u64),
            });
        }
        let drained = diagnostics.drain();
        assert_eq!(drained.len(), DIAGNOSTIC_CAPACITY);
        assert_eq!(drained[0].generation, Some(5));
        assert!(drained.iter().all(|entry| !entry.summary.contains("http")));
    }

    #[test]
    fn playback_error_codes_are_finite_and_path_free() {
        let error = anyhow!("failed to open C:\\Users\\private\\song.flac: not found");
        assert_eq!(playback_error_code(&error), "io");
        assert_eq!(
            playback_source_label(&PlaybackSource::Local(PathBuf::from("cache.bin")), true),
            "cache"
        );
    }
}
