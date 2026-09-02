//! Measure what a decoded stream actually contains, rather than what its label
//! claims.
//!
//! A file tagged 320 kbps may have been transcoded up from 128 kbps: the tag and
//! the file size change, but the discarded high frequencies never come back. A
//! lossy encoder removes everything above its band limit, leaving a cliff in the
//! spectrum that survives any later re-encode. Finding that cliff is the only
//! reliable way to tell a real 320 kbps file from an inflated one.
//!
//! The hard part is not detecting a cliff, it is *not* crying wolf. Solo piano,
//! close-miked voice and older recordings genuinely carry little energy near
//! Nyquist, and a detector that only looks at where energy falls off will call
//! every quiet acoustic track transcoded. So steepness is measured too: a codec
//! band limit drops tens of dB within a few bins, while natural rolloff is
//! gradual. Only the abrupt case is reported as a band limit.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

/// 4096 points at 48 kHz gives ~11.7 Hz bins: fine enough to locate a cliff to
/// well within the ~500 Hz spacing that separates one encoder preset's band limit
/// from the next.
const FFT_SIZE: usize = 4096;

/// Stop analysing after this many windows. At 48 kHz that is roughly 17 seconds of
/// audio spread across whatever was played, which is ample for a spectral average
/// and keeps the cost off a long track's back.
const MAX_WINDOWS: usize = 200;

/// Below this many windows the average is too noisy to draw a conclusion from.
const MIN_WINDOWS_FOR_VERDICT: usize = 24;

/// A window whose broadband level is this far under the loudest one seen is
/// skipped: silence and fades have no spectrum worth averaging, and including
/// them drags the noise floor down and fakes a cliff.
const QUIET_WINDOW_FLOOR_DB: f32 = -45.0;

/// Reference band for "how loud is this track": present in essentially all music,
/// and clear of both rumble and the region a codec would cut.
const REFERENCE_BAND_HZ: (f32, f32) = (300.0, 4_000.0);

/// Energy this far below the reference band counts as absent.
const ABSENT_DB: f32 = -55.0;

/// A drop of at least this much across `CLIFF_SPAN_HZ` is a band limit rather than
/// a natural rolloff.
const CLIFF_DROP_DB: f32 = 28.0;
const CLIFF_SPAN_HZ: f32 = 1_500.0;

/// Samples within this of full scale are treated as clipped.
const CLIP_THRESHOLD: f32 = 0.9995;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandLimit {
    /// A sharp cliff: content was band-limited by a lossy encoder.
    Abrupt,
    /// Energy fades out gradually — how the recording is, not how it was encoded.
    Gradual,
    /// Content reaches the top of what this sample rate can represent.
    FullBand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityReport {
    pub sample_rate: u32,
    pub channels: u16,
    /// Seconds of audio the spectrum was averaged over.
    pub analyzed_seconds: f32,
    /// Highest frequency carrying real energy, or `None` when undecided.
    pub cutoff_hz: Option<u32>,
    pub band_limit: Option<BandLimit>,
    /// dB drop measured across the span above the cutoff. Larger is sharper.
    pub rolloff_db: Option<f32>,
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    /// Peak-to-RMS. Small values mean heavy compression; piano should be large.
    pub crest_db: f32,
    /// Fraction of samples at or above full scale.
    pub clipped_ratio: f32,
    /// 1.0 is identical channels (effectively mono), 0 is unrelated. `None` if not
    /// stereo.
    pub channel_correlation: Option<f32>,
    /// False when too little was analysed to draw a conclusion.
    pub conclusive: bool,
}

impl QualityReport {
    /// Band limit expected of a stream whose label is honest, in Hz.
    ///
    /// These are the documented band limits of the common encoder presets, not a
    /// platform's naming: any source using the same labels maps the same way.
    pub fn expected_cutoff_for_label(label: &str) -> Option<u32> {
        let normalized = label.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "128k" | "128" => Some(16_000),
            "192k" | "192" => Some(18_000),
            "320k" | "320" => Some(20_000),
            // Lossless has no band limit; it reaches Nyquist.
            "flac" | "flac24bit" | "hires" | "lossless" => None,
            _ => None,
        }
    }

    /// Whether the measurement contradicts the label.
    ///
    /// Only an abrupt cliff well below the label's band limit counts. A gradual
    /// rolloff is a property of the recording, and being *above* the expected
    /// limit is never a complaint.
    pub fn contradicts_label(&self, label: &str) -> bool {
        if !self.conclusive || self.band_limit != Some(BandLimit::Abrupt) {
            return false;
        }
        let (Some(measured), Some(expected)) =
            (self.cutoff_hz, Self::expected_cutoff_for_label(label))
        else {
            return false;
        };
        // 1.5 kHz of slack: encoders vary and the bin grid is coarse.
        measured + 1_500 < expected
    }
}

