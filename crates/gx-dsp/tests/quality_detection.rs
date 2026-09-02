//! The detector exists to answer one question honestly: was this file's high end
//! removed by an encoder, or was it never there?
//!
//! Getting a "yes" on genuinely band-limited audio is the easy half. These tests
//! spend most of their effort on the other half — a quiet acoustic recording, a
//! gentle rolloff, a fade — because a false accusation is worse than no verdict:
//! it would tell someone their good file is fake.

use std::f32::consts::PI;

use gx_dsp::quality::{BandLimit, QualityAnalyzer, QualityReport};

const SAMPLE_RATE: u32 = 48_000;

/// Deterministic value noise; a PRNG keeps the fixture reproducible.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        // xorshift32
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Sum of sines up to `top_hz`, so the spectrum ends exactly where intended.
///
/// Partials are spaced closely enough to fill the band and given unequal
/// amplitudes so the result is not a single tone with a trivial spectrum.
fn band_limited(seconds: f32, top_hz: f32, rolloff: Rolloff) -> Vec<f32> {
    let frames = (SAMPLE_RATE as f32 * seconds) as usize;
    let mut out = vec![0.0f32; frames * 2];
    let nyquist = SAMPLE_RATE as f32 / 2.0;
    // A cliff stops dead at `top_hz`. A gentle rolloff must keep producing
    // partials above it, only quieter — content that fades and then stops is
    // still a wall, which is the distinction under test.
    let highest = match rolloff {
        Rolloff::Cliff => top_hz,
        Rolloff::Gentle => nyquist * 0.98,
    };
    let mut partial_hz = 90.0f32;
    let mut index = 0;
    while partial_hz < highest {
        let level = match rolloff {
            Rolloff::Cliff => 1.0 / (1.0 + index as f32 * 0.05),
            Rolloff::Gentle => {
                // Smooth decay through `top_hz` and beyond, never reaching zero
                // abruptly: about 6 dB per octave above the corner.
                let reach = partial_hz / top_hz;
                let taper = if reach <= 1.0 {
                    1.0
                } else {
                    1.0 / (reach * reach)
                };
                taper / (1.0 + index as f32 * 0.05)
            }
        };
        // Phase is accumulated per sample, in f64. Computing `step * frame` in f32
        // loses all fractional precision past a few million radians, which turns a
        // pure tone into broadband noise and silently invalidates the fixture.
        let step = 2.0 * std::f64::consts::PI * partial_hz as f64 / SAMPLE_RATE as f64;
        let mut phase = index as f64 * 0.7;
        for frame in 0..frames {
            let value = level * phase.sin() as f32;
            out[frame * 2] += value;
            out[frame * 2 + 1] += value;
            phase += step;
            if phase > std::f64::consts::TAU {
                phase -= std::f64::consts::TAU;
            }
        }
        partial_hz *= 1.06;
        index += 1;
    }
    normalize(&mut out, 0.6);
    out
}

#[derive(Clone, Copy)]
enum Rolloff {
    Cliff,
    Gentle,
}

fn normalize(samples: &mut [f32], target_peak: f32) {
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    if peak > 0.0 {
        let scale = target_peak / peak;
        for sample in samples {
            *sample *= scale;
        }
    }
}

fn analyze(samples: &[f32]) -> QualityReport {
    let mut analyzer = QualityAnalyzer::new(SAMPLE_RATE, 2);
    // Feed in realistic chunks, as the decoder does.
    for chunk in samples.chunks(2_048) {
        analyzer.push(chunk);
    }
    analyzer.finish()
}

#[test]
fn a_hard_band_limit_at_16k_is_reported_as_abrupt() {
    let report = analyze(&band_limited(6.0, 16_000.0, Rolloff::Cliff));

    assert!(report.conclusive, "{report:?}");
    assert_eq!(report.band_limit, Some(BandLimit::Abrupt), "{report:?}");
    let cutoff = report.cutoff_hz.expect("cutoff");
    assert!(
        (15_000..=17_000).contains(&cutoff),
        "cutoff {cutoff} should sit at the 16 kHz wall"
    );
}

#[test]
fn a_file_labelled_320k_but_cut_at_16k_contradicts_its_label() {
    let report = analyze(&band_limited(6.0, 16_000.0, Rolloff::Cliff));

    // This is the case worth catching: a 128 kbps file re-encoded and relabelled.
    assert!(report.contradicts_label("320k"), "{report:?}");
    // Against its honest label the same measurement is no complaint at all.
    assert!(!report.contradicts_label("128k"), "{report:?}");
}

#[test]
fn a_gentle_rolloff_is_not_called_a_band_limit() {
    // Quiet acoustic material — solo piano, close-miked voice — simply runs out of
    // energy up high. Reporting that as an encoder cut would be a false accusation.
    let report = analyze(&band_limited(6.0, 16_500.0, Rolloff::Gentle));

    assert!(report.conclusive, "{report:?}");
    assert_ne!(report.band_limit, Some(BandLimit::Abrupt), "{report:?}");
    assert!(!report.contradicts_label("320k"), "{report:?}");
}

