//! Allocation-free-in-process DSP building blocks for GXPlayer.
//!
//! Configuration may allocate on the decode/DSP worker. Steady-state processing mutates an
//! existing PCM slice and performs no allocation; the short crossfade window after `set_settings`
//! may grow a worker-owned scratch buffer. A disabled chain with no crossfade in flight returns
//! before reading or writing any sample.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod kemar;
pub mod quality;
mod spatial;

use spatial::{CrossfeedProcessor, EarlyReflections, LinkedLimiter, StereoHrtf};
pub use spatial::{CrossfeedSettings, HrtfSettings, LimiterSettings, RoomSettings};

/// Length of the crossfade that masks a `set_settings` swap. Long enough to hide the HRTF
/// 0<->128-frame latency step and the zeroed filter state of a freshly built generation, short
/// enough to feel instant.
const SETTINGS_CROSSFADE_SECONDS: f64 = 0.010;

#[cfg(test)]
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
struct CountingAllocator;

#[cfg(test)]
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

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EqBand {
    pub enabled: bool,
    pub kind: FilterKind,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl EqBand {
    pub fn peak(frequency_hz: f32, gain_db: f32, q: f32) -> Self {
        Self {
            enabled: true,
            kind: FilterKind::Peak,
            frequency_hz,
            gain_db,
            q,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DspSettings {
    pub enabled: bool,
    pub eq_enabled: bool,
    pub eq_bands: Vec<EqBand>,
    #[serde(default)]
    pub crossfeed: CrossfeedSettings,
    /// Runs ahead of the HRTF so reflections are spatialised with the direct sound.
    #[serde(default)]
    pub room: RoomSettings,
    #[serde(default)]
    pub hrtf: HrtfSettings,
    #[serde(default)]
    pub limiter: LimiterSettings,
}

impl Default for DspSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            eq_enabled: false,
            eq_bands: vec![EqBand::peak(1_000.0, 0.0, 1.0)],
            crossfeed: CrossfeedSettings::default(),
            room: RoomSettings::default(),
            hrtf: HrtfSettings::default(),
            limiter: LimiterSettings::default(),
        }
    }
}

impl DspSettings {
    /// Conservatively clamps parameters whose validity depends on the sample rate, so settings
    /// that were validated at a higher rate (persisted presets are checked at 48 kHz) still build
    /// on a lower-rate output device. Near-Nyquist EQ bands and the crossfeed cutoff are pinned to
    /// the highest value the device accepts; values that are invalid at every rate are left
    /// untouched so they keep failing loudly.
    pub fn clamped_for_sample_rate(mut self, sample_rate: u32) -> Self {
        let max_frequency = max_band_frequency_hz(sample_rate);
        for band in &mut self.eq_bands {
            if band.frequency_hz.is_finite() && band.frequency_hz > max_frequency {
                band.frequency_hz = max_frequency;
            }
        }
        let max_cutoff = spatial::max_crossfeed_cutoff_hz(sample_rate);
        if self.crossfeed.cutoff_hz.is_finite() && self.crossfeed.cutoff_hz > max_cutoff {
            self.crossfeed.cutoff_hz = max_cutoff;
        }
        self
    }
}

/// Highest EQ band frequency accepted by `BiquadCoefficients::from_band` at `sample_rate`.
fn max_band_frequency_hz(sample_rate: u32) -> f32 {
    sample_rate as f32 * 0.5 * 0.999
}

#[derive(Debug, Error, PartialEq)]
pub enum DspError {
    #[error("sample rate must be greater than zero")]
    InvalidSampleRate,
    #[error("channel count must be greater than zero")]
    InvalidChannels,
    #[error("PCM sample count {samples} is not divisible by channel count {channels}")]
    MisalignedPcm { samples: usize, channels: usize },
    #[error(
        "A/B dry sample count {ab_dry_samples} does not match processed sample count {processed_samples}"
    )]
    MismatchedAbDry {
        processed_samples: usize,
        ab_dry_samples: usize,
    },
    #[error("EQ frequency {frequency_hz} Hz must be between 5 Hz and {max_hz} Hz")]
    InvalidFrequency { frequency_hz: f32, max_hz: f32 },
    #[error("EQ Q {0} must be in the range 0.05..=30")]
    InvalidQ(f32),
    #[error("EQ gain {0} dB must be in the range -30..=30")]
    InvalidGain(f32),
    #[error("Crossfeed amount {0} must be in the range 0..=0.5")]
    InvalidCrossfeedAmount(f32),
    #[error("Crossfeed delay {0} ms must be in the range 0.05..=1")]
    InvalidCrossfeedDelay(f32),
    #[error("Crossfeed cutoff {0} Hz is invalid")]
    InvalidCrossfeedCutoff(f32),
    #[error("HRTF mix {0} must be in the range 0..=1")]
    InvalidHrtfMix(f32),
    #[error("HRTF output gain {0} dB must be in the range -24..=6")]
    InvalidHrtfGain(f32),
    #[error("room amount {0} must be in the range 0..=1")]
    InvalidRoomAmount(f32),
    #[error("room size {0} must be in the range 0..=1")]
    InvalidRoomSize(f32),
    #[error("limiter ceiling {0} dB must be in the range -12..=0")]
    InvalidLimiterCeiling(f32),
    #[error("limiter release {0} ms must be in the range 10..=1000")]
    InvalidLimiterRelease(f32),
    #[error("Crossfeed and stereo HRTF require exactly two channels, got {0}")]
    UnsupportedSpatialChannels(usize),
}

/// One buildable generation of processors for a settings value.
struct Processors {
    equalizer: ParametricEq,
    crossfeed: Option<CrossfeedProcessor>,
    room: Option<EarlyReflections>,
    hrtf: Option<StereoHrtf>,
    limiter: Option<LinkedLimiter>,
    /// Gain applied to the A/B reference lane so it matches the processed lane's
    /// loudness. Derived from the settings at build time; 1.0 when nothing shifts
    /// the broadband level.
    ab_dry_gain: f32,
}

impl Processors {
    fn process(&mut self, pcm: &mut [f32], settings: &DspSettings, channels: usize) {
        if !settings.enabled {
            return;
        }
        if settings.eq_enabled {
            self.equalizer.process_interleaved_in_place(pcm);
        }
        if let Some(crossfeed) = &mut self.crossfeed {
            crossfeed.process(pcm);
        }
        if let Some(room) = &mut self.room {
            room.process(pcm);
        }
        if let Some(hrtf) = &mut self.hrtf {
            hrtf.process(pcm);
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.process(pcm, channels);
        }
    }

    fn process_with_ab_dry(
        &mut self,
        pcm: &mut [f32],
        ab_dry: &mut [f32],
        settings: &DspSettings,
        channels: usize,
    ) {
        ab_dry.copy_from_slice(pcm);
        if !settings.enabled {
            return;
        }
        if settings.eq_enabled {
            self.equalizer.process_interleaved_in_place(pcm);
        }
        if let Some(crossfeed) = &mut self.crossfeed {
            crossfeed.process(pcm);
        }
        if let Some(room) = &mut self.room {
            room.process(pcm);
        }
        if let Some(hrtf) = &mut self.hrtf {
            hrtf.process_with_ab_dry(pcm, ab_dry);
        }
        // Level-match the reference lane so the A/B answers "does this sound better"
        // rather than "which one is louder". The factor is fixed by the settings, so the
        // lane stays chunk-invariant and free of the pumping an adaptive matcher would
        // add on transients.
        //
        // Ahead of the limiter, so the ceiling covers a matched-up reference rather than
        // being applied before the gain that could push it over.
        if self.ab_dry_gain != 1.0 {
            for sample in ab_dry.iter_mut() {
                *sample *= self.ab_dry_gain;
            }
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.process_with_ab_dry(pcm, ab_dry, channels);
        }
    }
}

