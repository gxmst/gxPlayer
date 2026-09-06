// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { useState, type ComponentProps } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerBar } from "./PlayerBar";

afterEach(cleanup);
function setup() {
  const props: ComponentProps<typeof PlayerBar> = {
    title: "Track", artist: "Artist", cover: <span>Cover</span>, playing: false, canPlay: true, canSeek: true,
    position: 10, duration: 100, mode: "sequential", volume: 0.7, adjustingVolume: false,
    queueOpen: true, queueCount: 2, presetLabel: "Default", details: <span>Audio details</span>,
    onOpenPlayer: vi.fn(), onPlayPause: vi.fn(), onPrevious: vi.fn(), onNext: vi.fn(), onCycleMode: vi.fn(),
    onSeekPreview: vi.fn(), onSeek: vi.fn(), onVolumePreview: vi.fn(), onVolumeCommit: vi.fn(),
    onMute: vi.fn(), onQueue: vi.fn(), onPreset: vi.fn(), onMini: vi.fn(),
  };
  function Harness() {
    const [position, setPosition] = useState(props.position);
    const [volume, setVolume] = useState(props.volume);
    return <PlayerBar {...props} position={position} volume={volume}
      onSeekPreview={(value) => { props.onSeekPreview(value); setPosition(value); }}
      onVolumePreview={(value) => { props.onVolumePreview(value); setVolume(value); }} />;
  }
  render(<Harness />);
  return props;
}

describe("PlayerBar", () => {
  it("commits the preview when seeking is interrupted, without committing again on blur", () => {
    const props = setup();
    const seek = screen.getByRole("slider", { name: "播放进度" });
    fireEvent.change(seek, { target: { value: "42" } });
    expect(props.onSeekPreview).toHaveBeenCalledWith(42);
    expect(props.onSeek).not.toHaveBeenCalled();
    fireEvent.pointerCancel(seek);
    fireEvent.blur(seek);
    expect(props.onSeek).toHaveBeenCalledExactlyOnceWith(42);
    const volume = screen.getByRole("slider", { name: "音量" });
    fireEvent.change(volume, { target: { value: "0.5" } });
    fireEvent.keyUp(volume, { key: "ArrowRight" });
    expect(props.onVolumeCommit).toHaveBeenCalledWith(0.5);
  });

  it("closes only the foreground options on Escape and restores focus", () => {
    setup();
    const backgroundEscape = vi.fn();
    document.addEventListener("keydown", backgroundEscape);
    try {
      const opener = screen.getByRole("button", { name: "播放选项" });
      fireEvent.click(opener);
      const close = screen.getByRole("button", { name: "关闭播放选项" });
      expect(close).toHaveFocus();
      fireEvent.keyDown(close, { key: "Escape" });
      expect(screen.queryByRole("dialog", { name: "播放选项" })).not.toBeInTheDocument();
      expect(opener).toHaveFocus();
      expect(backgroundEscape).not.toHaveBeenCalled();
      fireEvent.click(opener);
      fireEvent.pointerDown(document.body);
      expect(screen.queryByRole("dialog", { name: "播放选项" })).not.toBeInTheDocument();
    } finally { document.removeEventListener("keydown", backgroundEscape); }
  });
});