#[test]
fn full_band_content_is_recognised_rather_than_flagged() {
    let mut noise = Noise(0x1234_5678);
    let frames = SAMPLE_RATE as usize * 5;
    let mut samples = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        // Broadband noise reaches Nyquist, like lossless audio with real treble.
        let value = noise.next() * 0.35;
        samples[frame * 2] = value;
        samples[frame * 2 + 1] = noise.next() * 0.35;
    }

    let report = analyze(&samples);

    assert_eq!(report.band_limit, Some(BandLimit::FullBand), "{report:?}");
    assert!(!report.contradicts_label("flac"), "{report:?}");
    assert!(!report.contradicts_label("320k"), "{report:?}");
}

#[test]
fn silence_and_fades_do_not_manufacture_a_cliff() {
    // Half the material is a long fade to nothing. Averaging those windows in
    // would drag the noise floor down until an ordinary rolloff looked abrupt.
    let mut samples = band_limited(6.0, 20_000.0, Rolloff::Cliff);
    let frames = samples.len() / 2;
    let fade_from = frames / 2;
    for frame in fade_from..frames {
        let remaining = 1.0 - (frame - fade_from) as f32 / (frames - fade_from) as f32;
        samples[frame * 2] *= remaining;
        samples[frame * 2 + 1] *= remaining;
    }

    let report = analyze(&samples);

    let cutoff = report.cutoff_hz.expect("cutoff");
    assert!(
        cutoff >= 19_000,
        "the fade pulled the measured cutoff down to {cutoff}"
    );
}

#[test]
fn too_little_audio_yields_no_verdict_instead_of_a_guess() {
    // A few hundred milliseconds is not enough to average; saying so is the honest
    // outcome, and every consumer checks `conclusive` before believing a cutoff.
    let report = analyze(&band_limited(0.3, 16_000.0, Rolloff::Cliff));

    assert!(!report.conclusive, "{report:?}");
    assert_eq!(report.cutoff_hz, None);
    assert_eq!(report.band_limit, None);
    assert!(!report.contradicts_label("320k"));
}

#[test]
fn loudness_statistics_describe_the_material() {
    let samples = band_limited(4.0, 18_000.0, Rolloff::Cliff);
    let report = analyze(&samples);

    // Normalised to 0.6, so a little under full scale, and nothing clipped.
    assert!((report.peak_dbfs - -4.4).abs() < 1.0, "{report:?}");
    assert_eq!(report.clipped_ratio, 0.0, "{report:?}");
    // Dense partials give a modest crest; the point is that it is measured at all.
    assert!(
        report.crest_db > 0.0 && report.crest_db < 40.0,
        "{report:?}"
    );
    // Both channels carry the same signal here.
    let correlation = report.channel_correlation.expect("stereo correlation");
    assert!(correlation > 0.99, "{correlation}");
}

#[test]
fn clipping_is_counted_rather_than_inferred() {
    let frames = SAMPLE_RATE as usize * 3;
    let mut samples = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        let phase = 2.0 * PI * 440.0 * frame as f32 / SAMPLE_RATE as f32;
        // Overdriven sine: the tops are flattened at full scale.
        let value = (phase.sin() * 1.6).clamp(-1.0, 1.0);
        samples[frame * 2] = value;
        samples[frame * 2 + 1] = value;
    }

    let report = analyze(&samples);

    assert!(report.clipped_ratio > 0.1, "{}", report.clipped_ratio);
    assert!((report.peak_dbfs - 0.0).abs() < 0.1, "{report:?}");
}

#[test]
fn lossless_labels_carry_no_expected_band_limit() {
    // Nothing to contradict: lossless has no band limit to fall short of.
    assert_eq!(QualityReport::expected_cutoff_for_label("flac"), None);
    assert_eq!(QualityReport::expected_cutoff_for_label("flac24bit"), None);
    assert_eq!(
        QualityReport::expected_cutoff_for_label("320k"),
        Some(20_000)
    );
    assert_eq!(
        QualityReport::expected_cutoff_for_label("128k"),
        Some(16_000)
    );
    // An unfamiliar label is not guessed at.
    assert_eq!(QualityReport::expected_cutoff_for_label("mystery"), None);
}

#[test]
fn analysis_stops_once_saturated_so_a_long_track_costs_no_more() {
    let mut analyzer = QualityAnalyzer::new(SAMPLE_RATE, 2);
    let samples = band_limited(30.0, 16_000.0, Rolloff::Cliff);
    for chunk in samples.chunks(2_048) {
        analyzer.push(chunk);
        if analyzer.is_saturated() {
            break;
        }
    }

    assert!(analyzer.is_saturated());
    let report = analyzer.finish();
    assert_eq!(report.band_limit, Some(BandLimit::Abrupt), "{report:?}");
    // Saturation arrives well before the whole track has been consumed.
    assert!(report.analyzed_seconds < 25.0, "{report:?}");
}
