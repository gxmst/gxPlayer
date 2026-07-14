// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useActionDialog, type ActionSpec } from "./ActionDialog";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function Harness<T>({ spec }: { spec: ActionSpec<T> }) {
  const action = useActionDialog();
  return (
    <>
      <button type="button" onClick={() => action.openAction(spec)}>打开</button>
      <button type="button" onClick={action.closeAction}>外部关闭</button>
      {action.dialog}
    </>
  );
}

describe("ActionDialog", () => {
  afterEach(() => cleanup());

  it("uses a synchronous lock so same-tick double clicks only run once", async () => {
    const pending = deferred<void>();
    const run = vi.fn(() => pending.promise);
    render(<Harness spec={{ title: "删除缓存", confirmLabel: "确认删除", run }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    const confirm = screen.getByRole("button", { name: "确认删除" });
    fireEvent.click(confirm);
    fireEvent.click(confirm);

    expect(run).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "处理中…" })).toBeDisabled();
    pending.resolve();
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("only offers an explicit retry for retry-safe failures", async () => {
    const run = vi.fn()
      .mockRejectedValueOnce(new Error("网络暂时不可用"))
      .mockResolvedValueOnce(undefined);
    render(<Harness spec={{
      title: "刷新缓存",
      run,
      retrySafe: true,
      classifyError: (error) => ({ kind: "transient", message: String(error) }),
    }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("网络暂时不可用");

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(run).toHaveBeenCalledTimes(2);
  });

  it.each(["validation", "permanent"] as const)(
    "does not replay a %s failure even when the action is retry-safe",
    async (kind) => {
      const run = vi.fn(async () => { throw new Error("不能重放"); });
      render(<Harness spec={{
        title: "不可重放",
        run,
        retrySafe: true,
        classifyError: () => ({ kind, message: "请关闭后修正" }),
      }} />);

      fireEvent.click(screen.getByRole("button", { name: "打开" }));
      fireEvent.click(screen.getByRole("button", { name: "确认" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("请关闭后修正");
      expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
      expect(run).toHaveBeenCalledTimes(1);
    },
  );

  it("never replays a committed run when afterSuccess fails", async () => {
    const run = vi.fn(async () => "result");
    const afterSuccess = vi.fn(async () => { throw new Error("刷新界面失败"); });
    render(<Harness spec={{ title: "恢复备份", run, afterSuccess, retrySafe: true }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    const alert = await screen.findByRole("alert");

    expect(alert).toHaveTextContent("操作已完成，但后续处理失败");
    expect(alert).toHaveTextContent("刷新界面失败");
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
    expect(run).toHaveBeenCalledTimes(1);
    expect(afterSuccess).toHaveBeenCalledWith("result");
  });

  it("keeps a real undo available after committed follow-up work fails", async () => {
    const run = vi.fn(async () => 7);
    const undo = vi.fn(async () => undefined);
    render(<Harness spec={{
      title: "移除歌曲",
      run,
      afterSuccess: async () => { throw new Error("刷新失败"); },
      undo: { label: "撤销移除", run: undo },
    }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("操作已完成，但后续处理失败");

    fireEvent.click(screen.getByRole("button", { name: "撤销移除" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(undo).toHaveBeenCalledWith(7);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("cannot be closed while an action is busy", async () => {
    const pending = deferred<void>();
    render(<Harness spec={{ title: "正在恢复", run: () => pending.promise }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "外部关闭" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    pending.resolve();
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("silently closes cancelled actions", async () => {
    const cancelled = new Error("request stopped");
    cancelled.name = "AbortError";
    render(<Harness spec={{ title: "载入数据", run: async () => { throw cancelled; } }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("locks undo independently, displays its error, and retries only when safe", async () => {
    const firstUndo = deferred<void>();
    const undo = vi.fn()
      .mockImplementationOnce(() => firstUndo.promise)
      .mockResolvedValueOnce(undefined);
    render(<Harness spec={{
      title: "移除歌曲",
      run: async () => 42,
      completedDescription: "歌曲已移除。",
      undo: {
        run: undo,
        retrySafe: true,
        classifyError: () => ({ kind: "transient", message: "撤销暂时失败" }),
      },
    }} />);

    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    expect(await screen.findByText("歌曲已移除。")).toBeInTheDocument();

    const undoButton = screen.getByRole("button", { name: "撤销" });
    fireEvent.click(undoButton);
    fireEvent.click(undoButton);
    expect(undo).toHaveBeenCalledTimes(1);
    expect(undo).toHaveBeenCalledWith(42);

    firstUndo.reject(new Error("failed"));
    expect(await screen.findByRole("alert")).toHaveTextContent("撤销暂时失败");
    fireEvent.click(screen.getByRole("button", { name: "重试撤销" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(undo).toHaveBeenCalledTimes(2);
  });
});
