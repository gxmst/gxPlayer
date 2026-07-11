use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::{DspError, kemar};

const PARTITION_SIZE: usize = 128;
const FFT_SIZE: usize = PARTITION_SIZE * 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossfeedSettings {
    pub enabled: bool,
    pub amount: f32,
    pub delay_ms: f32,
    pub cutoff_hz: f32,
}

impl Default for CrossfeedSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 0.18,
            delay_ms: 0.28,
            cutoff_hz: 700.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HrtfSettings {
    pub enabled: bool,
    pub mix: f32,
    pub output_gain_db: f32,
}

impl Default for HrtfSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mix: 0.72,
            output_gain_db: -6.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimiterSettings {
    pub enabled: bool,
    pub ceiling_db: f32,
    pub release_ms: f32,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ceiling_db: -1.0,
            release_ms: 80.0,
        }
    }
}

pub(crate) struct CrossfeedProcessor {
    amount: f32,
    direct_gain: f32,
    lowpass_alpha: f32,
    left_lowpass: f32,
    right_lowpass: f32,
    left_delay: Vec<f32>,
    right_delay: Vec<f32>,
    delay_index: usize,
}

impl CrossfeedProcessor {
    pub(crate) fn new(sample_rate: u32, settings: &CrossfeedSettings) -> Result<Self, DspError> {
        validate_crossfeed(sample_rate, settings)?;
        let delay_samples =
            ((settings.delay_ms * sample_rate as f32 / 1000.0).round() as usize).max(1);
        Ok(Self {
            amount: settings.amount,
            direct_gain: 1.0 - settings.amount * 0.5,
            lowpass_alpha: 1.0 - (-2.0 * PI * settings.cutoff_hz / sample_rate as f32).exp(),
            left_lowpass: 0.0,
            right_lowpass: 0.0,
            left_delay: vec![0.0; delay_samples],
            right_delay: vec![0.0; delay_samples],
            delay_index: 0,
        })
    }

    pub(crate) fn process(&mut self, pcm: &mut [f32]) {
        for frame in pcm.chunks_exact_mut(2) {
            let left = frame[0];
            let right = frame[1];
            let delayed_left = self.left_delay[self.delay_index];
            let delayed_right = self.right_delay[self.delay_index];
            self.left_delay[self.delay_index] = left;
            self.right_delay[self.delay_index] = right;
            self.delay_index += 1;
            if self.delay_index == self.left_delay.len() {
                self.delay_index = 0;
            }
            self.left_lowpass += self.lowpass_alpha * (delayed_left - self.left_lowpass);
            self.right_lowpass += self.lowpass_alpha * (delayed_right - self.right_lowpass);
            frame[0] = left * self.direct_gain + self.right_lowpass * self.amount;
            frame[1] = right * self.direct_gain + self.left_lowpass * self.amount;
        }
    }
}

pub(crate) struct StereoHrtf {
    left_to_left: PartitionedConvolver,
    left_to_right: PartitionedConvolver,
    right_to_left: PartitionedConvolver,
    right_to_right: PartitionedConvolver,
    dry_left: VecDeque<f32>,
    dry_right: VecDeque<f32>,
    mix: f32,
    wet_gain: f32,
}

impl StereoHrtf {
    pub(crate) fn new(sample_rate: u32, settings: &HrtfSettings) -> Result<Self, DspError> {
        validate_hrtf(settings)?;
        let far = resample_hrir(&kemar::FAR_EAR_30, sample_rate);
        let near = resample_hrir(&kemar::NEAR_EAR_30, sample_rate);
        let mut dry_left = VecDeque::with_capacity(PARTITION_SIZE * 2);
        let mut dry_right = VecDeque::with_capacity(PARTITION_SIZE * 2);
        dry_left.resize(PARTITION_SIZE, 0.0);
        dry_right.resize(PARTITION_SIZE, 0.0);
        Ok(Self {
            // Left virtual speaker at -30° is the mirror of the measured +30° response.
            left_to_left: PartitionedConvolver::new(&near),
            left_to_right: PartitionedConvolver::new(&far),
            right_to_left: PartitionedConvolver::new(&far),
            right_to_right: PartitionedConvolver::new(&near),
            dry_left,
            dry_right,
            mix: settings.mix,
            wet_gain: 10.0f32.powf(settings.output_gain_db / 20.0),
        })
    }

