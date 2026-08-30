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
  {
    id: "warm",
    label: "温暖",
    description: "抬一点中低频、收一点高频，久听不累。",
  },
  {
    id: "bright",
    label: "明亮",
    description: "抬高频细节和空气感，适合闷一点的耳机。",
  },
  {
    id: "classical",
    label: "古典",
    description: "两端轻抬、中频略收，还原大编制的层次和厅堂感。",
  },
  {
    id: "electronic",
    label: "电子",
    description: "低频下潜配高频亮度，中频退后半步，避免糊成一团。",
  },
  {
    id: "rock",
    label: "摇滚",
    description: "低中频力度加高频咬合，250 Hz 让位以免人声被吉他埋掉。",
  },
  {
    id: "podcast",
    label: "播客",
    description: "重收低频隆隆声、抬人声清晰度，长时间说话内容不累耳。",
  },
  {
    id: "jazz",
    label: "爵士",
    description: "中频温润、保留高频空气感，低频基本不动。",
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

// Restrained voicing curves, ordered 31 Hz -> 16 kHz. Peak gain stays well inside
// the engine's +-12 dB product limit even after intensityScale's 1.4x ceiling, so a
// preset at full intensity still validates.
const WARM_GAINS = [0, 0.5, 1.25, 0.75, 0, 0, -0.25, -0.5, -0.75, -0.5] as const;
const BRIGHT_GAINS = [0, 0, 0, -0.25, 0, 0.25, 0.75, 1.25, 1.5, 1] as const;
const CLASSICAL_GAINS = [0.5, 0.5, 0, -0.5, -0.25, 0, 0.5, 0.75, 0.75, 0.5] as const;
// Sub weight plus air, with the middle stepped back so the mix does not turn muddy.
const ELECTRONIC_GAINS = [2.5, 2, 1, -0.5, -1, -0.5, 0, 1, 1.75, 1.25] as const;
// Body and bite together; the 250 Hz dip keeps vocals from being buried by guitars.
const ROCK_GAINS = [1.5, 1.75, 1, -1, -0.5, 0.5, 1.5, 1.75, 1, 0.25] as const;
// Speech: cut rumble hard, lift articulation, stay flat on top to avoid sibilance.
const PODCAST_GAINS = [-3.5, -3, -1.5, 0, 1, 2, 2.25, 1.25, 0, -0.5] as const;
// Warm midrange with air retained; bass is left essentially untouched.
const JAZZ_GAINS = [0.5, 0.75, 0.75, 0.25, -0.25, 0.25, 0.5, 0.75, 1, 0.75] as const;

/** Curve per voicing preset. These share one code path; only the gains differ. */
const VOICING_GAINS = {
  warm: WARM_GAINS,
  bright: BRIGHT_GAINS,
  classical: CLASSICAL_GAINS,
  electronic: ELECTRONIC_GAINS,
  rock: ROCK_GAINS,
  podcast: PODCAST_GAINS,
  jazz: JAZZ_GAINS,
} as const satisfies Record<string, readonly number[]>;

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
  return eqBands(zeroGains());
}

/** Band centre frequencies of the fixed product EQ, low to high. */
export const DSP_EQ_FREQUENCIES = EQ_FREQUENCIES;
/** Engine limit from `DspControlState::validate_product`; the editor clamps to it. */
export const DSP_EQ_MAX_GAIN_DB = 12;

export function zeroGains(): number[] {
  return EQ_FREQUENCIES.map(() => 0);
}

/**
 * Force a curve into the shape the engine accepts: exactly one gain per band, finite,
 * within +-12 dB. The editor clamps as you drag, but a curve can also arrive from
 * stored preferences, so it is normalized on the way in too.
 */
export function clampCustomGains(gains: readonly number[]): number[] {
  return EQ_FREQUENCIES.map((_, index) => {
    const gain = gains[index];
    if (typeof gain !== "number" || !Number.isFinite(gain)) return 0;
    return Math.min(DSP_EQ_MAX_GAIN_DB, Math.max(-DSP_EQ_MAX_GAIN_DB, gain));
  });
}

/** Read the curve back off a control state, for seeding the editor from any preset. */
export function gainsFromSettings(settings: DspSettings): number[] {
  return clampCustomGains(settings.eqBands.map((band) => band.gainDb));
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
  customGains: readonly number[] = zeroGains(),
): DspSettings {
  const normalizedIntensity = clampDspAmount(intensity);
  const normalizedSpatialAmount = clampDspAmount(spatialAmount);

  switch (presetId) {
    case "custom":
      // A hand-edited curve is used as authored: intensity would silently rescale
      // gains the user set by hand, so it is deliberately not applied here.
      return settings({
        eqEnabled: true,
        bands: eqBands(clampCustomGains(customGains)),
        crossfeedEnabled: true,
        crossfeedAmount: CROSSFEED_LIGHT,
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
        limiterEnabled: true,
      });
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
    case "warm":
    case "bright":
    case "classical":
    case "electronic":
    case "rock":
    case "podcast":
    case "jazz":
      // Voicing presets differ only by curve: EQ on, light crossfeed to soften the
      // headphone split, HRTF off because the engine reserves it for `spatial`, and
      // the limiter on to catch the boosted peaks.
      return settings({
        eqEnabled: true,
        bands: eqBands(VOICING_GAINS[presetId], intensityScale(normalizedIntensity)),
        crossfeedEnabled: true,
        crossfeedAmount: CROSSFEED_LIGHT,
        hrtfEnabled: false,
        hrtfMix: HRTF_MEDIUM,
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
  customGains: readonly number[] = zeroGains(),
): DspControlState {
  const normalizedIntensity = clampDspAmount(intensity);
  const normalizedSpatialAmount = clampDspAmount(spatialAmount);
  return {
    settings: buildDspSettings(
      activePresetId,
      normalizedIntensity,
      normalizedSpatialAmount,
      customGains,
    ),
    activePresetId,
    intensity: normalizedIntensity,
    spatialAmount: normalizedSpatialAmount,
  };
}

/**
 * The custom curve is reachable from the advanced editor rather than the preset shelf,
 * so it is described here instead of in DSP_PRESETS. Without an entry, lookups would
 * fall back to the first preset and the summary would name the wrong thing.
 */
export const DSP_CUSTOM_PRESET = {
  id: "custom",
  label: "自定义",
  description: "你自己调的曲线，强度不再二次缩放，所听即所调。",
} as const satisfies { id: DspPresetId; label: string; description: string };

export function getDspPreset(presetId: DspPresetId) {
  if (presetId === "custom") return DSP_CUSTOM_PRESET;
  return DSP_PRESETS.find((preset) => preset.id === presetId) ?? DSP_PRESETS[0];
}
