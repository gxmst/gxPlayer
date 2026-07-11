use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use gx_dsp::{CrossfeedSettings, DspSettings, HrtfSettings, LimiterSettings};
use serde::{Deserialize, Serialize};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const TRIALS: usize = 12;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrialManifest {
    instructions: String,
    reference_a: String,
    reference_b: String,
    trials: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Answers(Vec<String>);

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("generate") => generate(PathBuf::from(
            args.next()
                .context("usage: spatial-lab generate <output-directory>")?,
        )),
        Some("score") => score(
            PathBuf::from(
                args.next()
                    .context("usage: spatial-lab score <output-directory> <responses.json>")?,
            ),
            PathBuf::from(
                args.next()
                    .context("usage: spatial-lab score <output-directory> <responses.json>")?,
            ),
        ),
        Some("measure") => measure(),
        _ => bail!(
            "usage: spatial-lab generate <output-directory> | score <output-directory> <responses.json> | measure"
        ),
    }
}

fn generate(root: PathBuf) -> Result<()> {
    fs::create_dir_all(&root)?;
    let dry = calibration_signal();
    let mut wet = dry.clone();
    let mut chain = gx_dsp::DspChain::new(SAMPLE_RATE, CHANNELS, spatial_settings())?;
    chain.process_interleaved_in_place(&mut wet)?;
    let dry = delay_and_trim(&dry, chain.latency_frames());
    let dry = normalize(dry, 0.7);
    let wet = normalize(wet, 0.7);
    write_wav(&root.join("reference-a-bypass.wav"), &dry)?;
    write_wav(&root.join("reference-b-spatial.wav"), &wet)?;

    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut answers = Vec::with_capacity(TRIALS);
    let mut trials = Vec::with_capacity(TRIALS);
    for index in 0..TRIALS {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let answer = if seed & 1 == 0 { "A" } else { "B" };
        let name = format!("trial-{:02}.wav", index + 1);
        write_wav(&root.join(&name), if answer == "A" { &dry } else { &wet })?;
        answers.push(answer.to_owned());
        trials.push(name);
    }
    let manifest = TrialManifest {
        instructions: "Listen to reference A and B, then identify every trial as A or B. Put a JSON string array such as [\"A\",\"B\"] in responses.json. Do not inspect answers.json before scoring.".into(),
        reference_a: "reference-a-bypass.wav".into(),
        reference_b: "reference-b-spatial.wav".into(),
        trials,
    };
    fs::write(
        root.join("trials.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(
        root.join("answers.json"),
        serde_json::to_vec_pretty(&Answers(answers))?,
    )?;
    fs::write(
        root.join("listening-notes.md"),
        "# GXPlayer Phase 4 listening notes\n\n- Headphones:\n- Volume:\n- Session duration (target >= 30 minutes):\n- HRTF front/back confusion (1-5):\n- Externalization (1-5):\n- Tonal coloration (1-5):\n- Fatigue (1-5, lower is better):\n- Preferred dry/wet mix:\n- Free-form notes:\n",
    )?;
    println!(
        "GX_PHASE4_BLIND_LAB_READY {} trials={TRIALS}",
        root.display()
    );
    Ok(())
}

fn measure() -> Result<()> {
    let mut chain = gx_dsp::DspChain::new(SAMPLE_RATE, CHANNELS, spatial_settings())?;
    let latency = chain.latency_frames();
    let mut pcm = calibration_signal();
    let audio_seconds = pcm.len() as f64 / CHANNELS as f64 / SAMPLE_RATE as f64;
    let started = std::time::Instant::now();
    chain.process_interleaved_in_place(&mut pcm)?;
    let elapsed = started.elapsed().as_secs_f64();
    let peak = pcm
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if !pcm.iter().all(|sample| sample.is_finite()) {
        bail!("spatial measurement produced non-finite PCM");
    }
    println!(
        "GX_PHASE4_MEASURE_OK latency_frames={latency} latency_ms={:.3} peak={peak:.6} cpu_realtime_ratio={:.4}",
        latency as f64 * 1000.0 / SAMPLE_RATE as f64,
        elapsed / audio_seconds
    );
    Ok(())
}

fn spatial_settings() -> DspSettings {
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

fn score(root: PathBuf, responses_path: PathBuf) -> Result<()> {
    let Answers(answers) = serde_json::from_slice(&fs::read(root.join("answers.json"))?)?;
    let Answers(responses) = serde_json::from_slice(&fs::read(responses_path)?)?;
    if responses.len() != answers.len() {
        bail!(
            "expected {} responses but received {}",
            answers.len(),
            responses.len()
        );
    }
    let correct = answers
        .iter()
        .zip(&responses)
        .filter(|(answer, response)| answer.eq_ignore_ascii_case(response))
        .count();
    println!(
        "GX_PHASE4_BLIND_SCORE correct={correct} total={} accuracy={:.1}%",
        answers.len(),
        correct as f32 * 100.0 / answers.len() as f32
    );
    Ok(())
}

fn calibration_signal() -> Vec<f32> {
    let frames = SAMPLE_RATE as usize * 8;
    let mut pcm = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let segment = frame / (SAMPLE_RATE as usize / 2);
        let active = (frame % (SAMPLE_RATE as usize / 2)) < SAMPLE_RATE as usize * 2 / 5;
        let envelope = if active { 1.0 } else { 0.0 };
        let frequency = [220.0, 440.0, 880.0, 1760.0][segment % 4];
        let sample = (frame as f32 * frequency * std::f32::consts::TAU / SAMPLE_RATE as f32).sin()
            * 0.22
            * envelope;
        match segment % 3 {
            0 => pcm.extend_from_slice(&[sample, 0.0]),
            1 => pcm.extend_from_slice(&[0.0, sample]),
            _ => pcm.extend_from_slice(&[sample, sample]),
        }
    }
    pcm
}

fn delay_and_trim(pcm: &[f32], latency_frames: usize) -> Vec<f32> {
    let mut delayed = vec![0.0; latency_frames * CHANNELS];
    delayed.extend_from_slice(pcm);
    delayed.truncate(pcm.len());
    delayed
}

fn normalize(mut pcm: Vec<f32>, target_peak: f32) -> Vec<f32> {
    let peak = pcm
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak > 0.0 {
        let gain = target_peak / peak;
        for sample in &mut pcm {
            *sample *= gain;
        }
    }
    pcm
}

fn write_wav(path: &Path, pcm: &[f32]) -> Result<()> {
    let channels = CHANNELS as u16;
    let bits_per_sample = 16u16;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = SAMPLE_RATE * block_align as u32;
    let data_size = (pcm.len() * 2) as u32;
    let mut file = fs::File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    for sample in pcm {
        let quantized = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        file.write_all(&quantized.to_le_bytes())?;
    }
    Ok(())
}
