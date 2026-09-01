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

/// Early reflections, the cue that separates "outside my head" from "in a room".
///
/// Pure HRTF convolution places a source outside the head but in an anechoic
/// space, which reads as dry and unnatural: the ear judges distance largely from
/// the ratio of direct sound to its first reflections. A handful of delayed,
/// attenuated, damped taps supplies that ratio far more cheaply than a full
/// reverb, and because this runs ahead of the HRTF each reflection is spatialised
/// by the same head model as the direct sound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSettings {
    pub enabled: bool,
    /// Reflection level relative to direct sound. 0 is anechoic.
    pub amount: f32,
    /// Scales the tap delays: small values read as a booth, large as a hall.
    pub size: f32,
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 0.22,
            size: 0.45,
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
            // Unity. The equalised pair already has flat broadband gain, so there is
            // no loss left to compensate; the headroom trim this used to carry only
            // made the preset quieter than bypass and invited turning it up.
            output_gain_db: 0.0,
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

/// Tap layout in milliseconds at `size` = 1, with its gain and which ear leads.
///
/// The two sides are deliberately unequal. A symmetric set of reflections is not
/// something a real room produces, and identical delays on both ears comb-filter
/// into an audible metallic ring.
const REFLECTION_TAPS: [(f32, f32, bool); 6] = [
    (11.0, 0.70, true),
    (14.5, 0.62, false),
    (19.0, 0.50, false),
    (23.5, 0.42, true),
    (29.0, 0.32, true),
    (37.0, 0.24, false),
];

/// Shortest delay any tap may collapse to. Below roughly 8 ms a reflection stops
/// reading as a room and starts colouring the direct sound.
const MIN_REFLECTION_MS: f32 = 8.0;

pub(crate) struct EarlyReflections {
    /// Interleaved stereo history, long enough for the longest tap.
    buffer: Vec<f32>,
    write_index: usize,
    frames: usize,
    /// Per tap: frame offset, left gain, right gain.
    taps: Vec<(usize, f32, f32)>,
    /// One-pole low-pass state per ear: reflections lose treble on every bounce.
    damping_alpha: f32,
    left_damped: f32,
    right_damped: f32,
}

impl EarlyReflections {
    pub(crate) fn new(sample_rate: u32, settings: &RoomSettings) -> Result<Self, DspError> {
        validate_room(settings)?;
        let per_ms = sample_rate as f32 / 1000.0;
        let mut taps = Vec::with_capacity(REFLECTION_TAPS.len());
        let mut longest = 1usize;
        for (delay_ms, gain, left_leads) in REFLECTION_TAPS {
            // `size` shortens the room rather than lengthening it, so the longest
            // tap stays put and the buffer never has to grow with the setting.
            let scaled = (delay_ms * (0.45 + 0.55 * settings.size)).max(MIN_REFLECTION_MS);
            let frames = ((scaled * per_ms).round() as usize).max(1);
            longest = longest.max(frames);
            // The far ear hears the same reflection later and quieter; the HRTF
            // supplies the timing, so here it is only a level difference.
            let (left, right) = if left_leads {
                (gain, gain * 0.72)
            } else {
                (gain * 0.72, gain)
            };
            taps.push((frames, left * settings.amount, right * settings.amount));
        }

        let frames = longest + 1;
        Ok(Self {
            buffer: vec![0.0; frames * 2],
            write_index: 0,
            frames,
            taps,
            // Fixed 4.5 kHz corner: enough to take the edge off without dulling.
            damping_alpha: 1.0 - (-2.0 * PI * 4_500.0 / sample_rate as f32).exp(),
            left_damped: 0.0,
            right_damped: 0.0,
        })
    }

