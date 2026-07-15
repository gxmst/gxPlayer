export type ApplicationBackupPayload = {
  version: 1 | 2;
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

export function createApplicationBackup(
  library: unknown,
  sources: unknown,
): ApplicationBackupPayload {
  if (!isObject(library)) throw new Error("无法导出：曲库备份数据无效。");
  if (!isObject(sources)) throw new Error("无法导出：音源备份数据无效。");
  if (library.version !== 1 && library.version !== 2) {
    throw new Error("无法导出：曲库备份版本不受支持。");
  }
  return {
    version: library.version,
    library,
    sources,
  };
}

export function parseBackupText(text: string): ApplicationBackupPayload {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("备份内容不是有效的 JSON。");
  }
  if (!isObject(value)) throw new Error("备份内容必须是一个 JSON 对象。");
  if (value.version !== 1 && value.version !== 2) throw new Error("不支持的备份版本。");
  if (!isObject(value.library)) throw new Error("备份中缺少有效的曲库数据。");
  if (!isObject(value.sources)) throw new Error("备份中缺少有效的音源数据。");
  const expectedLibraryVersion = value.version;
  if (value.library.version !== expectedLibraryVersion) {
    throw new Error(`备份容器版本 ${value.version} 与曲库版本不匹配。`);
  }
  return {
    version: value.version,
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
