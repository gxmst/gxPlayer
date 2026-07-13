import type { DiagnosticLogEntry } from "../types";

const CATEGORY_LABELS: Readonly<Record<string, string>> = {
  cache_commit_failed: "缓存提交失败",
  cache_open_failed: "缓存初始化失败",
  cache_operation_failed: "缓存管理操作失败",
  cache_read_failed: "缓存读取失败",
  cache_write_failed: "缓存写入失败",
  online_resolve_failed: "在线音源解析失败",
  playback_runtime_failed: "播放过程失败",
  playback_start_failed: "播放启动失败",
  playback_submit_failed: "播放任务提交失败",
  proxy_fallback: "音源请求改走备用路由",
  route_switched: "音源首选路由已切换",
  source_http_failed: "音源网络请求失败",
  source_http_timeout: "音源网络请求超时",
  source_import_failed: "音源导入失败",
  source_reimport_failed: "音源重新导入失败",
  source_request_failed: "音源请求失败",
  source_request_timeout: "音源请求超时",
  source_worker_crashed: "音源工作进程崩溃",
  stream_request_failed: "流媒体请求失败",
  stream_response_failed: "流媒体响应异常",
  stream_route_fallback: "流媒体改走备用路由",
  stream_runtime_failed: "流媒体读取失败",
};

const ROUTE_LABELS: Readonly<Record<string, string>> = {
  direct: "直连",
  global: "全局网络策略",
  none: "未指定路由",
  system_proxy: "系统代理",
  unknown: "未知路由",
};

const STAGE_LABELS: Readonly<Record<string, string>> = {
  append: "写入缓存数据",
  begin: "初始化缓存写入",
  body: "读取响应数据",
  commit_metadata: "确认缓存元数据",
  commit_rename: "提交缓存文件",
  evict: "清理过期缓存",
  finish_flush: "刷新缓存文件",
  finish_rename: "提交缓存临时文件",
  finish_sync: "同步缓存文件",
  favorite_sidecar: "更新缓存收藏标记",
  initialize: "初始化音源",
  enqueue: "提交播放任务",
  list: "读取缓存列表",
  list_cleanup: "清理缓存列表",
  lookup: "查找缓存",
  lookup_cleanup: "清理缓存索引",
  manifest: "写入缓存索引",
  manifest_reconcile: "恢复缓存索引",
  play_cache_entry: "准备播放缓存",
  pump: "处理音频数据",
  request: "发送网络请求",
  resolve: "解析在线音频",
  response: "校验网络响应",
  restore: "恢复音源状态",
  runtime: "运行音源脚本",
  script: "执行音源脚本",
  session_new: "创建播放会话",
  sidecar: "写入缓存元数据",
  snapshot: "检查播放状态",
  touch: "更新缓存访问记录",
  unknown: "未知阶段",
  verify: "校验在线音频",
  worker: "运行音源工作进程",
};

const ERROR_LABELS: Readonly<Record<string, string>> = {
  active_source_restore_failed: "恢复音源状态失败",
  all_sources_failed: "所有音源均未能返回结果",
  already_exists: "目标文件已存在",
  audio_processing: "音频处理失败",
  blocked_destination: "目标地址被安全策略阻止",
  body: "读取响应数据失败",
  broken_pipe: "数据连接已中断",
  channel_disconnected: "内部通信已断开",
  connect: "网络连接失败",
  decode: "音频解码失败",
  dns_failed: "域名解析失败",
  early_eof: "流媒体数据提前结束",
  failed: "操作失败",
  http_401: "上游服务要求重新认证",
  http_403: "上游服务拒绝访问",
  http_404: "上游资源不存在",
  http_429: "上游服务请求过于频繁",
  http_5xx: "上游服务暂时异常",
  http_task_failed: "网络请求任务异常",
  invalid_candidate_payload: "音源返回的数据无效",
  invalid_data: "数据格式无效",
  invalid_header: "网络响应头无效",
  invalid_path: "缓存目录无效",
  invalid_request: "网络请求无效",
  io: "本地文件访问失败",
  io_error: "本地文件读写失败",
  io_failed: "本地文件读写失败",
  manifest_persist_failed: "缓存索引保存失败",
  media_format: "音频格式无法识别",
  media_spec: "音频参数不受支持",
  media_verification_failed: "在线音频校验失败",
  network: "网络请求失败",
  no_result: "音源未返回结果",
  no_source: "没有已导入且可用的音源",
  not_found: "所需文件或资源不存在",
  operation_failed: "操作失败",
  output_device: "音频输出设备不可用",
  permission_denied: "没有访问权限",
  persist_failed: "保存本地数据失败",
  policy_denied: "目标地址被安全策略阻止",
  preview_or_truncated_media: "音频可能是试听片段或内容不完整",
  range_invalid: "流媒体分段响应无效",
  range_verification_failed: "流媒体分段校验失败",
  redirect: "网络重定向失败",
  redirect_failed: "网络重定向失败",
  request: "网络请求构建失败",
  response_too_large: "网络响应超过大小限制",
  size_limit: "数据超过允许大小",
  source_initialization_failed: "音源初始化失败",
  source_resolution_failed: "音源解析失败",
  storage_full: "存储空间不足",
  timeout: "请求超时",
  transport: "流媒体连接中断",
  upstream_auth_rejected: "上游服务拒绝认证",
  upstream_not_found: "上游资源不存在",
  upstream_rate_limited: "上游服务请求过于频繁",
  upstream_server_error: "上游服务暂时异常",
  upstream_timeout: "上游服务响应超时",
  worker_failed: "音源工作进程异常",
  write_failed: "写入本地文件失败",
  write_zero: "写入本地文件时未写入数据",
};

