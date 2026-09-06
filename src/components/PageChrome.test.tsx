// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { TabBar } from "./PageChrome";

afterEach(cleanup);

function Tabs() {
  const [value, setValue] = useState("local");
  return <TabBar label="Library" value={value} onChange={setValue} items={[
    { id: "local", label: "Local" },
    { id: "cache", label: "Cache", count: 2 },
    { id: "all", label: "All" },
  ]} />;
}

describe("TabBar keyboard navigation", () => {
  it("moves selection and focus together, wrapping at both ends", () => {
    render(<Tabs />);
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((tab) => tab.tabIndex)).toEqual([0, -1, -1]);
    tabs[0].focus();
    fireEvent.keyDown(tabs[0], { key: "ArrowLeft" });
    expect(tabs[2]).toHaveFocus();
    expect(tabs[2]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(tabs[2], { key: "ArrowRight" });
    expect(tabs[0]).toHaveFocus();
    expect(tabs.map((tab) => tab.tabIndex)).toEqual([0, -1, -1]);
    fireEvent.keyDown(tabs[0], { key: "End" });
    expect(tabs[2]).toHaveFocus();
    fireEvent.keyDown(tabs[2], { key: "Home" });
    expect(tabs[0]).toHaveFocus();
    fireEvent.click(tabs[1]);
    expect(tabs[1]).toHaveAttribute("aria-selected", "true");
  });
});
