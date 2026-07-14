export type ApplicationBackupPayload = {
  version: 1;
  library: Record<string, unknown>;
  sources: Record<string, unknown>;
};

export type BackupRestorePreview = {
  trackCount: number;
  playlistCount: number;
  sourceCount: number;
};

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function parseBackupText(text: string): ApplicationBackupPayload {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("备份内容不是有效的 JSON。");
  }
  if (!isObject(value)) throw new Error("备份内容必须是一个 JSON 对象。");
  if (value.version !== 1) throw new Error("不支持的备份版本。");
  if (!isObject(value.library)) throw new Error("备份中缺少有效的曲库数据。");
  if (!isObject(value.sources)) throw new Error("备份中缺少有效的音源数据。");
  return {
    version: 1,
    library: value.library,
    sources: value.sources,
  };
}

export function formatRestoreConfirmation(preview: BackupRestorePreview): string {
  return [
    `即将覆盖当前数据：${preview.trackCount} 首曲目 / ${preview.playlistCount} 个歌单 / ${preview.sourceCount} 个音源。`,
    "恢复期间请勿关闭 GXPlayer；若任一步骤失败，应用会自动回滚。",
    "确定继续恢复吗？",
  ].join("\n\n");
}
