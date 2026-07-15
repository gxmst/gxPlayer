import { describe, expect, it } from "vitest";

import {
  createApplicationBackup,
  formatRestoreConfirmation,
  parseBackupText,
} from "./backupRestore";

describe("backup restore helpers", () => {
  it("uses the exported library version for the application envelope", () => {
    const library = { version: 2, tracks: [], playlists: [] };
    const sources = { version: 1, sources: [] };

    const backup = createApplicationBackup(library, sources);

    expect(backup).toEqual({ version: 2, library, sources });
    expect(parseBackupText(JSON.stringify(backup))).toEqual(backup);
  });

  it("accepts compatible v1 and v2 envelopes and rejects mismatches", () => {
    expect(parseBackupText(JSON.stringify({
      version: 1,
      library: { version: 1, tracks: [], playlists: [] },
      sources: { version: 1, sources: [] },
    }))).toEqual({
      version: 1,
      library: { version: 1, tracks: [], playlists: [] },
      sources: { version: 1, sources: [] },
    });

    expect(() => parseBackupText("not json")).toThrow("不是有效的 JSON");
    expect(parseBackupText(JSON.stringify({
      version: 2,
      library: { version: 2, tracks: [], playlists: [] },
      sources: { version: 1, sources: [] },
    })).version).toBe(2);
    expect(() => parseBackupText('{"version":3,"library":{},"sources":{}}')).toThrow("不支持的备份版本");
    expect(() => parseBackupText('{"version":2,"library":{"version":1},"sources":{}}')).toThrow("版本不匹配");
    expect(() => parseBackupText('{"version":1,"library":{}}')).toThrow("缺少有效的音源数据");
  });

  it("includes every overwrite count in the second confirmation", () => {
    const message = formatRestoreConfirmation({
      trackCount: 23,
      playlistCount: 4,
      sourceCount: 2,
    });

    expect(message).toContain("23 首曲目");
    expect(message).toContain("4 个歌单");
    expect(message).toContain("2 个音源");
    expect(message).toContain("自动回滚");
  });
});
