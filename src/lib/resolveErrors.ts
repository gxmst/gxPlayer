/** Map raw LX / engine errors into short Chinese reasons for the toast UI. */

export type FailureKind =
  | "no_source"
  | "timeout"
  | "need_key"
  | "network"
  | "no_url"
  | "preview_only"
  | "cancelled"
  | "file_missing"
  | "decode"
  | "unknown";

export type ClassifiedFailure = {
  kind: FailureKind;
  title: string;
  detail: string;
};

export function classifyPlayError(raw: unknown): ClassifiedFailure {
  const text = String(raw ?? "").trim();
  const lower = text.toLowerCase();

  if (!text || lower.includes("abort") || lower.includes("cancel") || lower.includes("取消")) {
    return {
      kind: "cancelled",
      title: "已取消解析",
      detail: text || "你取消了本次解析请求。",
    };
  }
  if (lower.includes("timeout") || lower.includes("timed out") || lower.includes("超时")) {
    return {
      kind: "timeout",
      title: "解析超时",
      detail: "音源响应过慢。可检查网络、更换音源，或稍后重试。",
    };
  }
  if (
    lower.includes("no_source")
    || lower.includes("no source")
    || lower.includes("还没有可用音源")
    || lower.includes("not ready")
    || lower.includes("no_source")
  ) {
    return {
      kind: "no_source",
      title: "还没有可用音源",
      detail: "请到「音源管理」导入并启用 LX 脚本后再听整首。",
    };
  }
  if (
    lower.includes("key")
    || lower.includes("token")
    || lower.includes("401")
    || lower.includes("403")
    || lower.includes("unauthorized")
    || lower.includes("额度")
  ) {
    return {
      kind: "need_key",
      title: "可能需要配置解析 Key",
      detail: "部分音源公益额度用尽后需要在音源配置里填写 key。",
    };
  }
  if (
    lower.includes("network")
    || lower.includes("dns")
    || lower.includes("connect")
    || lower.includes("tls")
    || lower.includes("ssl")
    || lower.includes("econn")
  ) {
    return {
      kind: "network",
      title: "网络异常",
      detail: "无法连接音源或元数据服务，请检查网络后重试。",
    };
  }
  if (lower.includes("preview") || lower.includes("30 秒") || lower.includes("30s")) {
    return {
      kind: "preview_only",
      title: "仅获得试听",
      detail: text,
    };
  }
  if (lower.includes("not found") || lower.includes("missing") || lower.includes("不存在") || lower.includes("找不到")) {
    return {
      kind: "file_missing",
      title: "文件不可用",
      detail: "本地路径失效或缓存已被清理。",
    };
  }
  if (lower.includes("decode") || lower.includes("unsupported") || lower.includes("codec")) {
    return {
      kind: "decode",
      title: "无法解码",
      detail: "格式不受支持或文件损坏。",
    };
  }
  if (lower.includes("url") || lower.includes("resolve") || lower.includes("musicurl")) {
    return {
      kind: "no_url",
      title: "未能解析播放地址",
      detail: "音源没有返回可用直链。可换音质档位或其它音源。",
    };
  }
  return {
    kind: "unknown",
    title: "播放失败",
    detail: text.slice(0, 280) || "未知错误",
  };
}

export function formatFailureMessage(raw: unknown, songTitle?: string): string {
  const c = classifyPlayError(raw);
  const head = songTitle ? `《${songTitle}》${c.title}` : c.title;
  return `${head}：${c.detail}`;
}
