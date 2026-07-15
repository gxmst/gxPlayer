//! Allocation-free-in-process DSP building blocks for GXPlayer.
//!
//! Configuration may allocate on the decode/DSP worker. Processing mutates an existing PCM slice
//! and performs no allocation. A disabled chain returns before reading or writing any sample.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod kemar;
mod spatial;

use spatial::{CrossfeedProcessor, LinkedLimiter, StereoHrtf};
pub use spatial::{CrossfeedSettings, HrtfSettings, LimiterSettings};

type ProcessorSet = (
    ParametricEq,
    Option<CrossfeedProcessor>,
    Option<StereoHrtf>,
    Option<LinkedLimiter>,
);

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
            hrtf: HrtfSettings::default(),
            limiter: LimiterSettings::default(),
        }
    }
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
    #[error("limiter ceiling {0} dB must be in the range -12..=0")]
    InvalidLimiterCeiling(f32),
    #[error("limiter release {0} ms must be in the range 10..=1000")]
    InvalidLimiterRelease(f32),
    #[error("Crossfeed and stereo HRTF require exactly two channels, got {0}")]
    UnsupportedSpatialChannels(usize),
}

pub struct DspChain {
    sample_rate: u32,
    channels: usize,
    settings: DspSettings,
    equalizer: ParametricEq,
    crossfeed: Option<CrossfeedProcessor>,
    hrtf: Option<StereoHrtf>,
    limiter: Option<LinkedLimiter>,
}

impl DspChain {
    pub fn new(sample_rate: u32, channels: usize, settings: DspSettings) -> Result<Self, DspError> {
        if sample_rate == 0 {
            return Err(DspError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(DspError::InvalidChannels);
        }
        let (equalizer, crossfeed, hrtf, limiter) =
            build_processors(sample_rate, channels, &settings)?;
        Ok(Self {
            sample_rate,
            channels,
            settings,
            equalizer,
            crossfeed,
            hrtf,
            limiter,
        })
    }

    pub fn settings(&self) -> &DspSettings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: DspSettings) -> Result<(), DspError> {
        let (equalizer, crossfeed, hrtf, limiter) =
            build_processors(self.sample_rate, self.channels, &settings)?;
        self.equalizer = equalizer;
        self.crossfeed = crossfeed;
        self.hrtf = hrtf;
        self.limiter = limiter;
        self.settings = settings;
        Ok(())
    }

    pub fn latency_frames(&self) -> usize {
        if self.settings.enabled && self.settings.hrtf.enabled {
            128
        } else {
            0
        }
    }

    pub fn process_interleaved_in_place(&mut self, pcm: &mut [f32]) -> Result<(), DspError> {
        if !self.settings.enabled {
            return Ok(());
        }
        if !pcm.len().is_multiple_of(self.channels) {
            return Err(DspError::MisalignedPcm {
                samples: pcm.len(),
                channels: self.channels,
            });
        }
        if self.settings.eq_enabled {
            self.equalizer.process_interleaved_in_place(pcm);
        }
        if let Some(crossfeed) = &mut self.crossfeed {
            crossfeed.process(pcm);
        }
        if let Some(hrtf) = &mut self.hrtf {
            hrtf.process(pcm);
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.process(pcm, self.channels);
        }
        Ok(())
    }

    /// Processes `pcm` through the configured chain while writing an untreated A/B lane into the
    /// caller-provided `ab_dry` buffer.
    ///
    /// The A/B lane starts as an exact copy of the input. EQ and Crossfeed affect only `pcm`. When
    /// HRTF is enabled, both lanes use the same fixed 128-frame dry queue so the untreated lane is
    /// aligned with the processed HRTF output. The limiter derives one linked gain from `pcm` and
    /// applies that same gain to both lanes. Processing performs no heap allocation.
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

