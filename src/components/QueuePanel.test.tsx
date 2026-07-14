// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { QueuePanel } from "./QueuePanel";

afterEach(cleanup);

function renderPanel(overrides: Partial<Parameters<typeof QueuePanel>[0]> = {}) {
  const props: Parameters<typeof QueuePanel>[0] = {
    open: true,
    rows: [{
      key: "local:missing",
      title: "离线歌曲",
      subtitle: "歌手 · 本地 · 暂不可用",
      active: false,
      unavailable: true,
    }],
    playMode: "sequential",
    availabilityStatus: "ready",
    onClose: vi.fn(),
    onClear: vi.fn(),
    onJump: vi.fn(),
    onRelink: vi.fn(),
    onRetryAvailability: vi.fn(),
    onRemove: vi.fn(),
    onReorder: vi.fn(),
    ...overrides,
  };
  render(<QueuePanel {...props} />);
  return props;
}

describe("QueuePanel local availability", () => {
  it("disables unavailable playback and exposes retry, relink, and named removal", () => {
    const props = renderPanel();

    expect(screen.getByText("离线歌曲").closest("button")).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重新定位" }));
    fireEvent.click(screen.getByRole("button", { name: "重试检查" }));
    fireEvent.click(screen.getByRole("button", { name: "从队列移除《离线歌曲》" }));

    expect(props.onRelink).toHaveBeenCalledWith(0);
    expect(props.onRetryAvailability).toHaveBeenCalledOnce();
    expect(props.onRemove).toHaveBeenCalledWith(0);
  });

  it("keeps the queue intact and offers retry when checking fails", () => {
    renderPanel({ availabilityStatus: "failed", rows: [{
      key: "local:unknown",
      title: "状态未知歌曲",
      subtitle: "歌手 · 本地",
      active: false,
      unavailable: false,
    }] });

    expect(screen.getByText("本地文件检查失败，队列仍已完整保留。")).toBeInTheDocument();
    expect(screen.getByText("状态未知歌曲").closest("button")).toBeEnabled();
    expect(screen.getByRole("button", { name: "重试检查" })).toBeInTheDocument();
  });
});
