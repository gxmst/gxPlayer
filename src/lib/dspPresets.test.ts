import { describe, expect, it } from "vitest";
import {
  buildDspControlState,
  buildDspSettings,
  clampCustomGains,
  DSP_EQ_MAX_GAIN_DB,
  DSP_PRESETS,
  gainsFromSettings,
  getDspPreset,
  zeroGains,
} from "./dspPresets";
import type { DspPresetId } from "../types";

const FREQUENCIES = [31, 62, 125, 250, 500, 1_000, 2_000, 4_000, 8_000, 16_000];

const VOICING_PRESETS = [
  "warm",
  "bright",
  "classical",
  "electronic",
  "rock",
  "podcast",
  "jazz",
] as const satisfies ReadonlyArray<DspPresetId>;

describe("DSP presets", () => {
  it("defines every preset and always emits a complete 10-band peak EQ", () => {
    expect(DSP_PRESETS.map((preset) => preset.id)).toEqual([
      "bypass",
      "headphone_daily",
      "vocal",
      "bass",
      "spatial",
      "warm",
      "bright",
      "classical",
      "electronic",
      "rock",
      "podcast",
      "jazz",
    ]);

    for (const preset of DSP_PRESETS) {
      const result = buildDspSettings(preset.id);
      expect(result.eqBands).toHaveLength(10);
      expect(result.eqBands.map((band) => band.frequencyHz)).toEqual(FREQUENCIES);
      expect(result.eqBands.every((band) => band.kind === "peak" && band.q === 1)).toBe(true);
      expect(result.eqBands.every((band) => band.enabled === (band.gainDb !== 0))).toBe(true);
    }
  });

  it("keeps every voicing curve restrained and inside the engine gain limit", () => {
    for (const presetId of VOICING_PRESETS) {
      // Full intensity applies the 1.4x ceiling, which is what the engine validates.
      const boosted = buildDspSettings(presetId, 1);
      const peak = Math.max(...boosted.eqBands.map((band) => Math.abs(band.gainDb)));
      expect(peak).toBeLessThanOrEqual(12);
      // Restraint is a product rule, not just a validity one.
      expect(peak).toBeLessThanOrEqual(5);
      expect(boosted.eqEnabled).toBe(true);
      expect(boosted.hrtf.enabled).toBe(false);
      expect(boosted.limiter.enabled).toBe(true);
    }
  });

  it("gives each voicing preset a distinct curve", () => {
    const curves = VOICING_PRESETS.map((presetId) =>
      buildDspSettings(presetId).eqBands.map((band) => band.gainDb).join(","),
    );
    expect(new Set(curves).size).toBe(VOICING_PRESETS.length);
  });

  it("shapes podcast for speech: rumble cut, articulation lifted", () => {
    const podcast = buildDspSettings("podcast", 0.5);
    // 31 Hz and 62 Hz cut, 1 kHz and 2 kHz lifted.
    expect(podcast.eqBands[0].gainDb).toBeLessThan(-2);
    expect(podcast.eqBands[1].gainDb).toBeLessThan(-2);
    expect(podcast.eqBands[5].gainDb).toBeGreaterThan(1);
    expect(podcast.eqBands[6].gainDb).toBeGreaterThan(1);
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
      // Reflections belong to the spatial preset: without a head model in front of
      // them they are an echo, and the host rejects that combination outright.
      expect(result.room.enabled).toBe(false);
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

  it("grows the room alongside the head model, and keeps it a supporting cue", () => {
    const sparse = buildDspSettings("spatial", 0, 0);
    const middle = buildDspSettings("spatial", 0, 0.5);
    const dense = buildDspSettings("spatial", 0, 1);

    for (const result of [sparse, middle, dense]) {
      expect(result.room.enabled).toBe(true);
      // Reflections must stay under the head model, or the space swallows the source.
      expect(result.room.amount).toBeLessThan(result.hrtf.mix);
      // Small-room sizes only: longer pre-delays read as an effect, not a space.
      expect(result.room.size).toBeLessThanOrEqual(0.55);
    }

    // Both cues move together with the one slider the user actually sees.
    expect(sparse.room.amount).toBeCloseTo(0.12);
    expect(middle.room.amount).toBeCloseTo(0.2);
    expect(dense.room.amount).toBeCloseTo(0.3);
    expect(sparse.room.size).toBeLessThan(dense.room.size);
  });

  it("builds a custom curve as authored, without intensity rescaling", () => {
    const gains = [1, -2, 3, 0, 0, 0, 0, 0, 4, 0];
    // Same curve at both intensity extremes: a hand-set gain must not be rescaled.
    for (const intensity of [0, 0.5, 1]) {
      const result = buildDspSettings("custom", intensity, 0.5, gains);
      expect(result.eqBands.map((band) => band.gainDb)).toEqual(gains);
      expect(result.eqEnabled).toBe(true);
      expect(result.hrtf.enabled).toBe(false);
      expect(result.limiter.enabled).toBe(true);
    }
  });

  it("clamps a custom curve to the shape the engine accepts", () => {
    // Too short, too long, non-finite and out-of-range all normalize rather than throw.
    expect(clampCustomGains([1, 2])).toEqual([1, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(clampCustomGains(new Array(14).fill(1))).toHaveLength(10);
    expect(clampCustomGains([Number.NaN, Number.POSITIVE_INFINITY])[0]).toBe(0);
    expect(clampCustomGains([99, -99])[0]).toBe(DSP_EQ_MAX_GAIN_DB);
    expect(clampCustomGains([99, -99])[1]).toBe(-DSP_EQ_MAX_GAIN_DB);

    const built = buildDspSettings("custom", 0.5, 0.5, [99, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(built.eqBands[0].gainDb).toBe(DSP_EQ_MAX_GAIN_DB);
    expect(built.eqBands).toHaveLength(10);
  });

  it("names the custom curve instead of falling back to the first preset", () => {
    // `custom` is not on the shelf, so a plain list lookup would return 原声.
    expect(getDspPreset("custom").label).toBe("自定义");
    expect(DSP_PRESETS.map((preset) => preset.id)).not.toContain("custom");
  });

  it("reads a curve back off any preset for seeding the editor", () => {
    const bass = buildDspSettings("bass", 1);
    expect(gainsFromSettings(bass)).toEqual(bass.eqBands.map((band) => band.gainDb));
    expect(gainsFromSettings(buildDspSettings("bypass"))).toEqual(zeroGains());
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
