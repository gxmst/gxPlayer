// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogTrack } from "../types";
import { TextPlaylistImportDialog } from "./TextPlaylistImportDialog";

function track(title: string): CatalogTrack {
  return {
    providerId: "provider",
    providerTrackId: title,
    title,
    artist: "歌手",
    album: "专辑",
    durationMs: null,
    artworkUrl: null,
    resolverPayload: {},
    preview: null,
  };
}

describe("TextPlaylistImportDialog", () => {
  afterEach(() => cleanup());

  it("matches rows and only enqueues after explicit confirmation", async () => {
    const onEnqueue = vi.fn();
    const onClose = vi.fn();
    const search = vi.fn(async (query: string) => [track(query)]);
    render(
      <TextPlaylistImportDialog
        open
        onClose={onClose}
        onEnqueue={onEnqueue}
        search={search}
        delayMs={0}
      />,
    );

    fireEvent.change(screen.getByLabelText("歌曲列表"), { target: { value: "第一首\n第二首" } });
    fireEvent.click(screen.getByRole("button", { name: "开始匹配" }));
    await waitFor(() => expect(screen.getByText("匹配完成")).toBeInTheDocument());

    expect(search).toHaveBeenCalledTimes(2);
    expect(onEnqueue).not.toHaveBeenCalled();
    expect(screen.getByText("已匹配 2 首")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "确认加入队列（2 首）" }));
    await waitFor(() => expect(onEnqueue).toHaveBeenCalledWith([track("第一首"), track("第二首")]));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("shows rejected links and keeps them out of the injected search", async () => {
    const search = vi.fn(async (query: string) => [track(query)]);
    render(
      <TextPlaylistImportDialog
        open
        onClose={() => undefined}
        onEnqueue={() => undefined}
        search={search}
        delayMs={0}
      />,
    );

    fireEvent.change(screen.getByLabelText("歌曲列表"), { target: { value: "https://example.invalid/list\n歌曲" } });
    fireEvent.click(screen.getByRole("button", { name: "开始匹配" }));
    await waitFor(() => expect(screen.getByText("匹配完成")).toBeInTheDocument());

    expect(search).toHaveBeenCalledTimes(1);
    expect(screen.getByText("不支持链接格式，请输入歌曲文本")).toBeInTheDocument();
    expect(screen.getByText("已匹配 1 首")).toBeInTheDocument();
  });

  it("cancels an active import from the dialog", async () => {
    let resolve!: (tracks: CatalogTrack[]) => void;
    const search = vi.fn(() => new Promise<CatalogTrack[]>((done) => { resolve = done; }));
    render(
      <TextPlaylistImportDialog
        open
        onClose={() => undefined}
        onEnqueue={() => undefined}
        search={search}
        delayMs={0}
      />,
    );

    fireEvent.change(screen.getByLabelText("歌曲列表"), { target: { value: "当前\n下一首" } });
    fireEvent.click(screen.getByRole("button", { name: "开始匹配" }));
    await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    resolve([track("当前")]);

    await waitFor(() => expect(screen.getByText("匹配已取消")).toBeInTheDocument());
    expect(search).toHaveBeenCalledTimes(1);
  });

  it("requires confirmation for weak matches, switches candidates, and exports unresolved input", async () => {
    const top = { ...track("目标歌"), artist: "其他歌手" };
    const alternate = { ...track("另一个版本"), artist: "目标歌手", album: "特别版" };
    const onEnqueue = vi.fn();
    const onExportUnmatched = vi.fn();
    const search = vi.fn(async (query: string) => query === "未找到" ? [] : [alternate, top]);
    render(
      <TextPlaylistImportDialog
        open
        onClose={() => undefined}
        onEnqueue={onEnqueue}
        onExportUnmatched={onExportUnmatched}
        search={search}
        delayMs={0}
      />,
    );

    fireEvent.change(screen.getByLabelText("歌曲列表"), {
      target: { value: "目标歌 - 目标歌手\n未找到" },
    });
    fireEvent.click(screen.getByRole("button", { name: "开始匹配" }));
    await waitFor(() => expect(screen.getByText("匹配完成")).toBeInTheDocument());

    const checkbox = screen.getByRole("checkbox", { name: "第 1 行加入队列" });
    expect(checkbox).not.toBeChecked();
    expect(screen.getByText("准备加入 0 首")).toBeInTheDocument();
    expect(screen.getByText("待确认 1 首")).toBeInTheDocument();
    expect(screen.getByText("未匹配 1 首")).toBeInTheDocument();
    expect(screen.getByText("已取消选择 0 首")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "导出未匹配（2 行）" }));
    await waitFor(() => expect(onExportUnmatched).toHaveBeenCalledWith("目标歌 - 目标歌手\n未找到"));

    fireEvent.change(screen.getByRole("combobox", { name: "第 1 行候选版本" }), { target: { value: "1" } });
    fireEvent.click(checkbox);
    expect(screen.getByText("准备加入 1 首")).toBeInTheDocument();
    expect(screen.getByText("待确认 0 首")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "确认加入队列（1 首）" }));
    await waitFor(() => expect(onEnqueue).toHaveBeenCalledWith([alternate]));
  });
});
