import { describe, expect, it } from "vitest";
import {
  diagnosticEntryDisplay,
  formatDiagnosticCategory,
  formatDiagnosticSummary,
} from "./diagnosticDisplay";

describe("diagnosticDisplay", () => {
  it("把已知类别和音源路由回退摘要显示为中文", () => {
    const display = diagnosticEntryDisplay({
      category: "proxy_fallback",
      source: "source-1",
      summary: "direct->system_proxy code=timeout",
    });

    expect(display).toEqual({
      category: "音源请求改走备用路由",
      source: "音源 source-1",
      summary: "路由由直连切换为系统代理；原因：请求超时。",
    });
  });

  it("翻译流媒体的路由、阶段、错误和播放代次", () => {
    expect(formatDiagnosticSummary("from=system_proxy to=direct stage=request code=connect generation=42"))
      .toBe("路由由系统代理切换为直连；阶段：发送网络请求；原因：网络连接失败；任务代次：42。");
  });

  it("翻译路由耗尽与 HTTP 状态码", () => {
    expect(formatDiagnosticSummary("routes_exhausted code=upstream_rate_limited"))
      .toBe("所有可用路由均已失败；原因：上游服务请求过于频繁。");
    expect(formatDiagnosticSummary("route=direct http_status=429"))
      .toBe("路由：直连；HTTP 状态码：429。");
  });

  it("翻译缓存操作和播放提交阶段", () => {
    expect(diagnosticEntryDisplay({
      category: "cache_operation_failed",
      source: "cache",
      summary: "operation=set_directory code=invalid_path",
    })).toEqual({
      category: "缓存管理操作失败",
      source: "播放缓存",
      summary: "操作：更改缓存目录；原因：缓存目录无效。",
    });
    expect(formatDiagnosticCategory("playback_submit_failed")).toBe("播放任务提交失败");
    expect(formatDiagnosticSummary("stage=play_cache_entry code=not_found"))
      .toBe("阶段：准备播放缓存；原因：所需文件或资源不存在。");
    expect(formatDiagnosticSummary("stage=enqueue code=channel_disconnected"))
      .toBe("阶段：提交播放任务；原因：内部通信已断开。");
    expect(formatDiagnosticSummary("route=global stage=favorite_sidecar code=write_failed"))
      .toBe("路由：全局网络策略；阶段：更新缓存收藏标记；原因：写入本地文件失败。");
    expect(formatDiagnosticCategory("source_request_timeout")).toBe("音源请求超时");
  });

  it("未知机器值只显示受限代码，不回显整段英文错误", () => {
    expect(formatDiagnosticCategory("future_event")).toBe("其他诊断事件（代码：future_event）");
    expect(formatDiagnosticSummary("future_error_code")).toBe("未分类错误（代码：future_error_code）。");
    expect(formatDiagnosticSummary("failed to open a private local file with unexpected details"))
      .toBe("发生未分类错误；请导出日志查看详细信息。");
  });

  it("保留后端已脱敏的中文摘要", () => {
    expect(formatDiagnosticSummary("缓存目录不可写，请检查权限")).toBe("缓存目录不可写，请检查权限");
  });
});
