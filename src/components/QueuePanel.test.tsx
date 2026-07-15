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
    fireEvent.click(screen.getByRole("button", { name: "重新定位《离线歌曲》" }));
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

describe("QueuePanel ordering", () => {
  const rows = [
    { key: "one", title: "第一首", subtitle: "歌手甲", active: true, unavailable: false },
    { key: "two", title: "第二首", subtitle: "歌手乙", active: false, unavailable: false },
    { key: "three", title: "第三首", subtitle: "歌手丙", active: false, unavailable: false },
  ];

  it("offers named keyboard controls and announces a completed move", () => {
    const props = renderPanel({ rows });

    expect(screen.getByRole("button", { name: "《第一首》已在队首，无法上移" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "《第三首》已在队尾，无法下移" })).toBeDisabled();

    const moveUp = screen.getByRole("button", { name: "将《第二首》上移至第 1 位" });
    expect(moveUp).toBeEnabled();
    fireEvent.click(moveUp);

    expect(props.onReorder).toHaveBeenCalledWith(1, 0);
    expect(screen.getByRole("status")).toHaveTextContent("《第二首》已移至第 1 位，共 3 首。");
  });

  it("keeps pointer drag-and-drop ordering and ignores foreign drag data", () => {
    const props = renderPanel({ rows });
    const items = screen.getAllByRole("listitem");
    const data = new Map<string, string>();
    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: vi.fn((type: string, value: string) => data.set(type, value)),
      getData: vi.fn((type: string) => data.get(type) ?? ""),
    };

    fireEvent.dragStart(items[0], { dataTransfer });
    fireEvent.dragOver(items[2], { dataTransfer });
    fireEvent.drop(items[2], { dataTransfer });

    expect(dataTransfer.effectAllowed).toBe("move");
    expect(dataTransfer.dropEffect).toBe("move");
    expect(props.onReorder).toHaveBeenCalledWith(0, 2);

    data.clear();
    fireEvent.drop(items[1], { dataTransfer });
    expect(props.onReorder).toHaveBeenCalledTimes(1);
  });
});

describe("QueuePanel accessible context", () => {
  it("names the panel, current track, positions, and track-specific actions", () => {
    renderPanel({
      rows: [
        { key: "current", title: "正在听", subtitle: "歌手 · 本地", active: true, unavailable: false },
        { key: "next", title: "下一首", subtitle: "歌手 · 在线", active: false, unavailable: false },
      ],
      playMode: "repeat_all",
    });

    expect(screen.getByRole("complementary", { name: "播放队列" })).toHaveAccessibleDescription(
      "2 首 · 列表循环 · 支持拖拽与键盘排序",
    );
    expect(screen.getByRole("list", { name: "队列曲目" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "当前播放《正在听》，歌手 · 本地，第 1 首，共 2 首" }))
      .toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("button", { name: "播放《下一首》，歌手 · 在线，第 2 首，共 2 首" }))
      .not.toHaveAttribute("aria-current");
    expect(screen.getByRole("group", { name: "调整《下一首》的位置" })).toBeInTheDocument();
  });

  it("does not render the panel when closed", () => {
    renderPanel({ open: false });

    expect(screen.queryByRole("complementary", { name: "播放队列" })).not.toBeInTheDocument();
  });
});