    pub(crate) fn process(&mut self, pcm: &mut [f32]) {
        for frame in pcm.chunks_exact_mut(2) {
            let left = frame[0];
            let right = frame[1];
            let wet_left =
                self.left_to_left.process_sample(left) + self.right_to_left.process_sample(right);
            let wet_right =
                self.left_to_right.process_sample(left) + self.right_to_right.process_sample(right);
            self.dry_left.push_back(left);
            self.dry_right.push_back(right);
            let dry_left = self.dry_left.pop_front().unwrap_or(0.0);
            let dry_right = self.dry_right.pop_front().unwrap_or(0.0);
            frame[0] = dry_left * (1.0 - self.mix) + wet_left * self.mix * self.wet_gain;
            frame[1] = dry_right * (1.0 - self.mix) + wet_right * self.mix * self.wet_gain;
        }
    }
}

pub(crate) struct LinkedLimiter {
    ceiling: f32,
    release_coefficient: f32,
    gain: f32,
}

impl LinkedLimiter {
    pub(crate) fn new(sample_rate: u32, settings: &LimiterSettings) -> Result<Self, DspError> {
        validate_limiter(settings)?;
        Ok(Self {
            ceiling: 10.0f32.powf(settings.ceiling_db / 20.0),
            release_coefficient: (-1.0 / (sample_rate as f32 * settings.release_ms / 1000.0)).exp(),
            gain: 1.0,
        })
    }

    pub(crate) fn process(&mut self, pcm: &mut [f32], channels: usize) {
        for frame in pcm.chunks_exact_mut(channels) {
            let peak = frame
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            let target = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            if target <= self.gain {
                self.gain = target;
            } else {
                self.gain = 1.0 - (1.0 - self.gain) * self.release_coefficient;
            }
            for sample in frame {
                *sample *= self.gain;
            }
        }
    }
}

struct PartitionedConvolver {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    impulse_spectra: Vec<Vec<Complex32>>,
    history: Vec<Vec<Complex32>>,
    history_pos: usize,
    input: Vec<f32>,
    input_fill: usize,
    fft_buffer: Vec<Complex32>,
    accumulator: Vec<Complex32>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
    overlap: Vec<f32>,
    output: VecDeque<f32>,
}

impl PartitionedConvolver {
    fn new(impulse: &[f32]) -> Self {
        let partitions = impulse.len().div_ceil(PARTITION_SIZE).max(1);
        let mut planner = FftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(FFT_SIZE);
        let inverse = planner.plan_fft_inverse(FFT_SIZE);
        let mut impulse_spectra = Vec::with_capacity(partitions);
        let mut scratch = vec![Complex32::default(); forward.get_inplace_scratch_len()];
        for partition in 0..partitions {
            let mut spectrum = vec![Complex32::default(); FFT_SIZE];
            let start = partition * PARTITION_SIZE;
            let end = (start + PARTITION_SIZE).min(impulse.len());
            for (target, sample) in spectrum.iter_mut().zip(&impulse[start..end]) {
                target.re = *sample;
            }
            forward.process_with_scratch(&mut spectrum, &mut scratch);
            impulse_spectra.push(spectrum);
        }
        let mut output = VecDeque::with_capacity(PARTITION_SIZE * 2);
        output.resize(PARTITION_SIZE, 0.0);
        Self {
            forward_scratch: vec![Complex32::default(); forward.get_inplace_scratch_len()],
            inverse_scratch: vec![Complex32::default(); inverse.get_inplace_scratch_len()],
            forward,
            inverse,
            impulse_spectra,
            history: vec![vec![Complex32::default(); FFT_SIZE]; partitions],
            history_pos: 0,
            input: vec![0.0; PARTITION_SIZE],
            input_fill: 0,
            fft_buffer: vec![Complex32::default(); FFT_SIZE],
            accumulator: vec![Complex32::default(); FFT_SIZE],
            overlap: vec![0.0; PARTITION_SIZE],
            output,
        }
    }

    #[inline]
    fn process_sample(&mut self, sample: f32) -> f32 {
        self.input[self.input_fill] = sample;
        self.input_fill += 1;
        if self.input_fill == PARTITION_SIZE {
            self.process_block();
            self.input_fill = 0;
        }
        self.output.pop_front().unwrap_or(0.0)
    }