        ab_dry.copy_from_slice(pcm);
        if !self.settings.enabled {
            return Ok(());
        }
        if self.settings.eq_enabled {
            self.equalizer.process_interleaved_in_place(pcm);
        }
        if let Some(crossfeed) = &mut self.crossfeed {
            crossfeed.process(pcm);
        }
        if let Some(hrtf) = &mut self.hrtf {
            hrtf.process_with_ab_dry(pcm, ab_dry);
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.process_with_ab_dry(pcm, ab_dry, self.channels);
        }
        Ok(())
    }
}

fn build_processors(
    sample_rate: u32,
    channels: usize,
    settings: &DspSettings,
) -> Result<ProcessorSet, DspError> {
    if (settings.crossfeed.enabled || settings.hrtf.enabled) && channels != 2 {
        return Err(DspError::UnsupportedSpatialChannels(channels));
    }
    let equalizer = ParametricEq::new(sample_rate, channels, &settings.eq_bands)?;
    let crossfeed = settings
        .crossfeed
        .enabled
        .then(|| CrossfeedProcessor::new(sample_rate, &settings.crossfeed))
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
    Ok((equalizer, crossfeed, hrtf, limiter))
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
        let nyquist_guard = sample_rate as f32 * 0.5 * 0.999;
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
        assert_eq!(chain.latency_frames(), 128);
        let mut impulse = vec![0.0f32; 512 * 2];
        impulse[0] = 1.0;
        chain.process_interleaved_in_place(&mut impulse).unwrap();
        let first = 128 * 2;
        assert!((impulse[first] - kemar::NEAR_EAR_30[0] as f32 / 32768.0).abs() < 1.0e-5);
        assert!((impulse[first + 1] - kemar::FAR_EAR_30[0] as f32 / 32768.0).abs() < 1.0e-5);
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
        let near_impulse = (0..128)
            .map(|index| impulse[(128 + index) * 2])
            .collect::<Vec<_>>();
        for (frequency, expected_db) in [
            (250.0, -4.316_937),
            (1_000.0, -1.542_039),
            (8_000.0, -7.298_68),
        ] {
            let measured_db = response_db(&near_impulse, frequency, 44_100.0);
            assert!((measured_db - expected_db).abs() < 0.02);
        }
        let near_peak = near_impulse
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))
            .unwrap()
            .0;
        let far_peak = (0..128)
            .max_by(|left, right| {
                impulse[(128 + *left) * 2 + 1]
                    .abs()
                    .total_cmp(&impulse[(128 + *right) * 2 + 1].abs())
            })
            .unwrap();
        assert!(
            near_peak < far_peak,
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

        let (fully_dry, reference_ab_dry) = render(0.0);
        let (fully_wet, wet_ab_dry) = render(1.0);
        let mut expected_delayed_input = vec![0.0f32; reference_ab_dry.len()];
        expected_delayed_input[128 * 2] = 1.0;
        assert_eq!(reference_ab_dry, expected_delayed_input);
        assert_eq!(reference_ab_dry, wet_ab_dry);
        assert_eq!(fully_dry, reference_ab_dry);
        assert!(fully_wet[..128 * 2].iter().all(|sample| *sample == 0.0));

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
        let mut processed = (0..512 * 2)
            .map(|index| (index as f32 * 0.013).sin() * 0.2)
            .collect::<Vec<_>>();
        let original = processed.clone();
        let mut ab_dry = vec![0.0f32; processed.len()];

        chain
            .process_interleaved_with_ab_dry(&mut processed, &mut ab_dry)
            .unwrap();

        for frame in 0..512 {
            for channel in 0..2 {
                let actual = ab_dry[frame * 2 + channel].to_bits();
                let expected = if frame < 128 {
                    0.0f32.to_bits()
                } else {
                    original[(frame - 128) * 2 + channel].to_bits()
                };
                assert_eq!(actual, expected);
            }
        }
        assert_ne!(processed[128 * 2].to_bits(), ab_dry[128 * 2].to_bits());
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
