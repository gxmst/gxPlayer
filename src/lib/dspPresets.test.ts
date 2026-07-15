import { describe, expect, it } from "vitest";
import {
  buildDspControlState,
  buildDspSettings,
  DSP_INTERNAL_EQ_PRESETS,
  DSP_PRESETS,
  getDspPreset,
} from "./dspPresets";

const FREQUENCIES = [31, 62, 125, 250, 500, 1_000, 2_000, 4_000, 8_000, 16_000];

describe("DSP presets", () => {
  it("defines the five v1 presets and always emits a complete 10-band peak EQ", () => {
    expect(DSP_PRESETS.map((preset) => preset.id)).toEqual([
      "bypass",
      "headphone_daily",
      "vocal",
      "bass",
      "spatial",
    ]);

    for (const preset of DSP_PRESETS) {
      const result = buildDspSettings(preset.id);
      expect(result.eqBands).toHaveLength(10);
      expect(result.eqBands.map((band) => band.frequencyHz)).toEqual(FREQUENCIES);
      expect(result.eqBands.every((band) => band.kind === "peak" && band.q === 1)).toBe(true);
      expect(result.eqBands.every((band) => band.enabled === (band.gainDb !== 0))).toBe(true);
    }
  });

  it("keeps future warm, bright and classical curves internal and restrained", () => {
    expect(Object.keys(DSP_INTERNAL_EQ_PRESETS)).toEqual(["warm", "bright", "classical"]);
    expect(DSP_PRESETS.map((preset) => preset.label)).not.toContain("温暖");
    expect(DSP_PRESETS.map((preset) => preset.label)).not.toContain("明亮");
    expect(DSP_PRESETS.map((preset) => preset.label)).not.toContain("古典");
    for (const preset of Object.values(DSP_INTERNAL_EQ_PRESETS)) {
      expect(preset.gains).toHaveLength(10);
      expect(Math.max(...preset.gains.map(Math.abs))).toBeLessThanOrEqual(3);
    }
  });

  it("keeps bypass as a true disabled chain", () => {
    const result = buildDspSettings("bypass");
    expect(result.enabled).toBe(false);
    expect(result.eqEnabled).toBe(false);
    expect(result.crossfeed.enabled).toBe(false);
    expect(result.hrtf.enabled).toBe(false);
    expect(result.limiter.enabled).toBe(false);
    expect(result.eqBands.every((band) => band.gainDb === 0)).toBe(true);
    expect(result.eqBands.every((band) => !band.enabled)).toBe(true);
  });

  it("keeps the fixed processing parameters stable across every preset", () => {
    for (const preset of DSP_PRESETS) {
      const result = buildDspSettings(preset.id);
      expect(result.crossfeed.delayMs).toBeCloseTo(0.28);
      expect(result.crossfeed.cutoffHz).toBe(700);
      expect(result.hrtf.outputGainDb).toBe(-6);
      expect(result.limiter.ceilingDb).toBe(-1);
      expect(result.limiter.releaseMs).toBe(80);
    }
  });

  it("interpolates headphone crossfeed through the light, medium and strong anchors", () => {
    expect(buildDspSettings("headphone_daily", 0).crossfeed.amount).toBeCloseTo(0.13);
    expect(buildDspSettings("headphone_daily", 0.5).crossfeed.amount).toBeCloseTo(0.18);
    expect(buildDspSettings("headphone_daily", 1).crossfeed.amount).toBeCloseTo(0.27);
  });

  it("scales vocal and bass curves from 0.6x through 1.0x to 1.4x", () => {
    const quietVocal = buildDspSettings("vocal", 0);
    const normalVocal = buildDspSettings("vocal", 0.5);
    const strongVocal = buildDspSettings("vocal", 1);
    expect(quietVocal.eqBands[2].gainDb).toBeCloseTo(-1.2);
    expect(normalVocal.eqBands[6].gainDb).toBeCloseTo(2.5);
    expect(strongVocal.eqBands[6].gainDb).toBeCloseTo(3.5);
    expect(normalVocal.crossfeed.amount).toBeCloseTo(0.13);

    const bass = buildDspSettings("bass", 1);
    expect(bass.eqBands[0].gainDb).toBeCloseTo(2.8);
    expect(bass.eqBands[1].gainDb).toBeCloseTo(4.2);
    expect(bass.eqBands[2].gainDb).toBeCloseTo(2.8);
    expect(bass.eqBands.slice(3).every((band) => band.gainDb === 0)).toBe(true);
  });

  it("keeps non-spatial HRTF off and enables the limiter for processed presets", () => {
    for (const presetId of ["headphone_daily", "vocal", "bass"] as const) {
      const result = buildDspSettings(presetId);
      expect(result.enabled).toBe(true);
      expect(result.hrtf.enabled).toBe(false);
      expect(result.limiter.enabled).toBe(true);
    }

    const headphone = buildDspSettings("headphone_daily");
    expect(headphone.eqEnabled).toBe(false);
    expect(headphone.crossfeed.enabled).toBe(true);
  });

  it("keeps spatial crossfeed fixed and interpolates only the HRTF mix", () => {
    expect(buildDspSettings("spatial", 0, 0).hrtf.mix).toBeCloseTo(0.3);
    expect(buildDspSettings("spatial", 1, 0.5).hrtf.mix).toBeCloseTo(0.55);
    const dense = buildDspSettings("spatial", 0, 1);
    expect(dense.hrtf.mix).toBeCloseTo(0.72);
    expect(dense.crossfeed.amount).toBeCloseTo(0.18);
    expect(dense.hrtf.outputGainDb).toBe(-6);
    expect(dense.limiter.enabled).toBe(true);
  });

  it("clamps normalized controls and preserves complete authoritative state", () => {
    const result = buildDspControlState("vocal", 3, Number.NaN);
    expect(result.activePresetId).toBe("vocal");
    expect(result.intensity).toBe(1);
    expect(result.spatialAmount).toBe(0.5);
    expect(result.settings).toEqual(buildDspSettings("vocal", 1, 0.5));
    expect(getDspPreset("spatial").label).toBe("空间");
  });
});
