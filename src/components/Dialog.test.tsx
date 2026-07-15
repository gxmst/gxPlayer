// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { createRef } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Dialog } from "./Dialog";

describe("Dialog", () => {
  afterEach(() => cleanup());

  it("moves focus to the preferred initial control", async () => {
    const initialFocusRef = createRef<HTMLButtonElement>();
    render(
      <Dialog open title="焦点" initialFocusRef={initialFocusRef} onRequestClose={() => undefined}>
        <button type="button">第一个</button>
        <button ref={initialFocusRef} type="button">首选</button>
      </Dialog>,
    );

    await waitFor(() => expect(screen.getByRole("button", { name: "首选" })).toHaveFocus());
  });

  it("traps Tab and Shift+Tab with the current set of controls", async () => {
    const { rerender } = render(
      <Dialog open title="圈定焦点" showClose={false} onRequestClose={() => undefined}>
        <button type="button">第一项</button>
        <button type="button">第二项</button>
      </Dialog>,
    );
    const first = screen.getByRole("button", { name: "第一项" });
    const second = screen.getByRole("button", { name: "第二项" });
    await waitFor(() => expect(first).toHaveFocus());

    second.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(first).toHaveFocus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(second).toHaveFocus();

    rerender(
      <Dialog open title="圈定焦点" showClose={false} onRequestClose={() => undefined}>
        <button type="button">第一项</button>
        <button type="button">新增项</button>
        <button type="button">第二项</button>
      </Dialog>,
    );
    first.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(screen.getByRole("button", { name: "第二项" })).toHaveFocus();
  });

  it("requests close from Escape, the backdrop, and the close button", async () => {
    const onRequestClose = vi.fn();
    render(<Dialog open title="关闭" onRequestClose={onRequestClose}>内容</Dialog>);
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭对话框" })).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.mouseDown(screen.getByRole("presentation"));
    fireEvent.click(screen.getByRole("button", { name: "关闭对话框" }));

    expect(onRequestClose.mock.calls.map(([reason]) => reason)).toEqual([
      "escape",
      "backdrop",
      "close-button",
    ]);
  });

  it("blocks every dismissal path while busy", async () => {
    const onRequestClose = vi.fn();
    render(<Dialog open busy title="处理中" onRequestClose={onRequestClose}>内容</Dialog>);
    await waitFor(() => expect(screen.getByRole("dialog")).toHaveAttribute("aria-busy", "true"));

    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.mouseDown(screen.getByRole("presentation"));
    fireEvent.click(screen.getByRole("button", { name: "关闭对话框" }));

    expect(screen.getByRole("button", { name: "关闭对话框" })).toBeDisabled();
    expect(onRequestClose).not.toHaveBeenCalled();
  });

  it("restores the opener and safely falls back when it was removed", async () => {
    const onRequestClose = vi.fn();
    const { rerender } = render(
      <>
        <button type="button">打开者</button>
        <Dialog open={false} title="回焦" onRequestClose={onRequestClose}>内容</Dialog>
      </>,
    );
    const opener = screen.getByRole("button", { name: "打开者" });
    opener.focus();

    rerender(
      <>
        <button type="button">打开者</button>
        <Dialog open title="回焦" onRequestClose={onRequestClose}>内容</Dialog>
      </>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭对话框" })).toHaveFocus());

    rerender(
      <>
        <button type="button">打开者</button>
        <Dialog open={false} title="回焦" onRequestClose={onRequestClose}>内容</Dialog>
      </>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "打开者" })).toHaveFocus());

    rerender(
      <>
        <button type="button">即将移除的打开者</button>
        <Dialog open={false} title="无打开者" onRequestClose={onRequestClose}>内容</Dialog>
      </>,
    );
    const removedOpener = screen.getByRole("button", { name: "即将移除的打开者" });
    removedOpener.focus();
    rerender(
      <>
        <button type="button">即将移除的打开者</button>
        <Dialog open title="无打开者" onRequestClose={onRequestClose}>内容</Dialog>
      </>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭对话框" })).toHaveFocus());
    rerender(<Dialog open={false} title="无打开者" onRequestClose={onRequestClose}>内容</Dialog>);
    await waitFor(() => expect(document.body).toHaveFocus());
  });

  it("lets only the top dialog handle keyboard commands", async () => {
    const closeLower = vi.fn();
    const closeUpper = vi.fn();
    const { rerender } = render(
      <>
        <Dialog open title="下层" showClose={false} onRequestClose={closeLower}>
          <button type="button">下层按钮</button>
        </Dialog>
        <Dialog open title="上层" showClose={false} onRequestClose={closeUpper}>
          <button type="button">上层第一项</button>
          <button type="button">上层第二项</button>
        </Dialog>
      </>,
    );
    const upperFirst = screen.getByRole("button", { name: "上层第一项" });
    const upperSecond = screen.getByRole("button", { name: "上层第二项" });
    await waitFor(() => expect(upperFirst).toHaveFocus());
    const [lowerBackdrop, upperBackdrop] = Array.from(
      document.querySelectorAll<HTMLElement>(".app-dialog-backdrop"),
    );
    expect(lowerBackdrop).toHaveAttribute("aria-hidden", "true");
    expect(lowerBackdrop).toHaveAttribute("inert");
    expect(lowerBackdrop.style.zIndex).toBe("220");
    expect(upperBackdrop).not.toHaveAttribute("aria-hidden");
    expect(upperBackdrop).not.toHaveAttribute("inert");
    expect(upperBackdrop.style.zIndex).toBe("222");

    upperSecond.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(upperFirst).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });

    expect(closeUpper).toHaveBeenCalledWith("escape");
    expect(closeLower).not.toHaveBeenCalled();

    rerender(
      <>
        <Dialog open title="下层" showClose={false} onRequestClose={closeLower}>
          <button type="button">下层按钮</button>
        </Dialog>
        <Dialog open={false} title="上层" showClose={false} onRequestClose={closeUpper}>
          <button type="button">上层第一项</button>
        </Dialog>
      </>,
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "下层按钮" })).toHaveFocus());
    expect(lowerBackdrop).not.toHaveAttribute("aria-hidden");
    expect(lowerBackdrop).not.toHaveAttribute("inert");
    expect(lowerBackdrop.style.zIndex).toBe("220");
  });
});