    pub(crate) fn process(&mut self, pcm: &mut [f32]) {
        for frame in pcm.chunks_exact_mut(2) {
            let left = frame[0];
            let right = frame[1];
            self.buffer[self.write_index * 2] = left;
            self.buffer[self.write_index * 2 + 1] = right;

            let mut wet_left = 0.0;
            let mut wet_right = 0.0;
            for &(delay, gain_left, gain_right) in &self.taps {
                let index = (self.write_index + self.frames - delay) % self.frames;
                // Reflections arrive crossed: a bounce off the left wall reaches the
                // right ear too, which is what widens the image rather than just
                // echoing each channel back onto itself.
                wet_left += self.buffer[index * 2 + 1] * gain_left;
                wet_right += self.buffer[index * 2] * gain_right;
            }

            self.left_damped += self.damping_alpha * (wet_left - self.left_damped);
            self.right_damped += self.damping_alpha * (wet_right - self.right_damped);

            frame[0] = left + self.left_damped;
            frame[1] = right + self.right_damped;

            self.write_index += 1;
            if self.write_index == self.frames {
                self.write_index = 0;
            }
        }
    }
}

pub(crate) struct StereoHrtf {
    left_to_left: PartitionedConvolver,
    left_to_right: PartitionedConvolver,
    right_to_left: PartitionedConvolver,
    right_to_right: PartitionedConvolver,
    /// Untreated copy for the A/B lane, delayed to match the processed path.
    dry_left: VecDeque<f32>,
    dry_right: VecDeque<f32>,
    latency: usize,
}

impl StereoHrtf {
    /// Strength is folded into the filters rather than applied as a dry/wet blend.
    ///
    /// Blending would sum two paths whose group delays differ by the HRIR's onset,
    /// and two delayed copies of one signal are a comb filter: a fixed pattern of
    /// cancellation notches that no gain setting can remove. The measured pair also
    /// inverts polarity below roughly 200 Hz, so at higher mixes the notch near
    /// 100 Hz deepened toward total cancellation. Convolving with one filter that
    /// already contains the unspatialised part cannot interfere with itself.
    pub(crate) fn new(sample_rate: u32, settings: &HrtfSettings) -> Result<Self, DspError> {
        validate_hrtf(settings)?;
        let (near, far, onset) = prepare_hrir_pair(sample_rate);
        let gain = settings.mix * 10.0f32.powf(settings.output_gain_db / 20.0);
        let direct = 1.0 - settings.mix;

        // The direct term sits at `onset` so it coincides with the spatialised
        // arrival. Placing it at tap 0 would reintroduce the delay mismatch.
        let mut same_ear = near.iter().map(|tap| tap * gain).collect::<Vec<_>>();
        if let Some(slot) = same_ear.get_mut(onset) {
            *slot += direct;
        }
        let cross_ear = far.iter().map(|tap| tap * gain).collect::<Vec<_>>();

        let latency = PARTITION_SIZE + onset;
        let mut dry_left = VecDeque::with_capacity(latency * 2);
        let mut dry_right = VecDeque::with_capacity(latency * 2);
        dry_left.resize(latency, 0.0);
        dry_right.resize(latency, 0.0);
        Ok(Self {
            // Left virtual speaker at -30° is the mirror of the measured +30° response.
            left_to_left: PartitionedConvolver::new(&same_ear),
            left_to_right: PartitionedConvolver::new(&cross_ear),
            right_to_left: PartitionedConvolver::new(&cross_ear),
            right_to_right: PartitionedConvolver::new(&same_ear),
            dry_left,
            dry_right,
            latency,
        })
    }

    /// Total delay through this processor: the convolver's block latency plus the
    /// arrival inside the impulse response.
    pub(crate) fn latency_frames(&self) -> usize {
        self.latency
    }

    pub(crate) fn process(&mut self, pcm: &mut [f32]) {
        for frame in pcm.chunks_exact_mut(2) {
            let left = frame[0];
            let right = frame[1];
            let (processed_left, processed_right, _, _) =
                self.process_frame(left, right, left, right);
            frame[0] = processed_left;
            frame[1] = processed_right;
        }
    }

