import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { ChangeEvent, CSSProperties, KeyboardEvent, PointerEvent } from "react";
import type { DspControlState, DspPresetId } from "../../types";
import {
  buildDspControlState,
  DSP_AB_LABEL,
  DSP_PRESETS,
  DSP_SYSTEM_EFFECTS_HINT,
  getDspPreset,
} from "../../lib/dspPresets";
import "./DspPresetControls.css";

export type DspPresetControlsProps = {
  value: DspControlState;
  onChange: (next: DspControlState) => void;
  onAbDryChange: (active: boolean) => void;
  disabled?: boolean;
  compact?: boolean;
  showSystemEffectsHint?: boolean;
};

type RangeFillStyle = CSSProperties & { "--fill": string };

const RANGE_ADJUSTMENT_KEYS = new Set([
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "End",
  "Home",
  "PageDown",
  "PageUp",
]);

function rangeFillStyle(value: number): RangeFillStyle {
  const percent = Math.round(Math.min(1, Math.max(0, value)) * 10_000) / 100;
  return { "--fill": `${percent}%` };
}

function controlLabel(value: number, labels: readonly [string, string, string]): string {
  if (value <= 0.01) return labels[0];
  if (Math.abs(value - 0.5) <= 0.01) return labels[1];
  if (value >= 0.99) return labels[2];
  return `${Math.round(value * 100)}%`;
}

