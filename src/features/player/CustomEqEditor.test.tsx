// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DSP_EQ_FREQUENCIES, DSP_EQ_MAX_GAIN_DB, zeroGains } from "../../lib/dspPresets";
import { CustomEqEditor } from "./CustomEqEditor";

afterEach(() => cleanup());

type EditorProps = Parameters<typeof CustomEqEditor>[0];

function renderEditor(overrides: Partial<EditorProps> = {}) {
  // Keep the spies concretely typed: spreading overrides into the literal would widen
  // them to the prop signature and hide `.mock` from the assertions below.
  const onGainsChange = vi.fn();
  const onSavePreset = vi.fn();
  const onDeletePreset = vi.fn();
  const onApplyPreset = vi.fn();
  const props: EditorProps = {
    gains: zeroGains(),
    savedPresets: [],
    onGainsChange,
    onSavePreset,
    onDeletePreset,
    onApplyPreset,
    ...overrides,
  };
  render(<CustomEqEditor {...props} />);
  return { onGainsChange, onSavePreset, onDeletePreset, onApplyPreset };
}

describe("CustomEqEditor", () => {
  it("exposes one labelled slider per product EQ band", () => {
    renderEditor();
    const sliders = screen.getAllByRole("slider");
    expect(sliders).toHaveLength(DSP_EQ_FREQUENCIES.length);
    expect(screen.getByRole("slider", { name: "31 Hz" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "16k Hz" })).toBeInTheDocument();
    // The engine limit has to be the control limit, or a drag could build a curve
    // that validate_product would reject.
    for (const slider of sliders) {
      expect(slider).toHaveAttribute("min", String(-DSP_EQ_MAX_GAIN_DB));
      expect(slider).toHaveAttribute("max", String(DSP_EQ_MAX_GAIN_DB));
    }
  });

  it("keeps a drag local and commits the finished curve once", () => {
    const { onGainsChange } = renderEditor();
    const band = screen.getByRole("slider", { name: "125 Hz" });

    fireEvent.change(band, { target: { value: "3" } });
    fireEvent.change(band, { target: { value: "4.5" } });
    expect(onGainsChange).not.toHaveBeenCalled();

    fireEvent.pointerUp(band);
    expect(onGainsChange).toHaveBeenCalledTimes(1);
    const committed = onGainsChange.mock.calls[0][0] as number[];
    expect(committed[2]).toBe(4.5);
    expect(committed.filter((gain) => gain !== 0)).toHaveLength(1);
  });

  it("saves under a trimmed name and reports an existing name as a replacement", () => {
    const { onSavePreset } = renderEditor({
      savedPresets: [{ name: "夜听", gainsDb: zeroGains() }],
    });

    const nameField = screen.getByRole("textbox");
    fireEvent.change(nameField, { target: { value: "  通勤  " } });
    expect(screen.getByRole("button", { name: "保存预设" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "保存预设" }));
    expect(onSavePreset).toHaveBeenCalledWith("通勤", zeroGains());

    // An existing name switches the button to an explicit overwrite.
    fireEvent.change(nameField, { target: { value: "夜听" } });
    expect(screen.getByRole("button", { name: "覆盖保存" })).toBeInTheDocument();
  });

  it("requires a name before saving", () => {
    renderEditor();
    expect(screen.getByRole("button", { name: "保存预设" })).toBeDisabled();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "   " } });
    expect(screen.getByRole("button", { name: "保存预设" })).toBeDisabled();
  });

  it("applies and deletes saved presets, and marks the one currently in use", () => {
    const active = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const { onApplyPreset, onDeletePreset } = renderEditor({
      gains: active,
      savedPresets: [
        { name: "低音", gainsDb: active },
        { name: "人声", gainsDb: zeroGains() },
      ],
      matchingPresetName: "低音",
    });

    fireEvent.click(screen.getByRole("button", { name: "人声" }));
    expect(onApplyPreset).toHaveBeenCalledWith(zeroGains());

    fireEvent.click(screen.getByRole("button", { name: "删除预设 低音" }));
    expect(onDeletePreset).toHaveBeenCalledWith("低音");
  });

  it("normalizes a stored curve that does not match the band count", () => {
    // Preferences can hold a curve written by another version; render must not break.
    renderEditor({ gains: [1, Number.NaN, 99] });
    expect(screen.getByRole("slider", { name: "31 Hz" })).toHaveValue("1");
    expect(screen.getByRole("slider", { name: "62 Hz" })).toHaveValue("0");
    expect(screen.getByRole("slider", { name: "125 Hz" })).toHaveValue(String(DSP_EQ_MAX_GAIN_DB));
    expect(screen.getByRole("slider", { name: "16k Hz" })).toHaveValue("0");
  });

  it("zeroes the curve on demand", () => {
    const { onGainsChange } = renderEditor({ gains: [4, 4, 4, 4, 4, 4, 4, 4, 4, 4] });
    fireEvent.click(screen.getByRole("button", { name: "归零" }));
    expect(onGainsChange).toHaveBeenCalledWith(zeroGains());
  });
});