const SOURCE_LABELS: Readonly<Record<string, string>> = {
  cache: "播放缓存",
  local: "本地播放",
  online: "在线播放",
  stream: "流媒体",
  unknown: "未知来源",
};

const OPERATION_LABELS: Readonly<Record<string, string>> = {
  clear: "清理缓存",
  remove_by_quality: "按音质删除缓存",
  remove_entries: "批量删除缓存项",
  remove_entry: "删除缓存项",
  reset_directory: "恢复默认缓存目录",
  set_directory: "更改缓存目录",
  set_favorite: "更新缓存收藏状态",
  set_limit: "调整缓存上限",
};

const SAFE_CODE_PATTERN = /^[a-z0-9][a-z0-9_.:-]{0,63}$/i;
const HAS_CJK_PATTERN = /[\u3400-\u9fff\uf900-\ufaff]/;

function safeCode(value: string): string | null {
  const normalized = value.trim();
  return SAFE_CODE_PATTERN.test(normalized) ? normalized : null;
}

function codeWithFallback(value: string, labels: Readonly<Record<string, string>>, fallback: string): string {
  const normalized = value.trim().toLowerCase();
  const known = labels[normalized];
  if (known) return known;
  const code = safeCode(normalized);
  return code ? `${fallback}（代码：${code}）` : fallback;
}

function routeLabel(value: string): string {
  return codeWithFallback(value, ROUTE_LABELS, "未知路由");
}

function stageLabel(value: string): string {
  return codeWithFallback(value, STAGE_LABELS, "未知阶段");
}

function errorLabel(value: string): string {
  return codeWithFallback(value, ERROR_LABELS, "未知错误");
}

function operationLabel(value: string): string {
  return codeWithFallback(value, OPERATION_LABELS, "未知操作");
}

function structuredFields(summary: string): {
  fields: Map<string, string>;
  routeTransition: [string, string] | null;
  routesExhausted: boolean;
} {
  const fields = new Map<string, string>();
  let routeTransition: [string, string] | null = null;
  let routesExhausted = false;

  for (const token of summary.split(/\s+/)) {
    if (token === "routes_exhausted") {
      routesExhausted = true;
      continue;
    }
    const separator = token.indexOf("=");
    if (separator > 0) {
      const key = token.slice(0, separator).toLowerCase();
      const value = token.slice(separator + 1);
      if (SAFE_CODE_PATTERN.test(key) && value) fields.set(key, value);
      continue;
    }
    const transition = token.match(/^([^>\s]+)->([^>\s]+)$/);
    if (transition?.[1] && transition[2]) routeTransition = [transition[1], transition[2]];
  }

  const from = fields.get("from");
  const to = fields.get("to");
  if (from && to) routeTransition = [from, to];
  return { fields, routeTransition, routesExhausted };
}

export function formatDiagnosticCategory(category: string): string {
  return codeWithFallback(category, CATEGORY_LABELS, "其他诊断事件");
}

export function formatDiagnosticSource(source: string | null): string {
  if (!source?.trim()) return "应用";
  const normalized = source.trim();
  const known = SOURCE_LABELS[normalized.toLowerCase()];
  if (known) return known;
  if (HAS_CJK_PATTERN.test(normalized)) return normalized.slice(0, 80);
  const code = safeCode(normalized);
  return code ? `音源 ${code}` : "未标明的音源";
}

export function formatDiagnosticSummary(summary: string): string {
  const normalized = summary.trim().replace(/\s+/g, " ");
  if (!normalized) return "未提供错误摘要。";

  const { fields, routeTransition, routesExhausted } = structuredFields(normalized);
  const parts: string[] = [];
  if (routesExhausted) parts.push("所有可用路由均已失败");
  if (routeTransition) {
    parts.push(`路由由${routeLabel(routeTransition[0])}切换为${routeLabel(routeTransition[1])}`);
  } else if (fields.has("route")) {
    parts.push(`路由：${routeLabel(fields.get("route") ?? "")}`);
  }
  if (fields.has("stage")) parts.push(`阶段：${stageLabel(fields.get("stage") ?? "")}`);
  if (fields.has("operation")) parts.push(`操作：${operationLabel(fields.get("operation") ?? "")}`);
  if (fields.has("code")) parts.push(`原因：${errorLabel(fields.get("code") ?? "")}`);
  if (fields.has("http_status")) {
    const status = safeCode(fields.get("http_status") ?? "");
    parts.push(status ? `HTTP 状态码：${status}` : "HTTP 状态码异常");
  }
  if (fields.has("generation")) {
    const generation = fields.get("generation") ?? "";
    if (/^\d{1,20}$/.test(generation)) parts.push(`任务代次：${generation}`);
  }
  if (parts.length > 0) return `${parts.join("；")}。`;

  const knownError = ERROR_LABELS[normalized.toLowerCase()];
  if (knownError) return `${knownError}。`;
  const code = safeCode(normalized);
  if (code) return `未分类错误（代码：${code}）。`;
  if (HAS_CJK_PATTERN.test(normalized)) return normalized.slice(0, 160);
  return "发生未分类错误；请导出日志查看详细信息。";
}

export function diagnosticEntryDisplay(entry: Pick<DiagnosticLogEntry, "category" | "source" | "summary">): {
  category: string;
  source: string;
  summary: string;
} {
  return {
    category: formatDiagnosticCategory(entry.category),
    source: formatDiagnosticSource(entry.source),
    summary: formatDiagnosticSummary(entry.summary),
  };
}