    pub(crate) fn process_with_ab_dry(&mut self, pcm: &mut [f32], ab_dry: &mut [f32]) {
        debug_assert_eq!(pcm.len(), ab_dry.len());
        for (frame, ab_frame) in pcm.chunks_exact_mut(2).zip(ab_dry.chunks_exact_mut(2)) {
            let (processed_left, processed_right, untreated_left, untreated_right) =
                self.process_frame(frame[0], frame[1], ab_frame[0], ab_frame[1]);
            frame[0] = processed_left;
            frame[1] = processed_right;
            ab_frame[0] = untreated_left;
            ab_frame[1] = untreated_right;
        }
    }

    #[inline]
    fn process_frame(
        &mut self,
        left: f32,
        right: f32,
        untreated_left: f32,
        untreated_right: f32,
    ) -> (f32, f32, f32, f32) {
        // One filter per ear pair carries both the spatialised and direct parts, so
        // there is nothing left here to mix and nothing that can comb-filter.
        let out_left =
            self.left_to_left.process_sample(left) + self.right_to_left.process_sample(right);
        let out_right =
            self.left_to_right.process_sample(left) + self.right_to_right.process_sample(right);
        self.dry_left.push_back(untreated_left);
        self.dry_right.push_back(untreated_right);
        (
            out_left,
            out_right,
            self.dry_left.pop_front().unwrap_or(0.0),
            self.dry_right.pop_front().unwrap_or(0.0),
        )
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
            let gain = self.next_gain(frame);
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    pub(crate) fn process_with_ab_dry(
        &mut self,
        pcm: &mut [f32],
        ab_dry: &mut [f32],
        channels: usize,
    ) {
        debug_assert_eq!(pcm.len(), ab_dry.len());
        for (frame, ab_frame) in pcm
            .chunks_exact_mut(channels)
            .zip(ab_dry.chunks_exact_mut(channels))
        {
            let gain = self.next_gain(frame);
            for sample in frame {
                *sample *= gain;
            }
            for sample in ab_frame {
                *sample *= gain;
            }
        }
    }

    #[inline]
    fn next_gain(&mut self, frame: &[f32]) -> f32 {
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
        self.gain
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

/// Safety bound on the equalised response length. The useful tail decays inside
/// ~170 taps at 44.1 kHz, so this never truncates anything audible at any rate the
/// engine accepts; it only stops a bogus rate from allocating without limit.
const HRIR_MAX_TAPS: usize = 512;

/// Transform length for designing the correction: comfortably longer than the
/// impulse, so the response is resolved rather than smeared by wraparound.
const EQ_FFT_SIZE: usize = 512;

/// Taps kept from the correction filter.
const EQ_CORRECTION_TAPS: usize = 96;

/// Bound on the correction, so a narrow measurement null cannot become huge gain.
const EQ_MAX_CORRECTION_DB: f32 = 9.0;

/// Region worth correcting. Below it `restore_low_end` has already made the
/// response flat by construction; above it the 1994 measurement is unreliable.
const EQ_RANGE_HZ: (f32, f32) = (120.0, 17_000.0);

/// Resample the measured pair, then make it usable as a headphone filter.
///
/// Returns both equalised responses and the sample at which the near ear's energy
/// arrives — where an unspatialised copy must be placed to stay time-aligned.
///
/// Three defects of using raw measured data as a filter are corrected here:
///
/// * The 1994 measurement used a speaker that rolled off below roughly 200 Hz, so
///   the raw response has *negative* DC gain. Convolving with it does not merely
///   lose bass, it inverts it, and the inverted copy then cancels against anything
///   unprocessed it is mixed with.
/// * The measurement contains the dummy head's own concha and ear-canal resonance
///   near 3-4 kHz. Whoever is wearing the headphones still has those ears, so
///   applying it again counts them twice — the familiar honk of unequalised HRTF.
/// * Direction is carried by the *difference* between the ears, never by the
///   colouration they share, so flattening the shared part costs no localisation.
fn prepare_hrir_pair(sample_rate: u32) -> (Vec<f32>, Vec<f32>, usize) {
    let mut near = resample_hrir(&kemar::NEAR_EAR_30, sample_rate);
    let mut far = resample_hrir(&kemar::FAR_EAR_30, sample_rate);
    restore_low_end(&mut near, &mut far);

    let correction = free_field_correction(&near, &far, sample_rate);
    let near = convolve_bounded(&near, &correction, HRIR_MAX_TAPS);
    let far = convolve_bounded(&far, &correction, HRIR_MAX_TAPS);

    let onset = near
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
        .map(|(index, _)| index)
        .unwrap_or(0);
    (near, far, onset)
}

/// Give the pair unity gain at DC, restoring what the measurement speaker lacked.
///
/// The deficit is added through a unit-sum window, so it lands only below roughly
/// `sample_rate / len` and nothing above that moves. Splitting it evenly is right
/// because a head is acoustically transparent at these wavelengths: both ears hear
/// a low frequency essentially equally, and the timing difference that does the
/// localising down here lives in the phase the taps already carry.
fn restore_low_end(near: &mut [f32], far: &mut [f32]) {
    let present: f32 = near.iter().sum::<f32>() + far.iter().sum::<f32>();
    let deficit = 1.0 - present;
    if !deficit.is_finite() || near.is_empty() {
        return;
    }
    let len = near.len();
    let window: Vec<f32> = (0..len)
        .map(|index| {
            let phase = 2.0 * PI * index as f32 / (len as f32 - 1.0).max(1.0);
            0.5 - 0.5 * phase.cos()
        })
        .collect();
    let total: f32 = window.iter().sum();
    if total <= 0.0 {
        return;
    }
    for (index, weight) in window.iter().enumerate() {
        let share = weight / total * deficit / 2.0;
        near[index] += share;
        if index < far.len() {
            far[index] += share;
        }
    }
}

/// Minimum-phase filter that flattens the colouration the two ears share.
///
/// Minimum phase rather than zero phase is the whole point: a zero-phase inverse
/// spreads energy symmetrically about its peak, which places ringing *before* each
/// transient. On piano that is audible as a softened hammer strike, and the attack
/// is most of what identifies the instrument. Minimum phase puts every bit of the
/// same magnitude correction after the onset instead.
fn free_field_correction(near: &[f32], far: &[f32], sample_rate: u32) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(EQ_FFT_SIZE);
    let inverse = planner.plan_fft_inverse(EQ_FFT_SIZE);

    // A centred source reaches one ear by both paths at once, so their sum is the
    // response a mono voice or a piano actually meets.
    let mut spectrum = vec![Complex32::default(); EQ_FFT_SIZE];
    for (index, slot) in spectrum.iter_mut().enumerate() {
        let sum = near.get(index).copied().unwrap_or(0.0) + far.get(index).copied().unwrap_or(0.0);
        slot.re = sum;
    }
    forward.process(&mut spectrum);

    let bins = EQ_FFT_SIZE / 2;
    let bin_hz = sample_rate as f32 / EQ_FFT_SIZE as f32;
    let ceiling = 10.0f32.powf(EQ_MAX_CORRECTION_DB / 20.0);

    // Third-octave power average: correct the broad tilt, leave fine structure. The
    // fine structure is partly real interaural detail, and inverting it exactly
    // would need a far longer filter to no audible benefit.
    let mut target = vec![1.0f32; bins + 1];
    for (bin, slot) in target.iter_mut().enumerate() {
        let hz = bin as f32 * bin_hz;
        if hz < EQ_RANGE_HZ.0 || hz > EQ_RANGE_HZ.1 {
            continue;
        }
        let low = ((hz / 1.12) / bin_hz).floor().max(1.0) as usize;
        let high = (((hz * 1.12) / bin_hz).ceil() as usize).min(bins);
        let band = &spectrum[low..=high];
        if band.is_empty() {
            continue;
        }
        let power: f32 = band.iter().map(Complex32::norm_sqr).sum();
        let magnitude = (power / band.len() as f32).sqrt();
        *slot = if magnitude > 1.0e-6 {
            (1.0 / magnitude).clamp(1.0 / ceiling, ceiling)
        } else {
            ceiling
        };
    }

    minimum_phase(&target, &forward, &inverse)
}

/// Impulse response with the requested magnitude and no energy before its onset.
///
/// Standard real-cepstrum construction: the log spectrum is transformed to the
/// cepstral domain, the anticausal half folded onto the causal half, and the result
/// exponentiated back. Folding is what moves the ringing after the onset.
fn minimum_phase(
    magnitude: &[f32],
    forward: &Arc<dyn Fft<f32>>,
    inverse: &Arc<dyn Fft<f32>>,
) -> Vec<f32> {
    let bins = EQ_FFT_SIZE / 2;
    let mut log_spectrum = vec![Complex32::default(); EQ_FFT_SIZE];
    for (bin, slot) in log_spectrum.iter_mut().enumerate() {
        let mirrored = if bin <= bins { bin } else { EQ_FFT_SIZE - bin };
        let value = magnitude.get(mirrored).copied().unwrap_or(1.0).max(1.0e-9);
        slot.re = value.ln();
    }
    inverse.process(&mut log_spectrum);
    let scale = 1.0 / EQ_FFT_SIZE as f32;

    let mut cepstrum = vec![Complex32::default(); EQ_FFT_SIZE];
    cepstrum[0].re = log_spectrum[0].re * scale;
    cepstrum[bins].re = log_spectrum[bins].re * scale;
    for index in 1..bins {
        cepstrum[index].re = 2.0 * log_spectrum[index].re * scale;
    }
    forward.process(&mut cepstrum);

    for slot in cepstrum.iter_mut() {
        let gain = slot.re.exp();
        *slot = Complex32::new(gain * slot.im.cos(), gain * slot.im.sin());
    }
    inverse.process(&mut cepstrum);

    cepstrum
        .iter()
        .take(EQ_CORRECTION_TAPS)
        .map(|value| value.re * scale)
        .collect()
}

fn convolve_bounded(signal: &[f32], kernel: &[f32], limit: usize) -> Vec<f32> {
    let len = (signal.len() + kernel.len() - 1).min(limit);
    let mut out = vec![0.0f32; len];
    for (offset, &tap) in signal.iter().enumerate() {
        if offset >= len {
            break;
        }
        if tap == 0.0 {
            continue;
        }
        for (index, &coefficient) in kernel.iter().enumerate() {
            match out.get_mut(offset + index) {
                Some(slot) => *slot += tap * coefficient,
                None => break,
            }
        }
    }
    out
}

/// Highest crossfeed cutoff accepted by `CrossfeedProcessor::new` at `sample_rate`.
pub(crate) fn max_crossfeed_cutoff_hz(sample_rate: u32) -> f32 {
    sample_rate as f32 * 0.45
}

fn validate_crossfeed(sample_rate: u32, settings: &CrossfeedSettings) -> Result<(), DspError> {
    if !settings.amount.is_finite() || !(0.0..=0.5).contains(&settings.amount) {
        return Err(DspError::InvalidCrossfeedAmount(settings.amount));
    }
    if !settings.delay_ms.is_finite() || !(0.05..=1.0).contains(&settings.delay_ms) {
        return Err(DspError::InvalidCrossfeedDelay(settings.delay_ms));
    }
    let max_cutoff = max_crossfeed_cutoff_hz(sample_rate);
    if !settings.cutoff_hz.is_finite()
        || settings.cutoff_hz < 100.0
        || settings.cutoff_hz > max_cutoff
    {
        return Err(DspError::InvalidCrossfeedCutoff(settings.cutoff_hz));
    }
    Ok(())
}

fn validate_room(settings: &RoomSettings) -> Result<(), DspError> {
    if !settings.amount.is_finite() || !(0.0..=1.0).contains(&settings.amount) {
        return Err(DspError::InvalidRoomAmount(settings.amount));
    }
    if !settings.size.is_finite() || !(0.0..=1.0).contains(&settings.size) {
        return Err(DspError::InvalidRoomSize(settings.size));
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

    /// Magnitude of `taps` at `hz`, in dB.
    fn response_db(taps: &[f32], hz: f32, sample_rate: u32) -> f32 {
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (index, tap) in taps.iter().enumerate() {
            let angle = -2.0 * std::f64::consts::PI * hz as f64 * index as f64 / sample_rate as f64;
            re += *tap as f64 * angle.cos();
            im += *tap as f64 * angle.sin();
        }
        20.0 * re.hypot(im).max(1.0e-12).log10() as f32
    }

    /// What a centred voice or piano meets: both paths into one ear at once.
    fn centred_response_db(sample_rate: u32, hz: f32) -> f32 {
        let (near, far, onset) = prepare_hrir_pair(sample_rate);
        let gain = 0.72;
        let mut summed: Vec<f32> = near
            .iter()
            .zip(&far)
            .map(|(near, far)| (near + far) * gain)
            .collect();
        summed[onset] += 1.0 - gain;
        response_db(&summed, hz, sample_rate)
    }

    #[test]
    fn a_centred_source_passes_through_the_head_model_without_a_hole() {
        // Before the pair was equalised this dipped 26 dB at 100 Hz: the measured
        // response is polarity-inverted down there, so the unspatialised copy it was
        // blended with cancelled it. Bass fundamentals are most of a piano's body
        // and a voice's chest, which made the preset sound thin and boxy.
        for hz in [31.0, 62.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1_000.0] {
            let level = centred_response_db(48_000, hz);
            assert!(
                level.abs() <= 4.0,
                "{hz} Hz sits at {level:.1} dB, outside +-4 dB"
            );
        }
    }

    #[test]
    fn the_presence_region_is_not_boosted_by_the_dummy_heads_own_ears() {
        // Raw KEMAR carries the measurement head's concha and ear-canal resonance,
        // about +10 dB near 4 kHz. The listener has those resonances already, so
        // reapplying them counts them twice and turns female vocals harsh.
        for hz in [2_000.0, 2_500.0, 3_150.0, 4_000.0, 5_000.0, 6_300.0] {
            let level = centred_response_db(48_000, hz);
            assert!(
                level.abs() <= 4.0,
                "{hz} Hz sits at {level:.1} dB, outside +-4 dB"
            );
        }
    }

    #[test]
    fn equalisation_preserves_the_level_difference_that_carries_direction() {
        // Flattening what the ears share must not touch how they differ: the whole
        // spatial impression is that difference. A "fix" that removed it would
        // measure beautifully and collapse the image to mono.
        let (near, far, _) = prepare_hrir_pair(48_000);
        for (hz, minimum) in [(1_000.0, 4.0), (2_000.0, 4.0), (4_000.0, 6.0), (8_000.0, 10.0)] {
            let difference =
                response_db(&near, hz, 48_000) - response_db(&far, hz, 48_000);
            assert!(
                difference >= minimum,
                "{hz} Hz interaural difference fell to {difference:.1} dB"
            );
        }
    }

    #[test]
    fn the_correction_places_no_energy_before_the_onset() {
        // Zero-phase inversion rings symmetrically about its peak, which puts that
        // ringing ahead of every transient. A piano is identified by its hammer
        // strike, so pre-ringing is exactly the artefact to refuse here.
        let mut planner = FftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(EQ_FFT_SIZE);
        let inverse = planner.plan_fft_inverse(EQ_FFT_SIZE);
        let near = resample_hrir(&kemar::NEAR_EAR_30, 48_000);
        let far = resample_hrir(&kemar::FAR_EAR_30, 48_000);
        let correction = free_field_correction(&near, &far, 48_000);

        let peak = correction
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
            .map(|(index, _)| index)
            .expect("correction is not empty");
        assert_eq!(peak, 0, "minimum phase puts the peak at the first tap");

        // Round-trip the design so the helpers above are exercised, not just reachable.
        let magnitude = vec![1.0f32; EQ_FFT_SIZE / 2 + 1];
        let flat = minimum_phase(&magnitude, &forward, &inverse);
        assert!((flat[0] - 1.0).abs() < 1.0e-3, "flat target is a unit impulse");
        assert!(flat[1..].iter().all(|tap| tap.abs() < 1.0e-3));
    }

    #[test]
    fn restoring_the_low_end_leaves_the_rest_of_the_spectrum_alone() {
        let mut near = resample_hrir(&kemar::NEAR_EAR_30, 48_000);
        let mut far = resample_hrir(&kemar::FAR_EAR_30, 48_000);
        let before: Vec<f32> = near.iter().zip(&far).map(|(n, f)| n + f).collect();
        let dc_before: f32 = before.iter().sum();
        assert!(dc_before < 0.0, "measured pair starts polarity-inverted at DC");

        restore_low_end(&mut near, &mut far);
        let after: Vec<f32> = near.iter().zip(&far).map(|(n, f)| n + f).collect();
        assert!(
            (after.iter().sum::<f32>() - 1.0).abs() < 1.0e-3,
            "DC gain should land on unity"
        );

        // The correction is applied through a unit-sum window, so it is confined to
        // the region the 1994 measurement could not reach.
        for hz in [2_000.0, 4_000.0, 8_000.0, 12_000.0] {
            let moved = (response_db(&after, hz, 48_000) - response_db(&before, hz, 48_000)).abs();
            assert!(moved < 1.0, "{hz} Hz moved {moved:.2} dB");
        }
    }

    #[test]
    fn unity_output_gain_leaves_headroom_on_dense_material() {
        // Unity gain is only defensible if the filter does not inflate real peaks.
        // The bound worth testing is what dense music does, not the L1 norm: that
        // describes a signal built to align with every tap at once, which is not
        // audio, and it would condemn any filter with a long tail.
        let settings = HrtfSettings {
            enabled: true,
            mix: 0.72,
            output_gain_db: 0.0,
        };
        let mut hrtf = StereoHrtf::new(48_000, &settings).unwrap();
        // Dense partials near full scale: harder on a convolver than a single tone,
        // because many components can align in phase.
        let mut pcm = Vec::with_capacity(8_192 * 2);
        for frame in 0..8_192 {
            let time = frame as f32 / 48_000.0;
            let mut value = 0.0;
            for harmonic in 1..=12 {
                value += (2.0 * PI * 110.0 * harmonic as f32 * time).sin() / harmonic as f32;
            }
            let scaled = value * 0.28;
            pcm.push(scaled);
            pcm.push(scaled);
        }
        let input_peak = pcm.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        hrtf.process(&mut pcm);
        let output_peak = pcm.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));

        let growth_db = 20.0 * (output_peak / input_peak).log10();
        assert!(
            growth_db < 6.0,
            "peak grew {growth_db:.1} dB, more than the limiter should have to absorb"
        );
        assert!(
            growth_db > -6.0,
            "peak fell {growth_db:.1} dB, so the preset would sit quieter than bypass"
        );
    }

    #[test]
    fn linked_limiter_applies_the_processed_gain_to_the_ab_lane() {
        let settings = LimiterSettings {
            enabled: true,
            ceiling_db: -6.0,
            release_ms: 80.0,
        };
        let mut limiter = LinkedLimiter::new(48_000, &settings).unwrap();
        let mut below_ceiling = [0.25, -0.25];
        let mut loud_ab_dry = [2.0, -2.0];
        limiter.process_with_ab_dry(&mut below_ceiling, &mut loud_ab_dry, 2);
        assert_eq!(below_ceiling, [0.25, -0.25]);
        assert_eq!(loud_ab_dry, [2.0, -2.0]);

        let mut processed = [2.0, -1.0, 0.5, -0.25];
        let processed_before = processed;
        let mut ab_dry = [0.4, -0.2, 0.75, -0.375];
        let ab_before = ab_dry;

        limiter.process_with_ab_dry(&mut processed, &mut ab_dry, 2);

        for frame in 0..2 {
            let processed_gain = processed[frame * 2] / processed_before[frame * 2];
            assert!((ab_dry[frame * 2] - ab_before[frame * 2] * processed_gain).abs() < 1.0e-6);
            assert!(
                (ab_dry[frame * 2 + 1] - ab_before[frame * 2 + 1] * processed_gain).abs() < 1.0e-6
            );
        }
        assert!(processed.iter().all(|sample| sample.is_finite()));
        assert!(ab_dry.iter().all(|sample| sample.is_finite()));
    }
}
