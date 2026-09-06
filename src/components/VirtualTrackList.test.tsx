// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { VirtualTrackList } from "./VirtualTrackList";
import type { LibraryTrack } from "../types";

afterEach(cleanup);
const tracks: LibraryTrack[] = Array.from({ length: 160 }, (_, id) => ({
  id, title: `Track ${id}`, path: `music/${id}.flac`, artist: "Artist", album: "Album",
  durationSeconds: 180, addedAtMs: 1, favorite: false, missing: false,
}));
const renderRow = (track: LibraryTrack) => <span>{track.title}</span>;

describe("VirtualTrackList", () => {
  it("returns to the first result after filtering a scrolled list", () => {
    const { rerender } = render(<VirtualTrackList tracks={tracks} renderRow={renderRow} className="selectable-track-list" />);
    const list = screen.getByRole("list");
    expect(list).toHaveClass("selectable-track-list");
    list.scrollTop = 6800;
    fireEvent.scroll(list);
    expect(screen.queryByText("Track 0")).not.toBeInTheDocument();
    rerender(<VirtualTrackList tracks={tracks.slice(0, 90)} renderRow={renderRow} />);
    expect(list.scrollTop).toBe(0);
    expect(screen.getByText("Track 0")).toBeInTheDocument();
  });

  it("preserves the scroll position when only track metadata changes", () => {
    const { rerender } = render(<VirtualTrackList tracks={tracks} renderRow={renderRow} />);
    const list = screen.getByRole("list");
    list.scrollTop = 3400;
    fireEvent.scroll(list);
    rerender(<VirtualTrackList tracks={tracks.map((track) => ({ ...track, favorite: true }))} renderRow={renderRow} />);
    expect(list.scrollTop).toBe(3400);
    expect(screen.queryByText("Track 0")).not.toBeInTheDocument();
  });
});
