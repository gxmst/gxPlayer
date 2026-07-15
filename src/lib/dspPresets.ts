import type { DspControlState, DspPresetId, DspSettings, EqBand } from "../types";

export const DSP_DEFAULT_INTENSITY = 0.5;
export const DSP_DEFAULT_SPATIAL_AMOUNT = 0.5;

export const DSP_PRESETS = [
  {
    id: "bypass",
    label: "原声",
    description: "整链关闭，不添加音效处理，保持零 DSP 延迟。",
  },
  {
    id: "headphone_daily",
    label: "耳机日常",
    description: "自然串音，减轻耳机左右声道的割裂感。",
  },
  {
    id: "vocal",
    label: "人声",
    description: "轻收低中频，让人声更清楚、更靠前。",
  },
  {
    id: "bass",
    label: "低音",
    description: "克制提升低频厚度，不追求夸张轰鸣。",
  },
  {
    id: "spatial",
    label: "空间",
    description: "固定前方 ±30° 音箱感，可能偏闷；建议不与系统杜比耳机虚拟化同时开。",
  },
] as const satisfies ReadonlyArray<{
  id: DspPresetId;
  label: string;
  description: string;
}>;

export const DSP_SYSTEM_EFFECTS_HINT = "系统音效（如杜比）开启时，建议用原声。";
export const DSP_AB_LABEL = "按住听未处理";

const EQ_FREQUENCIES = [31, 62, 125, 250, 500, 1_000, 2_000, 4_000, 8_000, 16_000] as const;
const VOCAL_GAINS = [0, 0, -2, -1, 0, 2, 2.5, 0, 0, 0] as const;
const BASS_GAINS = [2, 3, 2, 0, 0, 0, 0, 0, 0, 0] as const;

// Future-only restrained curves. They intentionally stay outside DSP_PRESETS,
// so v1 exposes exactly the five product choices above without a hidden editor.
export const DSP_INTERNAL_EQ_PRESETS = {
  warm: {
    label: "温暖",
    gains: [0, 0.5, 1.25, 0.75, 0, 0, -0.25, -0.5, -0.75, -0.5],
  },
  bright: {
    label: "明亮",
    gains: [0, 0, 0, -0.25, 0, 0.25, 0.75, 1.25, 1.5, 1],
  },
  classical: {
    label: "古典",
    gains: [0.5, 0.5, 0, -0.5, -0.25, 0, 0.5, 0.75, 0.75, 0.5],
  },
} as const;

const CROSSFEED_LIGHT = 0.13;
const CROSSFEED_MEDIUM = 0.18;
const CROSSFEED_STRONG = 0.27;
const CROSSFEED_DELAY_MS = 0.28;
const CROSSFEED_CUTOFF_HZ = 700;

const HRTF_LIGHT = 0.3;
const HRTF_MEDIUM = 0.55;
const HRTF_STRONG = 0.72;
const HRTF_OUTPUT_GAIN_DB = -6;

const LIMITER_CEILING_DB = -1;
const LIMITER_RELEASE_MS = 80;

export function clampDspAmount(value: number): number {
  if (!Number.isFinite(value)) return DSP_DEFAULT_INTENSITY;
  return Math.min(1, Math.max(0, value));
}

function interpolateThreeAnchors(value: number, light: number, medium: number, strong: number): number {
  const normalized = clampDspAmount(value);
  if (normalized <= 0.5) {
    return light + (medium - light) * normalized * 2;
  }
  return medium + (strong - medium) * (normalized - 0.5) * 2;
}

function intensityScale(intensity: number): number {
  return 0.6 + clampDspAmount(intensity) * 0.8;
}

function eqBands(gains: readonly number[], scale = 1): EqBand[] {
  return EQ_FREQUENCIES.map((frequencyHz, index) => ({
    // Keep the complete 10-band dictionary, but do not instantiate identity
    // filters. Besides saving work, this keeps the dormant 16 kHz band valid
    // on 32 kHz output devices where it sits just above the Nyquist guard.
    enabled: gains[index] !== 0,
    kind: "peak",
    frequencyHz,
    gainDb: gains[index] * scale,
    q: 1,
  }));
}

function zeroEqBands(): EqBand[] {
  return eqBands(EQ_FREQUENCIES.map(() => 0));
}

