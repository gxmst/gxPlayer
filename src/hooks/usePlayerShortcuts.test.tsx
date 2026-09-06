// @vitest-environment jsdom
import { cleanup, fireEvent, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePlayerShortcuts } from "./usePlayerShortcuts";

afterEach(cleanup);
function setup() {
  const actions = { onSearch: vi.fn(), onToggle: vi.fn(), onNext: vi.fn(), onPrevious: vi.fn(), onQueue: vi.fn() };
  const hook = renderHook(() => usePlayerShortcuts(actions));
  return { ...hook, actions };
}

describe("player shortcuts", () => {
  it("handles transport and queue shortcuts without repeating on held keys", () => {
    const { actions, unmount } = setup();
    fireEvent.keyDown(document, { code: "Space" });
    fireEvent.keyDown(document, { code: "Space", repeat: true });
    fireEvent.keyDown(document, { key: "ArrowRight", ctrlKey: true });
    fireEvent.keyDown(document, { key: "ArrowLeft", metaKey: true });
    fireEvent.keyDown(document, { key: "j", ctrlKey: true });
    expect(actions.onToggle).toHaveBeenCalledOnce();
    expect(actions.onNext).toHaveBeenCalledOnce();
    expect(actions.onPrevious).toHaveBeenCalledOnce();
    expect(actions.onQueue).toHaveBeenCalledOnce();
    unmount();
    fireEvent.keyDown(document, { code: "Space" });
    expect(actions.onToggle).toHaveBeenCalledOnce();
  });

  it("leaves text editing and modal dialogs alone", () => {
    const { actions } = setup();
    const input = document.createElement("input");
    document.body.append(input);
    fireEvent.keyDown(input, { code: "Space" });
    fireEvent.keyDown(input, { key: "ArrowLeft", ctrlKey: true });
    expect(actions.onToggle).not.toHaveBeenCalled();
    expect(actions.onPrevious).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "k", ctrlKey: true });
    expect(actions.onSearch).toHaveBeenCalledOnce();
    input.remove();
    const dialog = document.createElement("div");
    dialog.setAttribute("aria-modal", "true");
    document.body.append(dialog);
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    fireEvent.keyDown(document, { code: "Space" });
    expect(actions.onSearch).toHaveBeenCalledOnce();
    expect(actions.onToggle).not.toHaveBeenCalled();
    dialog.remove();
  });
});
