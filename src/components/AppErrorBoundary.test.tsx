// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppErrorBoundary } from "./AppErrorBoundary";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("AppErrorBoundary", () => {
  it("renders children while the application is healthy", () => {
    render(<AppErrorBoundary><p>播放器界面</p></AppErrorBoundary>);
    expect(screen.getByText("播放器界面")).toBeInTheDocument();
  });

  it("shows a recoverable fallback when a descendant render fails", () => {
    const onReload = vi.fn();
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const BrokenView = () => {
      throw new Error("render failed");
    };

    render(
      <AppErrorBoundary onReload={onReload}>
        <BrokenView />
      </AppErrorBoundary>,
    );

    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "界面暂时无法显示" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重新载入" }));
    expect(onReload).toHaveBeenCalledOnce();
  });
});