/// The previous processor generation, kept alive after `set_settings` so the processed output can
/// crossfade to the new generation instead of jumping across an HRTF latency step or zeroed
/// filter state.
struct RetiringProcessors {
    settings: DspSettings,
    processors: Processors,
    crossfaded_frames: usize,
    crossfade_frames: usize,
    /// Scratch for the retiring generation's output. Only touched while a crossfade is in flight,
    /// so the steady-state processing path stays allocation-free.
    scratch: Vec<f32>,
}

/// Blends the retiring generation's output (in `retiring.scratch`) into `pcm` with a per-frame
/// linear ramp toward the new generation. Returns `true` once the crossfade window is complete.
fn crossfade_from_retiring(
    pcm: &mut [f32],
    retiring: &mut RetiringProcessors,
    channels: usize,
) -> bool {
    let total = retiring.crossfade_frames;
    for (new_frame, retired_frame) in pcm
        .chunks_exact_mut(channels)
        .zip(retiring.scratch.chunks_exact(channels))
    {
        if retiring.crossfaded_frames >= total {
            break;
        }
        retiring.crossfaded_frames += 1;
        if retiring.crossfaded_frames >= total {
            // The final step lands on the new generation's untouched samples; blending with a
            // zero weight could still smear a non-finite retired sample into the output.
            break;
        }
        let new_weight = retiring.crossfaded_frames as f32 / total as f32;
        let retired_weight = 1.0 - new_weight;
        for (new_sample, retired_sample) in new_frame.iter_mut().zip(retired_frame) {
            *new_sample = *retired_sample * retired_weight + *new_sample * new_weight;
        }
    }
    retiring.crossfaded_frames >= total
}

pub struct DspChain {
    sample_rate: u32,
    channels: usize,
    settings: DspSettings,
    processors: Processors,
    retiring: Option<RetiringProcessors>,
}

impl DspChain {
    pub fn new(sample_rate: u32, channels: usize, settings: DspSettings) -> Result<Self, DspError> {
        if sample_rate == 0 {
            return Err(DspError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(DspError::InvalidChannels);
        }
        let processors = build_processors(sample_rate, channels, &settings)?;
        Ok(Self {
            sample_rate,
            channels,
            settings,
            processors,
            retiring: None,
        })
    }

    pub fn settings(&self) -> &DspSettings {
        &self.settings
    }

    /// Swaps in processors for `settings`, crossfading the processed output from the previous
    /// generation over a short window so mid-playback preset changes stay click-free.
    ///
    /// A bypass-to-bypass change swaps without a crossfade: both generations are pure copies and
    /// blend arithmetic would break the bit-exact passthrough guarantee. On error the previous
    /// state stays authoritative.
    pub fn set_settings(&mut self, settings: DspSettings) -> Result<(), DspError> {
        if settings == self.settings {
            // Rebuilding identical settings would only reset filter state audibly.
            return Ok(());
        }
        let processors = build_processors(self.sample_rate, self.channels, &settings)?;
        let previous_processors = std::mem::replace(&mut self.processors, processors);
        let previous_settings = std::mem::replace(&mut self.settings, settings);
        if previous_settings.enabled || self.settings.enabled {
            let crossfade_frames =
                ((self.sample_rate as f64 * SETTINGS_CROSSFADE_SECONDS).round() as usize).max(1);
            self.retiring = Some(RetiringProcessors {
                settings: previous_settings,
                processors: previous_processors,
                crossfaded_frames: 0,
                crossfade_frames,
                scratch: Vec::new(),
            });
        }
        // Bypass -> bypass keeps any crossfade already in flight: it keeps fading against the
        // (identical) copy output of the new generation.
        Ok(())
    }

    /// Frames of delay the chain adds, for the caller's A/V alignment.
    ///
    /// The head model contributes the convolver's block delay plus the arrival time
    /// inside the impulse response itself, so this asks the processor rather than
    /// assuming the block delay is all of it.
    pub fn latency_frames(&self) -> usize {
        if !self.settings.enabled {
            return 0;
        }
        self.processors
            .hrtf
            .as_ref()
            .map(StereoHrtf::latency_frames)
            .unwrap_or(0)
    }

    pub fn process_interleaved_in_place(&mut self, pcm: &mut [f32]) -> Result<(), DspError> {
        if !self.settings.enabled && self.retiring.is_none() {
            return Ok(());
        }
        if !pcm.len().is_multiple_of(self.channels) {
            return Err(DspError::MisalignedPcm {
                samples: pcm.len(),
                channels: self.channels,
            });
        }
        if let Some(mut retiring) = self.retiring.take() {
            retiring.scratch.clear();
            retiring.scratch.extend_from_slice(pcm);
            retiring
                .processors
                .process(&mut retiring.scratch, &retiring.settings, self.channels);
            self.processors.process(pcm, &self.settings, self.channels);
            if !crossfade_from_retiring(pcm, &mut retiring, self.channels) {
                self.retiring = Some(retiring);
            }
            return Ok(());
        }
        self.processors.process(pcm, &self.settings, self.channels);
        Ok(())
    }

    /// Processes `pcm` through the configured chain while writing an untreated A/B lane into the
    /// caller-provided `ab_dry` buffer.
    ///
    /// The A/B lane starts as an exact copy of the input. EQ and Crossfeed affect only `pcm`. When
    /// HRTF is enabled, both lanes use the same fixed 128-frame dry queue so the untreated lane is
    /// aligned with the processed HRTF output. The limiter derives one linked gain from `pcm` and
    /// applies that same gain to both lanes. During a `set_settings` crossfade the retiring
    /// generation contributes only to the processed lane; the A/B lane follows the new generation
    /// so its frame alignment never blends two delays. Steady-state processing performs no heap
    /// allocation.
    pub fn process_interleaved_with_ab_dry(
        &mut self,
        pcm: &mut [f32],
        ab_dry: &mut [f32],
    ) -> Result<(), DspError> {
        if pcm.len() != ab_dry.len() {
            return Err(DspError::MismatchedAbDry {
                processed_samples: pcm.len(),
                ab_dry_samples: ab_dry.len(),
            });
        }
        if !pcm.len().is_multiple_of(self.channels) {
            return Err(DspError::MisalignedPcm {
                samples: pcm.len(),
                channels: self.channels,
            });
        }

        if let Some(mut retiring) = self.retiring.take() {
            retiring.scratch.clear();
            retiring.scratch.extend_from_slice(pcm);
            retiring
                .processors
                .process(&mut retiring.scratch, &retiring.settings, self.channels);
            self.processors
                .process_with_ab_dry(pcm, ab_dry, &self.settings, self.channels);
            if !crossfade_from_retiring(pcm, &mut retiring, self.channels) {
                self.retiring = Some(retiring);
            }
            return Ok(());
        }
        self.processors
            .process_with_ab_dry(pcm, ab_dry, &self.settings, self.channels);
        Ok(())
    }
}

/// Probe points for the level estimate, and how much of a mix's energy each stands
/// for. Music is not flat: the weights follow the broad shape of a pink-ish spectrum,
/// so a 60 Hz shelf lift does not get credited with the same loudness change as the
/// same lift across the vocal range.
const AB_MATCH_PROBES: [(f32, f32); 10] = [
    (40.0, 0.06),
    (80.0, 0.10),
    (160.0, 0.13),
    (320.0, 0.15),
    (640.0, 0.15),
    (1_280.0, 0.13),
    (2_560.0, 0.11),
    (5_120.0, 0.08),
    (10_240.0, 0.06),
    (16_000.0, 0.03),
];

/// Gain that brings the untreated lane up (or down) to the EQ'd lane's loudness.
///
/// The EQ is the only stage that moves the broadband level by design: the head model
/// is equalised to unity, crossfeed redistributes energy between ears rather than
/// adding it, and the limiter already shares its gain reduction with both lanes. So
/// the estimate is the EQ's weighted mean magnitude, evaluated from the same biquad
/// coefficients the filters run.
///
/// Bands above the sample rate's guard cannot build and are skipped, matching what
/// `ParametricEq` actually instantiates.
fn ab_dry_match_gain(sample_rate: u32, bands: &[EqBand]) -> f32 {
    let coefficients = bands
        .iter()
        .copied()
        .filter(|band| band.enabled)
        .filter_map(|band| BiquadCoefficients::from_band(sample_rate, band).ok())
        .collect::<Vec<_>>();
    if coefficients.is_empty() {
        return 1.0;
    }

    // Summed as power, not as decibels. Loudness follows the energy sum, so a weighted
    // mean of dB values would under-correct every boost: one band lifted 8 dB moves the
    // total level by more than its share of a logarithmic average.
    let mut weighted_power = 0.0f64;
    let mut total_weight = 0.0f64;
    for (frequency, weight) in AB_MATCH_PROBES {
        // A probe above the usable band says nothing about this device's output.
        if frequency >= sample_rate as f32 * 0.5 {
            continue;
        }
        let mut magnitude_db = 0.0f32;
        for coefficient in &coefficients {
            magnitude_db += coefficient.magnitude_db_at(sample_rate, frequency);
        }
        let magnitude = 10.0f64.powf(magnitude_db as f64 / 20.0);
        weighted_power += weight as f64 * magnitude * magnitude;
        total_weight += weight as f64;
    }
    if total_weight <= 0.0 {
        return 1.0;
    }

    let gain = (weighted_power / total_weight).sqrt() as f32;
    if !gain.is_finite() || gain <= 0.0 {
        return 1.0;
    }
    gain.clamp(AB_MATCH_MIN_GAIN, AB_MATCH_MAX_GAIN)
}

/// The two directions are not symmetric, because their risks are not.
///
/// Attenuating the reference can never clip it, and the limit has to clear what real
/// curves ask for: ten bands at the editor's -12 dB floor stack into roughly -19 dB of
/// broadband cut, and a match that stopped at -6 dB would leave the rest of that as an
/// audible level difference — exactly the confound this is here to remove.
///
/// Boosting is capped much tighter, because a boosted reference is one that can clip.
/// The cap is what keeps a curve that lifts where the recording has nothing to lift from
/// making the reference the hot lane; past it the match is deliberately incomplete, and
/// the limiter holds the ceiling for whatever is left.
const AB_MATCH_MIN_GAIN: f32 = 0.063_095_73; // -24 dB
const AB_MATCH_MAX_GAIN: f32 = 1.995_262_3; // +6 dB

fn build_processors(
    sample_rate: u32,
    channels: usize,
    settings: &DspSettings,
) -> Result<Processors, DspError> {
    if (settings.crossfeed.enabled || settings.room.enabled || settings.hrtf.enabled)
        && channels != 2
    {
        return Err(DspError::UnsupportedSpatialChannels(channels));
    }
    let equalizer = ParametricEq::new(sample_rate, channels, &settings.eq_bands)?;
    let crossfeed = settings
        .crossfeed
        .enabled
        .then(|| CrossfeedProcessor::new(sample_rate, &settings.crossfeed))
        .transpose()?;
    let room = settings
        .room
        .enabled
        .then(|| EarlyReflections::new(sample_rate, &settings.room))
        .transpose()?;
    let hrtf = settings
        .hrtf
        .enabled
        .then(|| StereoHrtf::new(sample_rate, &settings.hrtf))
        .transpose()?;
    let limiter = settings
        .limiter
        .enabled
        .then(|| LinkedLimiter::new(sample_rate, &settings.limiter))
        .transpose()?;
    let ab_dry_gain = if settings.enabled && settings.eq_enabled {
        ab_dry_match_gain(sample_rate, &settings.eq_bands)
    } else {
        1.0
    };
    Ok(Processors {
        equalizer,
        crossfeed,
        room,
        hrtf,
        limiter,
        ab_dry_gain,
    })
}

struct ParametricEq {
    channels: usize,
    bands: Vec<BandProcessor>,
}

impl ParametricEq {
    fn new(sample_rate: u32, channels: usize, bands: &[EqBand]) -> Result<Self, DspError> {
        let bands = bands
            .iter()
            .copied()
            .filter(|band| band.enabled)
            .map(|band| BandProcessor::new(sample_rate, channels, band))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { channels, bands })
    }

