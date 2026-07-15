import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
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
    onChange(buildDspControlState(presetId, value.intensity, value.spatialAmount));
  };

  const changeIntensity = (intensity: number) => {
    releaseAb();
    onChange(buildDspControlState(value.activePresetId, intensity, value.spatialAmount));
  };

  const changeSpatialAmount = (spatialAmount: number) => {
    releaseAb();
    onChange(buildDspControlState(value.activePresetId, value.intensity, spatialAmount));
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

  const beginAb = () => {
    if (disabled || value.activePresetId === "bypass" || abHeldRef.current) return;
    abHeldRef.current = true;
    setAbHeld(true);
    onAbDryChangeRef.current(true);
  };

  const onAbPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    beginAb();
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
              <span><strong>强度</strong><output>{controlLabel(value.intensity, ["轻", "标准", "强"])}</output></span>
              <input
                type="range"
                min="0"
                max="1"
                step="0.01"
                value={value.intensity}
                disabled={disabled}
                aria-label="强度"
                aria-valuetext={controlLabel(value.intensity, ["轻", "标准", "强"])}
                onChange={(event) => changeIntensity(Number(event.target.value))}
              />
            </label>
          )}
          {showSpatialAmount && (
            <label className="dsp-slider-row">
              <span><strong>空间感</strong><output>{controlLabel(value.spatialAmount, ["轻", "中", "浓"])}</output></span>
              <input
                type="range"
                min="0"
                max="1"
                step="0.01"
                value={value.spatialAmount}
                disabled={disabled}
                aria-label="空间感"
                aria-valuetext={controlLabel(value.spatialAmount, ["轻", "中", "浓"])}
                onChange={(event) => changeSpatialAmount(Number(event.target.value))}
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
            onLostPointerCapture={releaseAb}
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
