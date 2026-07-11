//! Allocation-free-in-process DSP building blocks for GXPlayer.
//!
//! Configuration may allocate on the decode/DSP worker. Processing mutates an existing PCM slice
//! and performs no allocation. A disabled chain returns before reading or writing any sample.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}

impl Default for DspSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            eq_enabled: false,
            eq_bands: vec![EqBand::peak(1_000.0, 0.0, 1.0)],
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
    #[error("EQ frequency {frequency_hz} Hz must be between 5 Hz and {max_hz} Hz")]
    InvalidFrequency { frequency_hz: f32, max_hz: f32 },
    #[error("EQ Q {0} must be in the range 0.05..=30")]
    InvalidQ(f32),
    #[error("EQ gain {0} dB must be in the range -30..=30")]
    InvalidGain(f32),
}

pub struct DspChain {
    sample_rate: u32,
    channels: usize,
    settings: DspSettings,
    equalizer: ParametricEq,
}

impl DspChain {
    pub fn new(sample_rate: u32, channels: usize, settings: DspSettings) -> Result<Self, DspError> {
        if sample_rate == 0 {
            return Err(DspError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(DspError::InvalidChannels);
        }
        let equalizer = ParametricEq::new(sample_rate, channels, &settings.eq_bands)?;
        Ok(Self {
            sample_rate,
            channels,
            settings,
            equalizer,
        })
    }

    pub fn settings(&self) -> &DspSettings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: DspSettings) -> Result<(), DspError> {
        let equalizer = ParametricEq::new(self.sample_rate, self.channels, &settings.eq_bands)?;
        self.equalizer = equalizer;
        self.settings = settings;
        Ok(())
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
        Ok(())
    }
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
    fn eq_disabled_is_bitwise_transparent() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: false,
                eq_bands: vec![EqBand::peak(1_000.0, 12.0, 0.7)],
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
    fn peak_filter_reaches_requested_center_gain() {
        let mut chain = DspChain::new(
            48_000,
            2,
            DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: vec![EqBand::peak(1_000.0, 6.0, 1.0)],
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
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }
}