    fn process_block(&mut self) {
        self.fft_buffer.fill(Complex32::default());
        for (target, sample) in self.fft_buffer.iter_mut().zip(&self.input) {
            target.re = *sample;
        }
        self.forward
            .process_with_scratch(&mut self.fft_buffer, &mut self.forward_scratch);
        self.history[self.history_pos].copy_from_slice(&self.fft_buffer);
        self.accumulator.fill(Complex32::default());
        let partitions = self.impulse_spectra.len();
        for partition in 0..partitions {
            let history_index = (self.history_pos + partitions - partition) % partitions;
            for index in 0..FFT_SIZE {
                self.accumulator[index] +=
                    self.history[history_index][index] * self.impulse_spectra[partition][index];
            }
        }
        self.inverse
            .process_with_scratch(&mut self.accumulator, &mut self.inverse_scratch);
        let scale = 1.0 / FFT_SIZE as f32;
        for index in 0..PARTITION_SIZE {
            self.output
                .push_back(self.accumulator[index].re * scale + self.overlap[index]);
            self.overlap[index] = self.accumulator[index + PARTITION_SIZE].re * scale;
        }
        self.history_pos = (self.history_pos + 1) % partitions;
    }
}

fn resample_hrir(source: &[i16], target_sample_rate: u32) -> Vec<f32> {
    let target_len = ((source.len() as u64 * target_sample_rate as u64
        + kemar::SAMPLE_RATE as u64 / 2)
        / kemar::SAMPLE_RATE as u64) as usize;
    let mut output = Vec::with_capacity(target_len.max(1));
    for index in 0..target_len.max(1) {
        let position = index as f64 * kemar::SAMPLE_RATE as f64 / target_sample_rate as f64;
        let lower = position.floor() as usize;
        let fraction = (position - lower as f64) as f32;
        let a = source[lower.min(source.len() - 1)] as f32 / 32768.0;
        let b = source[(lower + 1).min(source.len() - 1)] as f32 / 32768.0;
        output.push(a + (b - a) * fraction);
    }
    output
}

fn validate_crossfeed(sample_rate: u32, settings: &CrossfeedSettings) -> Result<(), DspError> {
    if !settings.amount.is_finite() || !(0.0..=0.5).contains(&settings.amount) {
        return Err(DspError::InvalidCrossfeedAmount(settings.amount));
    }
    if !settings.delay_ms.is_finite() || !(0.05..=1.0).contains(&settings.delay_ms) {
        return Err(DspError::InvalidCrossfeedDelay(settings.delay_ms));
    }
    let max_cutoff = sample_rate as f32 * 0.45;
    if !settings.cutoff_hz.is_finite()
        || settings.cutoff_hz < 100.0
        || settings.cutoff_hz > max_cutoff
    {
        return Err(DspError::InvalidCrossfeedCutoff(settings.cutoff_hz));
    }
    Ok(())
}

fn validate_hrtf(settings: &HrtfSettings) -> Result<(), DspError> {
    if !settings.mix.is_finite() || !(0.0..=1.0).contains(&settings.mix) {
        return Err(DspError::InvalidHrtfMix(settings.mix));
    }
    if !settings.output_gain_db.is_finite() || !(-24.0..=6.0).contains(&settings.output_gain_db) {
        return Err(DspError::InvalidHrtfGain(settings.output_gain_db));
    }
    Ok(())
}

fn validate_limiter(settings: &LimiterSettings) -> Result<(), DspError> {
    if !settings.ceiling_db.is_finite() || !(-12.0..=0.0).contains(&settings.ceiling_db) {
        return Err(DspError::InvalidLimiterCeiling(settings.ceiling_db));
    }
    if !settings.release_ms.is_finite() || !(10.0..=1000.0).contains(&settings.release_ms) {
        return Err(DspError::InvalidLimiterRelease(settings.release_ms));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitioned_convolver_matches_direct_impulse_after_fixed_latency() {
        let impulse = vec![1.0, 0.5, -0.25, 0.125];
        let mut convolver = PartitionedConvolver::new(&impulse);
        let mut output = Vec::new();
        for index in 0..(PARTITION_SIZE * 3) {
            output.push(convolver.process_sample(if index == 0 { 1.0 } else { 0.0 }));
        }
        for (index, expected) in impulse.into_iter().enumerate() {
            assert!((output[PARTITION_SIZE + index] - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    fn resampled_hrir_keeps_expected_length_and_finite_values() {
        let hrir = resample_hrir(&kemar::NEAR_EAR_30, 48_000);
        assert_eq!(hrir.len(), 139);
        assert!(hrir.iter().all(|sample| sample.is_finite()));
    }
}