pub struct QualityAnalyzer {
    sample_rate: u32,
    channels: usize,
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    /// Mono mixdown accumulating toward one FFT window.
    pending: Vec<f32>,
    scratch: Vec<Complex32>,
    /// Sum of per-window power spectra, and the count that produced it.
    spectrum: Vec<f32>,
    windows: usize,
    /// Loudest window seen, to decide which windows are too quiet to count.
    loudest_window_power: f32,
    frames: u64,
    peak: f32,
    sum_squares: f64,
    clipped: u64,
    sum_lr: f64,
    sum_ll: f64,
    sum_rr: f64,
}

impl QualityAnalyzer {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let hann = (0..FFT_SIZE)
            .map(|index| {
                let phase = 2.0 * std::f32::consts::PI * index as f32 / (FFT_SIZE as f32 - 1.0);
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Self {
            sample_rate,
            channels: channels.max(1),
            fft,
            hann,
            pending: Vec::with_capacity(FFT_SIZE),
            scratch: vec![Complex32::default(); FFT_SIZE],
            spectrum: vec![0.0; FFT_SIZE / 2 + 1],
            windows: 0,
            loudest_window_power: 0.0,
            frames: 0,
            peak: 0.0,
            sum_squares: 0.0,
            clipped: 0,
            sum_lr: 0.0,
            sum_ll: 0.0,
            sum_rr: 0.0,
        }
    }

    /// True once no further audio is needed, so the caller can stop feeding.
    pub fn is_saturated(&self) -> bool {
        self.windows >= MAX_WINDOWS
    }

    /// Feed interleaved source samples, before resampling or any DSP.
    ///
    /// Cheap statistics accumulate per sample; the transform runs once per full
    /// window and stops entirely at `MAX_WINDOWS`, so per-call cost is bounded and
    /// falls to near zero on a long track.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks(self.channels) {
            if frame.len() < self.channels {
                break;
            }
            self.frames += 1;
            let mut sum = 0.0f32;
            for &sample in frame {
                let magnitude = sample.abs();
                if magnitude > self.peak {
                    self.peak = magnitude;
                }
                if magnitude >= CLIP_THRESHOLD {
                    self.clipped += 1;
                }
                self.sum_squares += (sample as f64) * (sample as f64);
                sum += sample;
            }
            if self.channels >= 2 {
                let left = frame[0] as f64;
                let right = frame[1] as f64;
                self.sum_lr += left * right;
                self.sum_ll += left * left;
                self.sum_rr += right * right;
            }
            if self.windows < MAX_WINDOWS {
                // Mono mixdown: a band limit applies to every channel, and summing
                // improves the signal-to-noise of the average.
                self.pending.push(sum / self.channels as f32);
                if self.pending.len() == FFT_SIZE {
                    self.transform_pending();
                }
            }
        }
    }

    fn transform_pending(&mut self) {
        let power: f32 = self
            .pending
            .iter()
            .map(|sample| sample * sample)
            .sum::<f32>()
            / FFT_SIZE as f32;
        if power > self.loudest_window_power {
            self.loudest_window_power = power;
        }
        // Skip near-silence: it has no usable spectrum and averaging it in lowers
        // the floor until an ordinary rolloff looks like a cliff.
        let quiet_floor = self.loudest_window_power * db_to_power_ratio(QUIET_WINDOW_FLOOR_DB);
        if power <= quiet_floor {
            self.pending.clear();
            return;
        }

        for (index, sample) in self.pending.iter().enumerate() {
            self.scratch[index] = Complex32::new(sample * self.hann[index], 0.0);
        }
        self.fft.process(&mut self.scratch);
        for (bin, value) in self.spectrum.iter_mut().enumerate() {
            *value += self.scratch[bin].norm_sqr();
        }
        self.windows += 1;
        self.pending.clear();
    }

    pub fn finish(self) -> QualityReport {
        let bin_hz = self.sample_rate as f32 / FFT_SIZE as f32;
        let conclusive = self.windows >= MIN_WINDOWS_FOR_VERDICT;
        let average: Vec<f32> = if self.windows == 0 {
            Vec::new()
        } else {
            self.spectrum
                .iter()
                .map(|value| value / self.windows as f32)
                .collect()
        };

        let (cutoff_hz, band_limit, rolloff_db) = if conclusive {
            analyze_spectrum(&average, bin_hz, self.sample_rate)
        } else {
            (None, None, None)
        };

        let total_samples = (self.frames * self.channels as u64).max(1) as f64;
        let rms = (self.sum_squares / total_samples).sqrt() as f32;
        let peak_dbfs = amplitude_to_dbfs(self.peak);
        let rms_dbfs = amplitude_to_dbfs(rms);

        let correlation = if self.channels >= 2 {
            let denominator = (self.sum_ll * self.sum_rr).sqrt();
            if denominator > 0.0 {
                Some((self.sum_lr / denominator) as f32)
            } else {
                None
            }
        } else {
            None
        };

        QualityReport {
            sample_rate: self.sample_rate,
            channels: self.channels as u16,
            analyzed_seconds: self.frames as f32 / self.sample_rate.max(1) as f32,
            cutoff_hz,
            band_limit,
            rolloff_db,
            peak_dbfs,
            rms_dbfs,
            crest_db: peak_dbfs - rms_dbfs,
            clipped_ratio: (self.clipped as f64 / total_samples) as f32,
            channel_correlation: correlation,
            conclusive,
        }
    }
}

