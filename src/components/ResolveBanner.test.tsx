// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ResolveBanner } from "./ResolveBanner";

describe("ResolveBanner", () => {
  it("does not render while hidden", () => {
    const { container } = render(<ResolveBanner visible={false} title="解析中" onCancel={() => undefined} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("exposes cancellation as an accessible action", () => {
    const onCancel = vi.fn();
    render(<ResolveBanner visible title="正在解析测试歌曲" detail="可取消" onCancel={onCancel} />);
    expect(screen.getByRole("status")).toHaveTextContent("正在解析测试歌曲");
    fireEvent.click(screen.getByRole("button", { name: /取消/ }));
    expect(onCancel).toHaveBeenCalledOnce();
  });
});