export function DspPresetControls({
  value,
  onChange,
  onAbDryChange,
  disabled = false,
  compact = false,
  showSystemEffectsHint = true,
}: DspPresetControlsProps) {
  const idPrefix = useId();
  const abHeldRef = useRef(false);
  const presetButtonRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [abHeld, setAbHeld] = useState(false);
  const [draftIntensity, setDraftIntensity] = useState(value.intensity);
  const [draftSpatialAmount, setDraftSpatialAmount] = useState(value.spatialAmount);
  const draftIntensityRef = useRef(value.intensity);
  const draftSpatialAmountRef = useRef(value.spatialAmount);
  const intensityDirtyRef = useRef(false);
  const spatialAmountDirtyRef = useRef(false);
  const valueRef = useRef(value);
  valueRef.current = value;
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const onAbDryChangeRef = useRef(onAbDryChange);
  onAbDryChangeRef.current = onAbDryChange;
  const activePreset = getDspPreset(value.activePresetId);
  const showIntensity = value.activePresetId === "headphone_daily"
    || value.activePresetId === "vocal"
    || value.activePresetId === "bass";
  const showSpatialAmount = value.activePresetId === "spatial";
  const showAdjustments = value.activePresetId !== "bypass";

  const releaseAb = useCallback(() => {
    if (!abHeldRef.current) return;
    abHeldRef.current = false;
    setAbHeld(false);
    onAbDryChangeRef.current(false);
  }, []);

  useLayoutEffect(() => {
    draftIntensityRef.current = value.intensity;
    draftSpatialAmountRef.current = value.spatialAmount;
    intensityDirtyRef.current = false;
    spatialAmountDirtyRef.current = false;
    setDraftIntensity(value.intensity);
    setDraftSpatialAmount(value.spatialAmount);
  }, [disabled, value.activePresetId, value.intensity, value.spatialAmount]);

  useEffect(() => {
    const onVisibilityChange = () => {
      if (document.visibilityState === "hidden") releaseAb();
    };
    window.addEventListener("blur", releaseAb);
    window.addEventListener("pointerup", releaseAb);
    window.addEventListener("pointercancel", releaseAb);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.removeEventListener("blur", releaseAb);
      window.removeEventListener("pointerup", releaseAb);
      window.removeEventListener("pointercancel", releaseAb);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      if (abHeldRef.current) {
        abHeldRef.current = false;
        onAbDryChangeRef.current(false);
      }
    };
  }, [releaseAb]);

  useEffect(() => {
    // A cold-path control change and the momentary hot-path comparison must
    // never remain active at the same time, including changes made elsewhere.
    releaseAb();
  }, [disabled, releaseAb, value.activePresetId, value.intensity, value.spatialAmount]);

  const choosePreset = (presetId: DspPresetId) => {
    if (disabled || presetId === value.activePresetId) return;
    releaseAb();
    intensityDirtyRef.current = false;
    spatialAmountDirtyRef.current = false;
    onChangeRef.current(
      buildDspControlState(
        presetId,
        draftIntensityRef.current,
        draftSpatialAmountRef.current,
      ),
    );
  };

  const updateIntensityDraft = (event: ChangeEvent<HTMLInputElement>) => {
    const intensity = Number(event.currentTarget.value);
    if (!Number.isFinite(intensity)) return;
    releaseAb();
    draftIntensityRef.current = intensity;
    intensityDirtyRef.current = intensity !== valueRef.current.intensity;
    setDraftIntensity(intensity);
  };

  const updateSpatialAmountDraft = (event: ChangeEvent<HTMLInputElement>) => {
    const spatialAmount = Number(event.currentTarget.value);
    if (!Number.isFinite(spatialAmount)) return;
    releaseAb();
    draftSpatialAmountRef.current = spatialAmount;
    spatialAmountDirtyRef.current = spatialAmount !== valueRef.current.spatialAmount;
    setDraftSpatialAmount(spatialAmount);
  };

  const commitIntensity = useCallback(() => {
    if (!intensityDirtyRef.current) return;
    intensityDirtyRef.current = false;
    const intensity = draftIntensityRef.current;
    if (intensity === valueRef.current.intensity) return;
    onChangeRef.current(
      buildDspControlState(
        valueRef.current.activePresetId,
        intensity,
        draftSpatialAmountRef.current,
      ),
    );
  }, []);

  const commitSpatialAmount = useCallback(() => {
    if (!spatialAmountDirtyRef.current) return;
    spatialAmountDirtyRef.current = false;
    const spatialAmount = draftSpatialAmountRef.current;
    if (spatialAmount === valueRef.current.spatialAmount) return;
    onChangeRef.current(
      buildDspControlState(
        valueRef.current.activePresetId,
        draftIntensityRef.current,
        spatialAmount,
      ),
    );
  }, []);

  useEffect(() => {
    const commitDrafts = () => {
      commitIntensity();
      commitSpatialAmount();
    };
    window.addEventListener("blur", commitDrafts);
    window.addEventListener("pointerup", commitDrafts);
    window.addEventListener("pointercancel", commitDrafts);
    return () => {
      window.removeEventListener("blur", commitDrafts);
      window.removeEventListener("pointerup", commitDrafts);
      window.removeEventListener("pointercancel", commitDrafts);
    };
  }, [commitIntensity, commitSpatialAmount]);

  const beginRangePointer = (event: PointerEvent<HTMLInputElement>) => {
    if (event.button !== 0) return;
    releaseAb();
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // Native range controls may already own capture; their pointerup still commits the draft.
    }
  };

  const beginRangeKey = (event: KeyboardEvent<HTMLInputElement>) => {
    if (RANGE_ADJUSTMENT_KEYS.has(event.key)) releaseAb();
  };

  const commitIntensityOnKeyUp = (event: KeyboardEvent<HTMLInputElement>) => {
    if (RANGE_ADJUSTMENT_KEYS.has(event.key)) commitIntensity();
  };

  const commitSpatialAmountOnKeyUp = (event: KeyboardEvent<HTMLInputElement>) => {
    if (RANGE_ADJUSTMENT_KEYS.has(event.key)) commitSpatialAmount();
  };

  const onPresetKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex: number | undefined;
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        nextIndex = (index + 1) % DSP_PRESETS.length;
        break;
      case "ArrowLeft":
      case "ArrowUp":
        nextIndex = (index - 1 + DSP_PRESETS.length) % DSP_PRESETS.length;
        break;
      case "Home":
        nextIndex = 0;
        break;
      case "End":
        nextIndex = DSP_PRESETS.length - 1;
        break;
      default:
        return;
    }

    event.preventDefault();
    event.stopPropagation();
    presetButtonRefs.current[nextIndex]?.focus();
    choosePreset(DSP_PRESETS[nextIndex].id);
  };

  const beginAb = (): boolean => {
    if (disabled || value.activePresetId === "bypass" || abHeldRef.current) return false;
    abHeldRef.current = true;
    setAbHeld(true);
    onAbDryChangeRef.current(true);
    return true;
  };

  const onAbPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0) return;
    if (!beginAb()) return;
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // The global pointerup/cancel/blur listeners still guarantee release.
    }
  };

  const onAbKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== " " && event.key !== "Enter") return;
    event.preventDefault();
    event.stopPropagation();
    beginAb();
  };

  const onAbKeyUp = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== " " && event.key !== "Enter") return;
    event.preventDefault();
    event.stopPropagation();
    releaseAb();
  };

  return (
    <section className={`dsp-preset-controls ${compact ? "is-compact" : ""}`} aria-label="音效预设">
      <div className="dsp-preset-grid" role="radiogroup" aria-label="音效预设">
        {DSP_PRESETS.map((preset, index) => (
          <button
            type="button"
            role="radio"
            aria-checked={preset.id === value.activePresetId}
            aria-label={preset.label}
            aria-describedby={!compact ? `${idPrefix}-${preset.id}-description` : undefined}
            className={preset.id === value.activePresetId ? "active" : ""}
            disabled={disabled}
            tabIndex={preset.id === value.activePresetId ? 0 : -1}
            key={preset.id}
            ref={(node) => {
              presetButtonRefs.current[index] = node;
            }}
            onClick={() => choosePreset(preset.id)}
            onKeyDown={(event) => onPresetKeyDown(event, index)}
          >
            <strong>{preset.label}</strong>
            {!compact && <small id={`${idPrefix}-${preset.id}-description`}>{preset.description}</small>}
          </button>
        ))}
      </div>

      <div className="dsp-preset-summary" aria-live="polite" aria-atomic="true">
        <strong>{activePreset.label}</strong>
        <span>{activePreset.description}</span>
      </div>

      {showAdjustments && (
        <div className="dsp-adjustments">
          {showIntensity && (
            <label className="dsp-slider-row">
              <span><strong>强度</strong><output>{controlLabel(draftIntensity, ["轻", "标准", "强"])}</output></span>
              <input
                className="dsp-range"
                type="range"
                min="0"
                max="1"
                step="0.01"
                value={draftIntensity}
                style={rangeFillStyle(draftIntensity)}
                disabled={disabled}
                aria-label="强度"
                aria-valuetext={controlLabel(draftIntensity, ["轻", "标准", "强"])}
                onChange={updateIntensityDraft}
                onPointerDown={beginRangePointer}
                onPointerUp={commitIntensity}
                onPointerCancel={commitIntensity}
                onLostPointerCapture={commitIntensity}
                onKeyDown={beginRangeKey}
                onKeyUp={commitIntensityOnKeyUp}
                onBlur={commitIntensity}
              />
            </label>
          )}
          {showSpatialAmount && (
            <label className="dsp-slider-row">
              <span><strong>空间感</strong><output>{controlLabel(draftSpatialAmount, ["轻", "中", "浓"])}</output></span>
              <input
                className="dsp-range"
                type="range"
                min="0"
                max="1"
                step="0.01"
                value={draftSpatialAmount}
                style={rangeFillStyle(draftSpatialAmount)}
                disabled={disabled}
                aria-label="空间感"
                aria-valuetext={controlLabel(draftSpatialAmount, ["轻", "中", "浓"])}
                onChange={updateSpatialAmountDraft}
                onPointerDown={beginRangePointer}
                onPointerUp={commitSpatialAmount}
                onPointerCancel={commitSpatialAmount}
                onLostPointerCapture={commitSpatialAmount}
                onKeyDown={beginRangeKey}
                onKeyUp={commitSpatialAmountOnKeyUp}
                onBlur={commitSpatialAmount}
              />
            </label>
          )}
          <button
            type="button"
            className="dsp-ab-button"
            disabled={disabled}
            aria-pressed={abHeld}
            aria-describedby={`${idPrefix}-ab-description`}
            onPointerDown={onAbPointerDown}
            onPointerUp={releaseAb}
            onPointerCancel={releaseAb}
            onKeyDown={onAbKeyDown}
            onKeyUp={onAbKeyUp}
            onBlur={releaseAb}
            onContextMenu={(event) => event.preventDefault()}
          >
            {DSP_AB_LABEL}
          </button>
          <span id={`${idPrefix}-ab-description`} className="dsp-sr-only">
            持续按住时生效，松开即恢复当前音效预设。
          </span>
        </div>
      )}

      {showSystemEffectsHint && <p className="dsp-system-effects-hint">{DSP_SYSTEM_EFFECTS_HINT}</p>}
    </section>
  );
}