    fn process_interleaved_in_place(&mut self, pcm: &mut [f32]) {
        for band in &mut self.bands {
            for frame in pcm.chunks_exact_mut(self.channels) {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample = band.states[channel].process(*sample, band.coefficients);
                }
            }
        }
    }
}

struct BandProcessor {
    coefficients: BiquadCoefficients,
    states: Vec<BiquadState>,
}

impl BandProcessor {
    fn new(sample_rate: u32, channels: usize, band: EqBand) -> Result<Self, DspError> {
        let coefficients = BiquadCoefficients::from_band(sample_rate, band)?;
        Ok(Self {
            coefficients,
            states: vec![BiquadState::default(); channels],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoefficients {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoefficients {
    pub fn from_band(sample_rate: u32, band: EqBand) -> Result<Self, DspError> {
        let nyquist_guard = max_band_frequency_hz(sample_rate);
        if !band.frequency_hz.is_finite()
            || band.frequency_hz < 5.0
            || band.frequency_hz > nyquist_guard
        {
            return Err(DspError::InvalidFrequency {
                frequency_hz: band.frequency_hz,
                max_hz: nyquist_guard,
            });
        }
        if !band.q.is_finite() || !(0.05..=30.0).contains(&band.q) {
            return Err(DspError::InvalidQ(band.q));
        }
        if !band.gain_db.is_finite() || !(-30.0..=30.0).contains(&band.gain_db) {
            return Err(DspError::InvalidGain(band.gain_db));
        }
        if matches!(
            band.kind,
            FilterKind::Peak | FilterKind::LowShelf | FilterKind::HighShelf
        ) && band.gain_db == 0.0
        {
            return Ok(Self::IDENTITY);
        }

        let omega = 2.0 * PI * band.frequency_hz as f64 / sample_rate as f64;
        let sin = omega.sin();
        let cos = omega.cos();
        let alpha = sin / (2.0 * band.q as f64);
        let a = 10.0f64.powf(band.gain_db as f64 / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            FilterKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            FilterKind::LowPass => {
                let b0 = (1.0 - cos) * 0.5;
                (b0, 1.0 - cos, b0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterKind::HighPass => {
                let b0 = (1.0 + cos) * 0.5;
                (b0, -(1.0 + cos), b0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterKind::LowShelf => {
                let root = a.sqrt();
                let term = 2.0 * root * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + term),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - term),
                    (a + 1.0) + (a - 1.0) * cos + term,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - term,
                )
            }
            FilterKind::HighShelf => {
                let root = a.sqrt();
                let term = 2.0 * root * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + term),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - term),
                    (a + 1.0) - (a - 1.0) * cos + term,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - term,
                )
            }
        };
        Ok(Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        })
    }

    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Magnitude response at one frequency, in dB. Evaluates `H(z)` on the unit circle
    /// directly from the coefficients, so it reports what the running filter does rather
    /// than what the band asked for.
    fn magnitude_db_at(self, sample_rate: u32, frequency_hz: f32) -> f32 {
        let omega = 2.0 * PI * frequency_hz as f64 / sample_rate as f64;
        let (cos1, sin1) = (omega.cos(), omega.sin());
        let (cos2, sin2) = ((2.0 * omega).cos(), (2.0 * omega).sin());
        let numerator_real = self.b0 as f64 + self.b1 as f64 * cos1 + self.b2 as f64 * cos2;
        let numerator_imaginary = -(self.b1 as f64 * sin1 + self.b2 as f64 * sin2);
        let denominator_real = 1.0 + self.a1 as f64 * cos1 + self.a2 as f64 * cos2;
        let denominator_imaginary = -(self.a1 as f64 * sin1 + self.a2 as f64 * sin2);
        let numerator = numerator_real.hypot(numerator_imaginary);
        let denominator = denominator_real.hypot(denominator_imaginary);
        if denominator <= f64::MIN_POSITIVE || numerator <= 0.0 {
            return 0.0;
        }
        (20.0 * (numerator / denominator).log10()) as f32
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

impl BiquadState {
    #[inline]
    fn process(&mut self, input: f32, c: BiquadCoefficients) -> f32 {
        let output = c.b0 * input + self.z1;
        self.z1 = c.b1 * input - c.a1 * output + self.z2;
        self.z2 = c.b2 * input - c.a2 * output;
        output
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn disabled_chain_is_bitwise_transparent() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: false,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 12.0, 0.7)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = vec![
            f32::from_bits(0),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f12_3456),
            f32::from_bits(0x7fc0_1234),
        ];
        let before = pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        let after = pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        assert_eq!(before, after);
    }

    #[test]
    fn disabled_dual_chain_is_bitwise_transparent() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: false,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 12.0, 0.7)],
                crossfeed: CrossfeedSettings {
                    enabled: true,
                    ..CrossfeedSettings::default()
                },
                room: RoomSettings {
                    enabled: true,
                    ..RoomSettings::default()
                },
                hrtf: HrtfSettings {
                    enabled: true,
                    ..HrtfSettings::default()
                },
                limiter: LimiterSettings {
                    enabled: true,
                    ..LimiterSettings::default()
                },
            },
        )
        .unwrap();
        let mut pcm = vec![
            f32::from_bits(0),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f12_3456),
            f32::from_bits(0x7fc0_1234),
        ];
        let before = pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        let mut ab_dry = vec![42.0; pcm.len()];

        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();

        assert_eq!(
            pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            before
        );
        assert_eq!(
            ab_dry
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn eq_disabled_is_bitwise_transparent() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: false,
                eq_bands: vec![EqBand::peak(1_000.0, 12.0, 0.7)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = vec![0.1, -0.1, 0.25, -0.25];
        let before = pcm.clone();
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        assert_eq!(before, pcm);
    }

    #[test]
    fn zero_db_gain_is_exact_identity() {
        let coefficients =
            BiquadCoefficients::from_band(48_000, EqBand::peak(1_000.0, 0.0, 1.0)).unwrap();
        assert_eq!(coefficients, BiquadCoefficients::IDENTITY);
    }

    #[test]
    fn rbj_peak_coefficients_match_golden_reference() {
        let coefficients =
            BiquadCoefficients::from_band(48_000, EqBand::peak(1_000.0, 6.0, 1.0)).unwrap();
        let expected = [
            1.043_953_1,
            -1.895_320_8,
            0.867_722_3,
            -1.895_320_8,
            0.911_675_4,
        ];
        let actual = [
            coefficients.b0,
            coefficients.b1,
            coefficients.b2,
            coefficients.a1,
            coefficients.a2,
        ];
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
    }

    #[test]
    fn processing_performs_no_heap_allocation() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 6.0, 1.0)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = vec![0.1f32; 4096];
        chain.process_interleaved_in_place(&mut pcm).unwrap();

        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        let allocations = ALLOCATION_COUNT.with(Cell::get);
        assert_eq!(allocations, 0);
    }

    #[test]
    fn dual_spatial_processing_performs_no_heap_allocation() {
        let mut chain = DspChain::new(48_000, 2, enabled_spatial_settings()).unwrap();
        let mut pcm = vec![0.1f32; 4096];
        let mut ab_dry = vec![0.0f32; pcm.len()];

        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        assert_eq!(ALLOCATION_COUNT.with(Cell::get), 0);
    }

    #[test]
    fn peak_filter_reaches_requested_center_gain() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 6.0, 1.0)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let frames = 96_000;
        let mut pcm = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let sample = (frame as f32 * 1_000.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.1;
            pcm.extend_from_slice(&[sample, sample]);
        }
        let input_rms = rms(&pcm[96_000..]);
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        let output_rms = rms(&pcm[96_000..]);
        let measured_db = 20.0 * (output_rms / input_rms).log10();
        assert!(
            (measured_db - 6.0).abs() < 0.08,
            "measured {measured_db:.3} dB"
        );
    }

    #[test]
    fn aggressive_valid_chain_remains_finite() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![
                    EqBand::peak(60.0, 24.0, 0.2),
                    EqBand::peak(1_000.0, -24.0, 10.0),
                    EqBand {
                        enabled: true,
                        kind: FilterKind::HighShelf,
                        frequency_hz: 8_000.0,
                        gain_db: 18.0,
                        q: 0.7,
                    },
                ],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut impulse = vec![0.0f32; 96_000];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        chain.process_interleaved_in_place(&mut impulse).unwrap();
        assert!(impulse.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn rejects_invalid_configuration_and_misaligned_pcm() {
        assert!(matches!(
            BiquadCoefficients::from_band(48_000, EqBand::peak(30_000.0, 0.0, 1.0)),
            Err(DspError::InvalidFrequency { .. })
        ));
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: Vec::new(),
                ..DspSettings::default()
            },
        )
        .unwrap();
        assert_eq!(
            chain.process_interleaved_in_place(&mut [0.0]),
            Err(DspError::MisalignedPcm {
                samples: 1,
                channels: 2
            })
        );
        let mut processed = [0.25, -0.25];
        let mut ab_dry = [9.0];
        assert_eq!(
            chain.process_interleaved_with_ab_dry(&mut processed, &mut ab_dry),
            Err(DspError::MismatchedAbDry {
                processed_samples: 2,
                ab_dry_samples: 1,
            })
        );
        assert_eq!(processed, [0.25, -0.25]);
        assert_eq!(ab_dry, [9.0]);
    }

    #[test]
    fn crossfeed_impulse_uses_bounded_delayed_low_pass_crosstalk() {
        let settings = DspSettings {
            enabled: true,
            crossfeed: CrossfeedSettings {
                enabled: true,
                amount: 0.2,
                delay_ms: 0.25,
                cutoff_hz: 700.0,
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, settings).unwrap();
        let mut impulse = vec![0.0f32; 128];
        impulse[1] = 1.0;
        chain.process_interleaved_in_place(&mut impulse).unwrap();
        assert!((impulse[1] - 0.9).abs() < 1.0e-6);
        let delayed_frame = 12;
        assert_eq!(impulse[(delayed_frame - 1) * 2], 0.0);
        assert!(impulse[delayed_frame * 2] > 0.0);
        assert!(impulse.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn early_reflections_arrive_crossed_after_the_direct_sound() {
        let settings = DspSettings {
            enabled: true,
            room: RoomSettings {
                enabled: true,
                amount: 0.5,
                size: 1.0,
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, settings).unwrap();
        // Left-only impulse: every reflection of it must appear on the right.
        let mut pcm = vec![0.0f32; 48_000 * 2];
        pcm[0] = 1.0;
        chain.process_interleaved_in_place(&mut pcm).unwrap();

        // The direct sound is untouched and nothing precedes it.
        assert!((pcm[0] - 1.0).abs() < 1.0e-6);

        let per_ms = 48.0;
        let first_tap = (11.0 * per_ms) as usize;
        // Nothing may arrive before the first reflection: an earlier arrival would
        // colour the direct sound instead of reading as a room.
        let early_energy: f32 = (1..first_tap - 1)
            .map(|frame| pcm[frame * 2].abs() + pcm[frame * 2 + 1].abs())
            .sum();
        assert!(
            early_energy < 1.0e-4,
            "energy before the first tap: {early_energy}"
        );

        // A left impulse reflects onto the right ear.
        let right_energy: f32 = (first_tap..(60.0 * per_ms) as usize)
            .map(|frame| pcm[frame * 2 + 1].abs())
            .sum();
        assert!(right_energy > 0.01, "no crossed reflection: {right_energy}");

        // Reflections decay: the late window is quieter than the early one.
        let window = |from_ms: f32, to_ms: f32| -> f32 {
            ((from_ms * per_ms) as usize..(to_ms * per_ms) as usize)
                .map(|frame| pcm[frame * 2].abs() + pcm[frame * 2 + 1].abs())
                .sum()
        };
        assert!(window(10.0, 22.0) > window(30.0, 42.0));

        // And they stop: nothing lingers a second later, so this is not a reverb tail.
        assert!(window(500.0, 999.0) < 1.0e-6);
        assert!(pcm.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn early_reflections_stay_bounded_and_are_skipped_when_silent() {
        // amount = 0 must be inaudible rather than merely quiet, so the preset's
        // lowest setting is honestly "off".
        let silent = DspSettings {
            enabled: true,
            room: RoomSettings {
                enabled: true,
                amount: 0.0,
                size: 0.5,
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, silent).unwrap();
        let mut pcm = vec![0.0f32; 4_096];
        pcm[0] = 1.0;
        pcm[1] = 1.0;
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        assert!((pcm[0] - 1.0).abs() < 1.0e-6);
        assert!(pcm[2..].iter().all(|sample| sample.abs() < 1.0e-6));

        // Full-scale sustained input with the strongest room must not run away.
        let loud = DspSettings {
            enabled: true,
            room: RoomSettings {
                enabled: true,
                amount: 1.0,
                size: 1.0,
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, loud).unwrap();
        let mut pcm = vec![1.0f32; 48_000 * 2];
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        let peak = pcm.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        // Taps sum to a finite gain; there is no feedback path, so this is bounded.
        assert!(peak.is_finite() && peak < 4.0, "peak {peak}");
    }

    #[test]
    fn room_rejects_out_of_range_settings() {
        for (amount, size) in [(1.5, 0.5), (-0.1, 0.5), (f32::NAN, 0.5), (0.5, 2.0)] {
            let settings = DspSettings {
                enabled: true,
                room: RoomSettings {
                    enabled: true,
                    amount,
                    size,
                },
                ..DspSettings::default()
            };
            assert!(
                DspChain::new(48_000, 2, settings).is_err(),
                "accepted amount={amount} size={size}"
            );
        }
    }

    #[test]
    fn hrtf_impulse_matches_embedded_kemar_golden_samples() {
        let settings = DspSettings {
            enabled: true,
            hrtf: HrtfSettings {
                enabled: true,
                mix: 1.0,
                output_gain_db: 0.0,
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(44_100, 2, settings).unwrap();
        let latency = chain.latency_frames();
        // Block delay plus the arrival inside the response, not the block alone.
        assert!(
            (128..192).contains(&latency),
            "latency {latency} should be the block delay plus a short onset"
        );
        let mut impulse = vec![0.0f32; 512 * 2];
        impulse[0] = 1.0;
        chain.process_interleaved_in_place(&mut impulse).unwrap();
        // The reported figure is where the main arrival lands, which is what a caller
        // aligning an unprocessed reference against this output needs. Energy does
        // start before it: the response has a leading edge ahead of its peak, as any
        // real arrival does, so "first non-zero sample" would be the wrong contract.
        let arrival = (0..384)
            .max_by(|left, right| impulse[left * 2].abs().total_cmp(&impulse[right * 2].abs()))
            .unwrap();
        assert_eq!(
            arrival, latency,
            "main arrival should land at the reported latency"
        );
        let left_energy = impulse
            .iter()
            .step_by(2)
            .map(|sample| sample * sample)
            .sum::<f32>();
        let right_energy = impulse
            .iter()
            .skip(1)
            .step_by(2)
            .map(|sample| sample * sample)
            .sum::<f32>();
        assert!(left_energy > right_energy * 4.0);

        // A centred source meets both paths into one ear at once. That combination is
        // what must stay flat; the individual ears are shaped, and should be.
        //
        // The window opens at the convolver's block delay, not at the reported
        // arrival: the response's leading edge sits ahead of its peak and carries the
        // treble, so starting later would measure a high-frequency loss that is an
        // artefact of the window rather than of the filter.
        const BLOCK_DELAY: usize = 128;
        let centred = (0..320)
            .map(|index| {
                impulse[(BLOCK_DELAY + index) * 2] + impulse[(BLOCK_DELAY + index) * 2 + 1]
            })
            .collect::<Vec<_>>();
        for frequency in [100.0, 250.0, 1_000.0, 4_000.0, 8_000.0] {
            let level = response_db(&centred, frequency, 44_100.0);
            assert!(
                level.abs() <= 4.0,
                "{frequency} Hz sits at {level:.1} dB, outside +-4 dB"
            );
        }

        let peak_of = |channel: usize| {
            (0..256)
                .max_by(|left, right| {
                    impulse[(latency + *left) * 2 + channel]
                        .abs()
                        .total_cmp(&impulse[(latency + *right) * 2 + channel].abs())
                })
                .unwrap()
        };
        assert!(
            peak_of(0) < peak_of(1),
            "near ear should receive the impulse before the far ear"
        );
    }

    #[test]
    fn dual_hrtf_dry_and_wet_stay_aligned_across_mix_values() {
        fn render(mix: f32) -> (Vec<f32>, Vec<f32>) {
            let settings = DspSettings {
                enabled: true,
                hrtf: HrtfSettings {
                    enabled: true,
                    mix,
                    output_gain_db: -6.0,
                },
                ..DspSettings::default()
            };
            let mut chain = DspChain::new(48_000, 2, settings).unwrap();
            let mut processed = vec![0.0f32; 512 * 2];
            processed[0] = 1.0;
            let mut ab_dry = vec![0.0f32; processed.len()];
            chain
                .process_interleaved_with_ab_dry(&mut processed, &mut ab_dry)
                .unwrap();
            (processed, ab_dry)
        }

        let latency = {
            let settings = DspSettings {
                enabled: true,
                hrtf: HrtfSettings {
                    enabled: true,
                    mix: 1.0,
                    output_gain_db: -6.0,
                },
                ..DspSettings::default()
            };
            DspChain::new(48_000, 2, settings).unwrap().latency_frames()
        };

        let (fully_dry, reference_ab_dry) = render(0.0);
        let (fully_wet, wet_ab_dry) = render(1.0);
        let mut expected_delayed_input = vec![0.0f32; reference_ab_dry.len()];
        expected_delayed_input[latency * 2] = 1.0;
        assert_eq!(reference_ab_dry, expected_delayed_input);
        assert_eq!(reference_ab_dry, wet_ab_dry);
        // At mix 0 the head model reduces to a plain delay. It reaches that through
        // the convolver rather than around it, so agreement is to float precision
        // rather than bit-exact; true bypass is the chain's `enabled` flag, which
        // `disabled_chain_is_bitwise_transparent` pins separately.
        for (processed, delayed) in fully_dry.iter().zip(&reference_ab_dry) {
            assert!(
                (processed - delayed).abs() < 1.0e-6,
                "mix 0 should pass the input through unchanged"
            );
        }
        // Silent until the convolver's block delay. Output starts there rather than at
        // the reported arrival, because the response's leading edge precedes its peak.
        assert!(fully_wet[..128 * 2].iter().all(|sample| *sample == 0.0));
        assert!(latency >= 128, "arrival cannot precede the block delay");

        for mix in [0.3, 0.55, 0.72] {
            let (mixed, ab_dry) = render(mix);
            assert_eq!(ab_dry, reference_ab_dry);
            for ((actual, dry), wet) in mixed.iter().zip(&reference_ab_dry).zip(&fully_wet) {
                let expected = dry * (1.0 - mix) + wet * mix;
                assert!((actual - expected).abs() < 1.0e-6);
            }
        }
    }

    #[test]
    fn dual_hrtf_ab_lane_delays_the_untreated_input_not_the_processed_dry() {
        let settings = DspSettings {
            enabled: true,
            eq_enabled: true,
            eq_bands: vec![EqBand::peak(1_000.0, 12.0, 0.7)],
            crossfeed: CrossfeedSettings {
                enabled: true,
                amount: 0.27,
                ..CrossfeedSettings::default()
            },
            hrtf: HrtfSettings {
                enabled: true,
                mix: 0.55,
                ..HrtfSettings::default()
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, settings).unwrap();
        let latency = chain.latency_frames();
        let mut processed = (0..512 * 2)
            .map(|index| (index as f32 * 0.013).sin() * 0.2)
            .collect::<Vec<_>>();
        let original = processed.clone();
        let mut ab_dry = vec![0.0f32; processed.len()];

        chain
            .process_interleaved_with_ab_dry(&mut processed, &mut ab_dry)
            .unwrap();

        // The lane carries the untreated input, delayed to meet the head model and scaled
        // by the level-match factor. Recovering that one factor and finding it holds for
        // every sample is what separates "the input, matched" from "the EQ'd signal".
        let scale = (0..512 - latency)
            .flat_map(|frame| [0, 1].map(move |channel| (frame, channel)))
            .find_map(|(frame, channel)| {
                let input = original[frame * 2 + channel];
                (input.abs() > 0.05).then(|| ab_dry[(frame + latency) * 2 + channel] / input)
            })
            .expect("the probe signal must reach a usable amplitude");
        assert!(scale.is_finite() && scale > 0.0);
        for frame in 0..512 {
            for channel in 0..2 {
                let actual = ab_dry[frame * 2 + channel];
                let expected = if frame < latency {
                    0.0
                } else {
                    original[(frame - latency) * 2 + channel] * scale
                };
                assert!(
                    (actual - expected).abs() < 1.0e-5,
                    "frame {frame} channel {channel}: {actual} vs {expected}"
                );
            }
        }
        assert_ne!(
            processed[latency * 2].to_bits(),
            ab_dry[latency * 2].to_bits()
        );
    }

    #[test]
    fn spatial_processing_is_chunk_invariant() {
        let settings = enabled_spatial_settings();
        let mut whole = DspChain::new(48_000, 2, settings.clone()).unwrap();
        let mut chunked = DspChain::new(48_000, 2, settings).unwrap();
        let mut input = (0..8192)
            .map(|index| (index as f32 * 0.017).sin() * 0.4)
            .collect::<Vec<_>>();
        let mut chunks = input.clone();
        whole.process_interleaved_in_place(&mut input).unwrap();
        for chunk in chunks.chunks_mut(74) {
            chunked.process_interleaved_in_place(chunk).unwrap();
        }
        for (left, right) in input.into_iter().zip(chunks) {
            assert!((left - right).abs() < 1.0e-5);
        }
    }

    #[test]
    fn linked_limiter_respects_ceiling_without_channel_imbalance() {
        let settings = DspSettings {
            enabled: true,
            limiter: LimiterSettings {
                enabled: true,
                ceiling_db: -1.0,
                ..LimiterSettings::default()
            },
            ..DspSettings::default()
        };
        let mut chain = DspChain::new(48_000, 2, settings).unwrap();
        let mut pcm = vec![2.0, -1.0, -2.0, 1.0];
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        let ceiling = 10.0f32.powf(-1.0 / 20.0);
        assert!(pcm.iter().all(|sample| sample.abs() <= ceiling + 1.0e-6));
        assert!((pcm[0].abs() / pcm[1].abs() - 2.0).abs() < 1.0e-5);
    }

    #[test]
    fn full_spatial_chain_allocates_nothing_during_processing() {
        let mut chain = DspChain::new(48_000, 2, enabled_spatial_settings()).unwrap();
        let mut pcm = vec![0.1f32; 4096];
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        assert_eq!(ALLOCATION_COUNT.with(Cell::get), 0);
    }

    #[test]
    fn spatial_chain_supports_common_sample_rates_with_bounded_cpu() {
        for sample_rate in [44_100, 48_000, 96_000] {
            let mut chain = DspChain::new(sample_rate, 2, enabled_spatial_settings()).unwrap();
            let mut pcm = vec![0.05f32; sample_rate as usize * 2];
            let started = Instant::now();
            chain.process_interleaved_in_place(&mut pcm).unwrap();
            assert!(pcm.iter().all(|sample| sample.is_finite()));
            assert!(
                started.elapsed().as_secs_f32() < 5.0,
                "{sample_rate} Hz spatial processing exceeded the debug-build CPU budget"
            );
        }
    }

    #[test]
    fn spatial_settings_reject_non_stereo_and_invalid_ranges() {
        let settings = DspSettings {
            enabled: true,
            hrtf: HrtfSettings {
                enabled: true,
                ..HrtfSettings::default()
            },
            ..DspSettings::default()
        };
        assert!(matches!(
            DspChain::new(48_000, 1, settings),
            Err(DspError::UnsupportedSpatialChannels(1))
        ));
        let settings = DspSettings {
            crossfeed: CrossfeedSettings {
                enabled: true,
                amount: 0.75,
                ..CrossfeedSettings::default()
            },
            ..DspSettings::default()
        };
        assert!(matches!(
            DspChain::new(48_000, 2, settings),
            Err(DspError::InvalidCrossfeedAmount(_))
        ));
    }

    #[test]
    fn set_settings_crossfades_between_generations_without_discontinuity() {
        fn render(chain: &mut DspChain, start_frame: usize, frames: usize, output: &mut Vec<f32>) {
            let mut buffer = Vec::with_capacity(frames * 2);
            for frame in 0..frames {
                let sample =
                    ((start_frame + frame) as f32 * 220.0 * std::f32::consts::TAU / 48_000.0).sin()
                        * 0.5;
                buffer.extend_from_slice(&[sample, sample]);
            }
            chain.process_interleaved_in_place(&mut buffer).unwrap();
            output.extend_from_slice(&buffer);
        }

        // Spatial -> EQ-only is the worst case: the HRTF latency steps from 128 frames to zero
        // and the new equalizer starts from zeroed state.
        let mut chain = DspChain::new(48_000, 2, enabled_spatial_settings()).unwrap();
        let eq_only = DspSettings {
            enabled: true,
            eq_enabled: true,
            eq_bands: vec![EqBand::peak(1_000.0, 6.0, 1.0)],
            ..DspSettings::default()
        };
        let mut output = Vec::new();
        let mut cursor = 0;
        for _ in 0..16 {
            render(&mut chain, cursor, 256, &mut output);
            cursor += 256;
        }
        chain.set_settings(eq_only).unwrap();
        for _ in 0..16 {
            render(&mut chain, cursor, 256, &mut output);
            cursor += 256;
        }

        // Skip the fresh spatial chain's own onset; judge steady state plus the switch window.
        let mut max_step = 0.0f32;
        for frame in 513..(output.len() / 2) {
            for channel in 0..2 {
                let step = (output[frame * 2 + channel] - output[(frame - 1) * 2 + channel]).abs();
                max_step = max_step.max(step);
            }
        }
        assert!(
            max_step < 0.05,
            "settings switch produced a {max_step} adjacent-frame step"
        );
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn set_settings_to_bypass_restores_bitwise_passthrough_after_the_crossfade() {
        let mut chain = DspChain::new(48_000, 2, enabled_spatial_settings()).unwrap();
        let mut warmup = vec![0.25f32; 2048];
        chain.process_interleaved_in_place(&mut warmup).unwrap();

        chain.set_settings(DspSettings::default()).unwrap();
        // Drain the 10 ms (480-frame) crossfade window.
        let mut fade = vec![0.25f32; 480 * 2];
        chain.process_interleaved_in_place(&mut fade).unwrap();
        assert!(fade.iter().all(|sample| sample.is_finite()));

        let mut pcm = vec![
            f32::from_bits(0),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f12_3456),
            f32::from_bits(0x7fc0_1234),
        ];
        let before = pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        assert_eq!(
            pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn bypass_to_bypass_set_settings_keeps_bitwise_transparency() {
        let mut chain = DspChain::new(48_000, 2, DspSettings::default()).unwrap();
        chain
            .set_settings(DspSettings {
                enabled: false,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(4_000.0, -6.0, 2.0)],
                ..DspSettings::default()
            })
            .unwrap();

        let mut pcm = vec![
            f32::from_bits(0),
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x3f12_3456),
            f32::from_bits(0x7fc0_1234),
        ];
        let before = pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        let mut ab_dry = vec![42.0; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();
        assert_eq!(
            pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            before
        );
        assert_eq!(
            ab_dry
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            before
        );
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        assert_eq!(
            pcm.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn ab_dry_lane_stays_untreated_during_a_settings_crossfade() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 6.0, 1.0)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut warmup = vec![0.2f32; 1024];
        chain.process_interleaved_in_place(&mut warmup).unwrap();

        chain
            .set_settings(DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(250.0, -6.0, 2.0)],
                ..DspSettings::default()
            })
            .unwrap();

        let mut pcm = (0..1024)
            .map(|index| (index as f32 * 0.019).sin() * 0.2)
            .collect::<Vec<_>>();
        let original = pcm.clone();
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();

        // Without HRTF the A/B lane is the input scaled by one level-match factor and
        // nothing else: the crossfade must not filter, delay or smear it. Recovering a
        // single constant from every sample is what rules that out.
        let ratios = ab_dry
            .iter()
            .zip(&original)
            .filter(|(_, input)| input.abs() > 1.0e-3)
            .map(|(lane, input)| lane / input)
            .collect::<Vec<_>>();
        assert!(!ratios.is_empty());
        for ratio in &ratios {
            assert!(
                (ratio - ratios[0]).abs() < 1.0e-5,
                "A/B lane is not a plain scaling of the input"
            );
        }
        assert_ne!(pcm, original);
    }

    #[test]
    fn clamped_settings_build_high_frequency_presets_at_low_sample_rates() {
        let settings = DspSettings {
            enabled: true,
            eq_enabled: true,
            eq_bands: vec![
                EqBand::peak(16_000.0, 3.0, 0.7),
                EqBand {
                    enabled: true,
                    kind: FilterKind::HighShelf,
                    frequency_hz: 12_000.0,
                    gain_db: 4.0,
                    q: 0.7,
                },
            ],
            crossfeed: CrossfeedSettings {
                enabled: true,
                cutoff_hz: 12_000.0,
                ..CrossfeedSettings::default()
            },
            ..DspSettings::default()
        };
        // Valid at the 48 kHz persistence-validation rate.
        DspChain::new(48_000, 2, settings.clone()).unwrap();
        // In-range parameters survive clamping untouched.
        assert_eq!(settings.clone().clamped_for_sample_rate(48_000), settings);
        // As-is the preset cannot build on a 22.05 kHz output device…
        assert!(matches!(
            DspChain::new(22_050, 2, settings.clone()),
            Err(DspError::InvalidFrequency { .. })
        ));
        // …but the clamped variant builds and stays finite.
        let mut chain = DspChain::new(22_050, 2, settings.clamped_for_sample_rate(22_050)).unwrap();
        let mut pcm = vec![0.1f32; 2048];
        chain.process_interleaved_in_place(&mut pcm).unwrap();
        assert!(pcm.iter().all(|sample| sample.is_finite()));
    }

    /// A probe built at the probe points the estimator assumes, with each partial's
    /// amplitude set so its share of the total power is that point's weight. This is the
    /// spectrum the matcher is designed for, so what the test measures is whether the
    /// matcher gets its own case right rather than how the two spectra differ.
    fn broadband_probe(frames: usize) -> Vec<f32> {
        let partials = AB_MATCH_PROBES
            .iter()
            .map(|&(frequency, weight)| (frequency, weight.sqrt()))
            .collect::<Vec<_>>();
        // Scaled to leave headroom for a +6 dB match on top of a +12 dB band.
        let normalisation = 0.2 / partials.iter().map(|(_, amplitude)| amplitude).sum::<f32>();
        (0..frames * 2)
            .map(|index| {
                let frame = (index / 2) as f32;
                partials
                    .iter()
                    .map(|&(frequency, amplitude)| {
                        amplitude * (frame * frequency * TAU_OVER_48K).sin()
                    })
                    .sum::<f32>()
                    * normalisation
            })
            .collect()
    }

    const TAU_OVER_48K: f32 = std::f32::consts::TAU / 48_000.0;

    fn level_matched_lanes(bands: Vec<EqBand>) -> (f32, f32) {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: bands,
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = broadband_probe(48_000);
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();
        (rms(&pcm), rms(&ab_dry))
    }

    #[test]
    fn ab_reference_lane_is_level_matched_to_the_processed_lane() {
        // Lifts and cuts alike leave the two lanes level, so pressing the A/B button is a
        // tone comparison rather than a loudness one. The unmatched figure is asserted
        // alongside it: without that, a matcher that returned unity would also pass.
        for bands in [
            vec![EqBand::peak(1_000.0, 8.0, 0.7)],
            vec![EqBand::peak(1_000.0, -8.0, 0.7)],
            vec![
                EqBand {
                    enabled: true,
                    kind: FilterKind::LowShelf,
                    frequency_hz: 200.0,
                    gain_db: 6.0,
                    q: 0.7,
                },
                EqBand::peak(3_000.0, 4.0, 1.0),
            ],
        ] {
            let gain = ab_dry_match_gain(48_000, &bands);
            let (processed, reference) = level_matched_lanes(bands);
            let matched_db = 20.0 * (processed / reference).log10();
            let unmatched_db = 20.0 * (processed / (reference / gain)).log10();
            assert!(
                matched_db.abs() <= 0.3,
                "lanes still differ by {matched_db:.2} dB after matching"
            );
            assert!(
                unmatched_db.abs() >= 1.5,
                "the unmatched gap is only {unmatched_db:.2} dB, so this case proves nothing"
            );
        }
    }

    #[test]
    fn matching_still_shrinks_the_gap_on_a_spectrum_it_did_not_assume() {
        // Real music is not the assumed weighting. The estimate is a model, so the claim
        // it has to earn is "much closer", not "exact": equal-amplitude partials at
        // different frequencies from the probe points are the awkward case for it.
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 8.0, 0.7)],
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = (0..48_000 * 2)
            .map(|index| {
                let frame = (index / 2) as f32;
                let tone = |hz: f32| (frame * hz * TAU_OVER_48K).sin();
                (tone(70.0) + tone(220.0) + tone(700.0) + tone(2_200.0) + tone(7_000.0)) * 0.12
            })
            .collect::<Vec<_>>();
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();

        let gain = ab_dry_match_gain(48_000, &[EqBand::peak(1_000.0, 8.0, 0.7)]);
        let matched_db = 20.0 * (rms(&pcm) / rms(&ab_dry)).log10();
        let unmatched_db = 20.0 * (rms(&pcm) / (rms(&ab_dry) / gain)).log10();
        assert!(
            matched_db.abs() < unmatched_db.abs() * 0.6,
            "matching left {matched_db:.2} dB of a {unmatched_db:.2} dB gap"
        );
        // Still inside the range where level stops driving the verdict.
        assert!(matched_db.abs() <= 1.5, "residual {matched_db:.2} dB");
    }

    #[test]
    fn a_flat_eq_leaves_the_reference_lane_bit_exact() {
        // Nothing to match means nothing to touch: presets like 耳机日常 carry a flat
        // curve, and the reference must stay the caller's samples rather than the
        // caller's samples times 0.999.
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 0.0, 1.0)],
                crossfeed: CrossfeedSettings {
                    enabled: true,
                    ..CrossfeedSettings::default()
                },
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = broadband_probe(512);
        let original = pcm.clone();
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();
        assert_eq!(
            ab_dry
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            original
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn level_matching_is_chunk_invariant_and_bounded() {
        // One factor fixed by the settings, so splitting the stream cannot move it. An
        // adaptive matcher would drift here, and drift is what pumps.
        let settings = DspSettings {
            enabled: true,
            eq_enabled: true,
            eq_bands: vec![EqBand::peak(1_000.0, 9.0, 0.7)],
            ..DspSettings::default()
        };
        let mut whole = DspChain::new(48_000, 2, settings.clone()).unwrap();
        let mut chunked = DspChain::new(48_000, 2, settings).unwrap();

        let input = broadband_probe(4_096);
        let mut whole_pcm = input.clone();
        let mut whole_dry = vec![0.0f32; input.len()];
        whole
            .process_interleaved_with_ab_dry(&mut whole_pcm, &mut whole_dry)
            .unwrap();

        let mut chunked_dry = Vec::with_capacity(input.len());
        for chunk in input.chunks(256 * 2) {
            let mut pcm = chunk.to_vec();
            let mut dry = vec![0.0f32; pcm.len()];
            chunked
                .process_interleaved_with_ab_dry(&mut pcm, &mut dry)
                .unwrap();
            chunked_dry.extend_from_slice(&dry);
        }

        assert_eq!(
            whole_dry
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            chunked_dry
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
        // Headroom: a matched reference must not be the one that clips.
        assert!(whole_dry.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn a_runaway_boost_cannot_make_the_reference_the_hot_lane() {
        // Every band pinned to +12 dB is a curve no preset ships, but the editor can
        // reach it. The reference tracks it only as far as the cap allows.
        let bands = DSP_PROBE_FREQUENCIES
            .iter()
            .map(|&frequency| EqBand::peak(frequency, 12.0, 0.7))
            .collect::<Vec<_>>();
        let gain = ab_dry_match_gain(48_000, &bands);
        assert!(
            gain <= AB_MATCH_MAX_GAIN + 1.0e-4,
            "cap let the reference lane reach {gain}x"
        );
    }

    #[test]
    fn a_full_depth_cut_is_matched_rather_than_left_on_the_clamp() {
        // The case a symmetric +-6 dB clamp got wrong. Ten bands at the editor's floor
        // stack into far more than 6 dB of broadband cut, and stopping the match there
        // would leave the rest audible as exactly the level difference A/B removes.
        let bands = DSP_PROBE_FREQUENCIES
            .iter()
            .map(|&frequency| EqBand::peak(frequency, -12.0, 1.0))
            .collect::<Vec<_>>();
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: bands,
                ..DspSettings::default()
            },
        )
        .unwrap();
        let mut pcm = broadband_probe(48_000);
        let input_rms = rms(&pcm);
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();

        // The curve really does cut this hard: unmatched, the lanes would sit that far
        // apart, which is what makes the residual below worth asserting.
        let eq_db = 20.0 * (rms(&pcm) / input_rms).log10();
        assert!(eq_db < -12.0, "curve only cut {eq_db:.2} dB");
        let residual_db = 20.0 * (rms(&pcm) / rms(&ab_dry)).log10();
        assert!(
            residual_db.abs() <= 1.5,
            "{residual_db:.2} dB left between the lanes"
        );
    }

    #[test]
    fn the_matched_reference_lane_respects_the_limiter_ceiling() {
        // A hot source plus a boosting curve is where a post-limiter match would clip. The
        // match runs ahead of the limiter and the limiter holds the reference to the same
        // ceiling on its own peak, so the lane the listener switches to cannot overshoot.
        let ceiling_db = -1.0;
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 10.0, 0.7)],
                limiter: LimiterSettings {
                    enabled: true,
                    ceiling_db,
                    release_ms: 80.0,
                },
                ..DspSettings::default()
            },
        )
        .unwrap();
        // Normalised to just under full scale before anything is applied.
        let raw = broadband_probe(48_000);
        let raw_peak = raw
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        let mut pcm = raw
            .into_iter()
            .map(|sample| sample * 0.98 / raw_peak)
            .collect::<Vec<_>>();
        assert!(pcm.iter().any(|sample| sample.abs() > 0.9));
        let mut ab_dry = vec![0.0f32; pcm.len()];
        chain
            .process_interleaved_with_ab_dry(&mut pcm, &mut ab_dry)
            .unwrap();

        let ceiling = 10.0f32.powf(ceiling_db / 20.0);
        let reference_peak = ab_dry
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            reference_peak <= ceiling + 1.0e-4,
            "reference lane peaked at {reference_peak}, over the {ceiling} ceiling"
        );
    }

    #[test]
    fn the_shipped_curves_need_only_a_gentle_match() {
        // The product's voicing curves, 31 Hz -> 16 kHz, as the frontend authors them.
        // They are deliberately restrained, so the reference lane should need a nudge
        // rather than a shove: a curve that demanded several dB here would be one worth
        // re-examining rather than compensating for.
        let curves: [(&str, [f32; 10]); 8] = [
            (
                "warm",
                [0.0, 0.5, 1.25, 0.75, 0.0, 0.0, -0.25, -0.5, -0.75, -0.5],
            ),
            (
                "bright",
                [0.0, 0.0, 0.0, -0.25, 0.0, 0.25, 0.75, 1.25, 1.5, 1.0],
            ),
            (
                "classical",
                [0.5, 0.5, 0.0, -0.5, -0.25, 0.0, 0.5, 0.75, 0.75, 0.5],
            ),
            (
                "electronic",
                [2.5, 2.0, 1.0, -0.5, -1.0, -0.5, 0.0, 1.0, 1.75, 1.25],
            ),
            (
                "rock",
                [1.5, 1.75, 1.0, -1.0, -0.5, 0.5, 1.5, 1.75, 1.0, 0.25],
            ),
            (
                "podcast",
                [-3.5, -3.0, -1.5, 0.0, 1.0, 2.0, 2.25, 1.25, 0.0, -0.5],
            ),
            (
                "jazz",
                [0.5, 0.75, 0.75, 0.25, -0.25, 0.25, 0.5, 0.75, 1.0, 0.75],
            ),
            (
                "piano_vocal",
                [0.0, 0.0, -2.5, -1.5, 0.0, 0.0, 0.0, 0.0, -0.5, 0.0],
            ),
        ];
        for (name, gains) in curves {
            // 1.4x is the ceiling the intensity slider applies at full strength.
            let bands = DSP_PROBE_FREQUENCIES
                .iter()
                .zip(gains)
                .filter(|(_, gain)| *gain != 0.0)
                .map(|(&frequency, gain)| EqBand::peak(frequency, gain * 1.4, 1.0))
                .collect::<Vec<_>>();
            let match_db = 20.0 * ab_dry_match_gain(48_000, &bands).log10();
            assert!(
                match_db.abs() <= 3.0,
                "{name} asks the reference lane for {match_db:.2} dB"
            );
        }
    }

    #[test]
    fn bands_the_device_cannot_build_are_left_out_of_the_estimate() {
        // A 16 kHz band cannot exist on a 32 kHz device. It must not be counted as a
        // level change there, or the reference lane would compensate for a filter that
        // is not running.
        let bands = vec![EqBand::peak(16_000.0, 12.0, 0.7)];
        assert!((ab_dry_match_gain(32_000, &bands) - 1.0).abs() < 1.0e-6);
        assert!(ab_dry_match_gain(48_000, &bands) > 1.0);
    }

    const DSP_PROBE_FREQUENCIES: [f32; 10] = [
        31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
    ];

    fn enabled_spatial_settings() -> DspSettings {
        DspSettings {
            enabled: true,
            crossfeed: CrossfeedSettings {
                enabled: true,
                ..CrossfeedSettings::default()
            },
            hrtf: HrtfSettings {
                enabled: true,
                ..HrtfSettings::default()
            },
            limiter: LimiterSettings {
                enabled: true,
                ..LimiterSettings::default()
            },
            ..DspSettings::default()
        }
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn response_db(impulse: &[f32], frequency: f32, sample_rate: f32) -> f32 {
        let (real, imaginary) = impulse.iter().enumerate().fold(
            (0.0f32, 0.0f32),
            |(real, imaginary), (index, sample)| {
                let phase = -std::f32::consts::TAU * frequency * index as f32 / sample_rate;
                (
                    real + sample * phase.cos(),
                    imaginary + sample * phase.sin(),
                )
            },
        );
        20.0 * real.hypot(imaginary).log10()
    }
}
