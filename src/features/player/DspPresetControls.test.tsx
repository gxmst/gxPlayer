// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { buildDspControlState, DSP_AB_LABEL, DSP_SYSTEM_EFFECTS_HINT } from "../../lib/dspPresets";
import { DspPresetControls } from "./DspPresetControls";

afterEach(() => cleanup());

describe("DspPresetControls", () => {
  it("renders the five v1 preset choices and exact guidance copy", () => {
    render(
      <DspPresetControls
        value={buildDspControlState("bypass")}
        onChange={vi.fn()}
        onAbDryChange={vi.fn()}
      />,
    );

    expect(screen.getAllByRole("radio")).toHaveLength(5);
    for (const name of ["原声", "耳机日常", "人声", "低音", "空间"]) {
      expect(screen.getByRole("radio", { name: new RegExp(name) })).toBeInTheDocument();
    }
    expect(screen.getByText(DSP_SYSTEM_EFFECTS_HINT)).toBeInTheDocument();
    expect(screen.queryByRole("slider")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: DSP_AB_LABEL })).not.toBeInTheDocument();
  });

  it("emits a complete control state when a preset is selected", () => {
    const onChange = vi.fn();
    render(
      <DspPresetControls
        value={buildDspControlState("bypass", 0.2, 0.8)}
        onChange={onChange}
        onAbDryChange={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /人声/ }));
    expect(onChange).toHaveBeenCalledWith(buildDspControlState("vocal", 0.2, 0.8));
    expect(onChange.mock.calls[0][0].settings.eqBands).toHaveLength(10);
  });

  it("uses roving focus and arrow keys for the radio group", () => {
    const onChange = vi.fn();
    render(
      <DspPresetControls
        value={buildDspControlState("vocal", 0.2, 0.8)}
        onChange={onChange}
        onAbDryChange={vi.fn()}
      />,
    );

    const vocal = screen.getByRole("radio", { name: "人声" });
    const bass = screen.getByRole("radio", { name: "低音" });
    expect(vocal).toHaveAttribute("tabindex", "0");
    expect(bass).toHaveAttribute("tabindex", "-1");

    vocal.focus();
    fireEvent.keyDown(vocal, { key: "ArrowRight" });
    expect(bass).toHaveFocus();
    expect(onChange).toHaveBeenLastCalledWith(buildDspControlState("bass", 0.2, 0.8));

    fireEvent.keyDown(bass, { key: "End" });
    expect(screen.getByRole("radio", { name: "空间" })).toHaveFocus();
    expect(onChange).toHaveBeenLastCalledWith(buildDspControlState("spatial", 0.2, 0.8));
  });

  it("shows strength only for headphone, vocal and bass presets", () => {
    const { rerender } = render(
      <DspPresetControls
        value={buildDspControlState("headphone_daily")}
        onChange={vi.fn()}
        onAbDryChange={vi.fn()}
      />,
    );
    expect(screen.getByRole("slider", { name: "强度" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "强度" })).toHaveAttribute("aria-valuetext", "标准");
    expect(screen.queryByRole("slider", { name: "空间感" })).not.toBeInTheDocument();

    rerender(
      <DspPresetControls
        value={buildDspControlState("spatial")}
        onChange={vi.fn()}
        onAbDryChange={vi.fn()}
      />,
    );
    expect(screen.queryByRole("slider", { name: "强度" })).not.toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "空间感" })).toBeInTheDocument();
    expect(screen.getAllByText("固定前方 ±30° 音箱感，可能偏闷；建议不与系统杜比耳机虚拟化同时开。").length).toBeGreaterThan(0);
  });

  it("rebuilds the full settings when a slider changes", () => {
    const onChange = vi.fn();
    render(
      <DspPresetControls
        value={buildDspControlState("bass", 0.5, 0.5)}
        onChange={onChange}
        onAbDryChange={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByRole("slider", { name: "强度" }), { target: { value: "1" } });
    expect(onChange).toHaveBeenCalledWith(buildDspControlState("bass", 1, 0.5));
  });

  it("uses a separate momentary A/B path for pointer input", () => {
    const onChange = vi.fn();
    const onAbDryChange = vi.fn();
    render(
      <DspPresetControls
        value={buildDspControlState("vocal")}
        onChange={onChange}
        onAbDryChange={onAbDryChange}
      />,
    );
    const button = screen.getByRole("button", { name: DSP_AB_LABEL });

    fireEvent.pointerDown(button, { button: 0, pointerId: 7 });
    expect(button).toHaveAttribute("aria-pressed", "true");
    fireEvent.pointerUp(button, { pointerId: 7 });
    expect(button).toHaveAttribute("aria-pressed", "false");

    expect(onAbDryChange.mock.calls.map(([active]) => active)).toEqual([true, false]);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("releases keyboard A/B on keyup and window blur without duplicate calls", () => {
    const onAbDryChange = vi.fn();
    render(
      <DspPresetControls
        value={buildDspControlState("spatial")}
        onChange={vi.fn()}
        onAbDryChange={onAbDryChange}
      />,
    );
    const button = screen.getByRole("button", { name: DSP_AB_LABEL });

    fireEvent.keyDown(button, { key: " " });
    fireEvent.keyUp(button, { key: " " });
    fireEvent.keyDown(button, { key: "Enter" });
    fireEvent.blur(window);
    fireEvent.keyUp(button, { key: "Enter" });

    expect(onAbDryChange.mock.calls.map(([active]) => active)).toEqual([true, false, true, false]);
  });

  it("releases A/B when a cold-path value changes outside this control", () => {
    const onAbDryChange = vi.fn();
    const { rerender } = render(
      <DspPresetControls
        value={buildDspControlState("vocal")}
        onChange={vi.fn()}
        onAbDryChange={onAbDryChange}
      />,
    );
    const button = screen.getByRole("button", { name: DSP_AB_LABEL });
    fireEvent.pointerDown(button, { button: 0, pointerId: 11 });

    rerender(
      <DspPresetControls
        value={buildDspControlState("bass")}
        onChange={vi.fn()}
        onAbDryChange={onAbDryChange}
      />,
    );

    expect(onAbDryChange.mock.calls.map(([active]) => active)).toEqual([true, false]);
  });

  it("releases A/B when the page is hidden or the control unmounts", () => {
    const onAbDryChange = vi.fn();
    const { unmount } = render(
      <DspPresetControls
        value={buildDspControlState("spatial")}
        onChange={vi.fn()}
        onAbDryChange={onAbDryChange}
      />,
    );
    const button = screen.getByRole("button", { name: DSP_AB_LABEL });
    const visibilityState = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");

    fireEvent.pointerDown(button, { button: 0, pointerId: 13 });
    fireEvent(document, new Event("visibilitychange"));
    fireEvent.pointerDown(button, { button: 0, pointerId: 14 });
    unmount();
    visibilityState.mockRestore();

    expect(onAbDryChange.mock.calls.map(([active]) => active)).toEqual([true, false, true, false]);
  });
});