/// Locate the top of the content and judge how sharply it ends.
fn analyze_spectrum(
    average: &[f32],
    bin_hz: f32,
    sample_rate: u32,
) -> (Option<u32>, Option<BandLimit>, Option<f32>) {
    if average.is_empty() || bin_hz <= 0.0 {
        return (None, None, None);
    }
    let bin_of = |hz: f32| ((hz / bin_hz).round() as usize).min(average.len() - 1);

    // Reference level from a band every recording occupies.
    let reference = {
        let (low, high) = (bin_of(REFERENCE_BAND_HZ.0), bin_of(REFERENCE_BAND_HZ.1));
        if high <= low {
            return (None, None, None);
        }
        average[low..=high].iter().copied().fold(0.0f32, f32::max)
    };
    if reference <= 0.0 {
        return (None, None, None);
    }

    let absent = reference * db_to_power_ratio(ABSENT_DB);
    let nyquist_bin = average.len() - 1;
    // Scan down from Nyquist for the first bin still carrying energy.
    let Some(cutoff_bin) = (0..=nyquist_bin).rev().find(|&bin| average[bin] > absent) else {
        return (None, None, None);
    };
    let cutoff_hz = cutoff_bin as f32 * bin_hz;

    // Content reaching the top of the representable range was never band-limited.
    let nyquist = sample_rate as f32 / 2.0;
    if cutoff_hz >= nyquist - CLIFF_SPAN_HZ {
        return (
            Some(cutoff_hz.round() as u32),
            Some(BandLimit::FullBand),
            None,
        );
    }

    // Compare the level just below the cutoff with the level just above it: a
    // codec's band limit is a wall, a recording's rolloff is a slope.
    let span_bins = (CLIFF_SPAN_HZ / bin_hz).round() as usize;
    let below_high = cutoff_bin;
    let below_low = below_high.saturating_sub(span_bins);
    let inside = average[below_low..=below_high]
        .iter()
        .copied()
        .fold(0.0f32, f32::max);
    let above_low = (cutoff_bin + 1).min(nyquist_bin);
    let above_high = (cutoff_bin + span_bins).min(nyquist_bin);
    let outside = if above_high > above_low {
        average[above_low..=above_high]
            .iter()
            .copied()
            .fold(0.0f32, f32::max)
    } else {
        0.0
    };

    let drop_db = power_ratio_to_db(inside.max(f32::MIN_POSITIVE), outside);
    let band_limit = if drop_db >= CLIFF_DROP_DB {
        BandLimit::Abrupt
    } else {
        BandLimit::Gradual
    };
    (
        Some(cutoff_hz.round() as u32),
        Some(band_limit),
        Some(drop_db),
    )
}

fn db_to_power_ratio(db: f32) -> f32 {
    10.0f32.powf(db / 10.0)
}

/// Positive dB by how much `high` exceeds `low`, saturating for silence.
fn power_ratio_to_db(high: f32, low: f32) -> f32 {
    if low <= 0.0 || !low.is_finite() {
        return 120.0;
    }
    10.0 * (high / low).log10()
}

fn amplitude_to_dbfs(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return -120.0;
    }
    (20.0 * amplitude.log10()).max(-120.0)
}