function settings({
  eqEnabled,
  bands,
  crossfeedEnabled,
  crossfeedAmount,
  hrtfEnabled,
  hrtfMix,
  limiterEnabled,
}: {
  eqEnabled: boolean;
  bands: EqBand[];
  crossfeedEnabled: boolean;
  crossfeedAmount: number;
  hrtfEnabled: boolean;
  hrtfMix: number;
  limiterEnabled: boolean;
}): DspSettings {
  const enabled = eqEnabled || crossfeedEnabled || hrtfEnabled || limiterEnabled;
  return {
    enabled,
    eqEnabled,
    eqBands: bands,
    crossfeed: {
      enabled: crossfeedEnabled,
      amount: crossfeedAmount,
      delayMs: CROSSFEED_DELAY_MS,
      cutoffHz: CROSSFEED_CUTOFF_HZ,
    },
    hrtf: {
      enabled: hrtfEnabled,
      mix: hrtfMix,
      outputGainDb: HRTF_OUTPUT_GAIN_DB,
    },
    limiter: {
      enabled: limiterEnabled,
      ceilingDb: LIMITER_CEILING_DB,
      releaseMs: LIMITER_RELEASE_MS,
    },
  };
}

export function buildDspSettings(
  presetId: DspPresetId,
  intensity = DSP_DEFAULT_INTENSITY,
  spatialAmount = DSP_DEFAULT_SPATIAL_AMOUNT,
): DspSettings {
  const normalizedIntensity = clampDspAmount(intensity);
  const normalizedSpatialAmount = clampDspAmount(spatialAmount);

  switch (presetId) {
    case "bypass":
      return settings({
        eqEnabled: false,
        bands: zeroEqBands(),
        crossfeedEnabled: false,
        crossfeedAmount: CROSSFEED_MEDIUM,
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
        limiterEnabled: false,
      });
    case "headphone_daily":
      return settings({
        eqEnabled: false,
        bands: zeroEqBands(),
        crossfeedEnabled: true,
        crossfeedAmount: interpolateThreeAnchors(
          normalizedIntensity,
          CROSSFEED_LIGHT,
          CROSSFEED_MEDIUM,
          CROSSFEED_STRONG,
        ),
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
        limiterEnabled: true,
      });
    case "vocal":
      return settings({
        eqEnabled: true,
        bands: eqBands(VOCAL_GAINS, intensityScale(normalizedIntensity)),
        crossfeedEnabled: true,
        crossfeedAmount: CROSSFEED_LIGHT,
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
        limiterEnabled: true,
      });
    case "bass":
      return settings({
        eqEnabled: true,
        bands: eqBands(BASS_GAINS, intensityScale(normalizedIntensity)),
        crossfeedEnabled: true,
        crossfeedAmount: CROSSFEED_LIGHT,
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
        limiterEnabled: true,
      });
    case "spatial":
      return settings({
        eqEnabled: false,
        bands: zeroEqBands(),
        crossfeedEnabled: true,
        crossfeedAmount: CROSSFEED_MEDIUM,
        hrtfEnabled: true,
        hrtfMix: interpolateThreeAnchors(
          normalizedSpatialAmount,
          HRTF_LIGHT,
          HRTF_MEDIUM,
          HRTF_STRONG,
        ),
        limiterEnabled: true,
      });
    default:
      throw new Error(`unknown DSP preset: ${String(presetId)}`);
  }
}

export function buildDspControlState(
  activePresetId: DspPresetId,
  intensity = DSP_DEFAULT_INTENSITY,
  spatialAmount = DSP_DEFAULT_SPATIAL_AMOUNT,
): DspControlState {
  const normalizedIntensity = clampDspAmount(intensity);
  const normalizedSpatialAmount = clampDspAmount(spatialAmount);
  return {
    settings: buildDspSettings(activePresetId, normalizedIntensity, normalizedSpatialAmount),
    activePresetId,
    intensity: normalizedIntensity,
    spatialAmount: normalizedSpatialAmount,
  };
}

export function getDspPreset(presetId: DspPresetId) {
  return DSP_PRESETS.find((preset) => preset.id === presetId) ?? DSP_PRESETS[0];
}
