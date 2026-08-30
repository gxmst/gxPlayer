import { useEffect, useId, useRef, useState } from "react";
import type { ChangeEvent } from "react";
import type { CustomEqPreset } from "../../types";
import {
  clampCustomGains,
  DSP_EQ_FREQUENCIES,
  DSP_EQ_MAX_GAIN_DB,
  zeroGains,
} from "../../lib/dspPresets";
import "./CustomEqEditor.css";

export type CustomEqEditorProps = {
  /** Curve currently applied to the engine, low band to high. */
  gains: readonly number[];
  onGainsChange: (gains: number[]) => void;
  savedPresets: readonly CustomEqPreset[];
  onSavePreset: (name: string, gains: readonly number[]) => void;
  onDeletePreset: (name: string) => void;
  onApplyPreset: (gains: readonly number[]) => void;
  disabled?: boolean;
  /** Name of the saved preset the current curve matches exactly, if any. */
  matchingPresetName?: string | null;
};

const GAIN_STEP = 0.5;
const CURVE_WIDTH = 320;
const CURVE_HEIGHT = 96;

function formatFrequency(hz: number): string {
  return hz >= 1_000 ? `${hz / 1_000}k` : String(hz);
}

function formatGain(db: number): string {
  if (Math.abs(db) < 0.05) return "0.0";
  return `${db > 0 ? "+" : ""}${db.toFixed(1)}`;
}

/** Map a curve to an SVG polyline across the plot area. */
function curvePoints(gains: readonly number[]): string {
  if (gains.length < 2) return "";
  const stepX = CURVE_WIDTH / (gains.length - 1);
  return gains
    .map((gain, index) => {
      const clamped = Math.min(DSP_EQ_MAX_GAIN_DB, Math.max(-DSP_EQ_MAX_GAIN_DB, gain));
      const y = CURVE_HEIGHT / 2 - (clamped / DSP_EQ_MAX_GAIN_DB) * (CURVE_HEIGHT / 2);
      return `${(index * stepX).toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");
}

export function CustomEqEditor({
  gains,
  onGainsChange,
  savedPresets,
  onSavePreset,
  onDeletePreset,
  onApplyPreset,
  disabled = false,
  matchingPresetName = null,
}: CustomEqEditorProps) {
  const idPrefix = useId();
  const [draftName, setDraftName] = useState("");
  // Slider drags emit continuously; keep them local and commit on release so the
  // engine is not rebuilt for every intermediate value.
  const [draftGains, setDraftGains] = useState<number[]>(() => clampCustomGains(gains));
  const draftGainsRef = useRef(draftGains);
  draftGainsRef.current = draftGains;
  const dirtyRef = useRef(false);
  const onGainsChangeRef = useRef(onGainsChange);
  onGainsChangeRef.current = onGainsChange;

  // Follow external changes (preset switch, restored preferences) unless the user is
  // mid-edit, which would otherwise yank the slider out from under them.
  useEffect(() => {
    if (dirtyRef.current) return;
    setDraftGains(clampCustomGains(gains));
  }, [gains]);

  const commit = () => {
    if (!dirtyRef.current) return;
    dirtyRef.current = false;
    onGainsChangeRef.current(clampCustomGains(draftGainsRef.current));
  };

  useEffect(() => {
    // A pointer released outside the slider still ends the drag.
    const finish = () => commit();
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
    return () => {
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      commit();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const updateBand = (index: number) => (event: ChangeEvent<HTMLInputElement>) => {
    const next = Number(event.currentTarget.value);
    if (!Number.isFinite(next)) return;
    dirtyRef.current = true;
    setDraftGains((current) => {
      const updated = [...current];
      updated[index] = next;
      return updated;
    });
  };

  const reset = () => {
    dirtyRef.current = false;
    const flat = zeroGains();
    setDraftGains(flat);
    onGainsChangeRef.current(flat);
  };

  const trimmedName = draftName.trim();
  const willReplace = savedPresets.some((preset) => preset.name === trimmedName);

  return (
    <div className="custom-eq-editor">
      <div className="custom-eq-plot-row">
        <svg
          className="custom-eq-plot"
          viewBox={`0 0 ${CURVE_WIDTH} ${CURVE_HEIGHT}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={`当前均衡曲线：${draftGains
            .map((gain, index) => `${formatFrequency(DSP_EQ_FREQUENCIES[index])} ${formatGain(gain)} dB`)
            .join("，")}`}
        >
          <line
            className="custom-eq-zero-line"
            x1="0"
            y1={CURVE_HEIGHT / 2}
            x2={CURVE_WIDTH}
            y2={CURVE_HEIGHT / 2}
          />
          <polyline className="custom-eq-curve" points={curvePoints(draftGains)} />
        </svg>
        <div className="custom-eq-scale" aria-hidden="true">
          <span>+{DSP_EQ_MAX_GAIN_DB}</span>
          <span>0</span>
          <span>−{DSP_EQ_MAX_GAIN_DB}</span>
        </div>
      </div>

      <div className="custom-eq-bands">
        {DSP_EQ_FREQUENCIES.map((frequency, index) => (
          <label className="custom-eq-band" key={frequency}>
            <input
              type="range"
              className="custom-eq-range"
              min={-DSP_EQ_MAX_GAIN_DB}
              max={DSP_EQ_MAX_GAIN_DB}
              step={GAIN_STEP}
              value={draftGains[index]}
              disabled={disabled}
              aria-label={`${formatFrequency(frequency)} Hz`}
              aria-valuetext={`${formatGain(draftGains[index])} dB`}
              onChange={updateBand(index)}
              onPointerUp={commit}
              onKeyUp={commit}
              onBlur={commit}
            />
            <span className="custom-eq-band-gain">{formatGain(draftGains[index])}</span>
            <span className="custom-eq-band-frequency">{formatFrequency(frequency)}</span>
          </label>
        ))}
      </div>

      <div className="custom-eq-actions">
        <label className="custom-eq-name">
          <span className="custom-eq-sr-only" id={`${idPrefix}-name-label`}>
            预设名称
          </span>
          <input
            type="text"
            placeholder="给这条曲线起个名字"
            maxLength={40}
            autoComplete="off"
            spellCheck={false}
            value={draftName}
            disabled={disabled}
            aria-labelledby={`${idPrefix}-name-label`}
            onChange={(event) => setDraftName(event.currentTarget.value)}
          />
        </label>
        <button
          type="button"
          className="primary"
          disabled={disabled || !trimmedName}
          onClick={() => {
            onSavePreset(trimmedName, clampCustomGains(draftGainsRef.current));
            setDraftName("");
          }}
        >
          {willReplace ? "覆盖保存" : "保存预设"}
        </button>
        <button type="button" disabled={disabled} onClick={reset}>
          归零
        </button>
      </div>

      {savedPresets.length > 0 && (
        <ul className="custom-eq-saved" aria-label="已保存的预设">
          {savedPresets.map((preset) => (
            <li key={preset.name} className={preset.name === matchingPresetName ? "is-active" : ""}>
              <button
                type="button"
                className="custom-eq-saved-apply"
                disabled={disabled}
                onClick={() => onApplyPreset(preset.gainsDb)}
              >
                {preset.name}
              </button>
              <button
                type="button"
                className="custom-eq-saved-delete"
                disabled={disabled}
                aria-label={`删除预设 ${preset.name}`}
                onClick={() => onDeletePreset(preset.name)}
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
