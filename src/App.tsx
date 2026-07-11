import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./App.css";

type PlaybackStatus =
  | "idle"
  | "loading"
  | "playing"
  | "paused"
  | "buffering"
  | "stopped"
  | "failed";

type QueueItem = {
  location: string;
  title: string;
  durationSeconds: number | null;
  online: boolean;
};

type RuntimeStatus = {
  generation: number;
  state: "no_source" | "initializing" | "ready" | "failed";
  activeSourceId: string | null;
  capabilities: unknown;
  error: string | null;
};

type ListedSource = {
  id: string;
  origin: string;
  metadata: { name: string; version: string; author: string };
  active: boolean;
  updatesEnabled: boolean;
};

type EqBand = {
  enabled: boolean;
  kind: "peak" | "low_shelf" | "high_shelf" | "low_pass" | "high_pass";
  frequencyHz: number;
  gainDb: number;
  q: number;
};

type DspSettings = {
  enabled: boolean;
  eqEnabled: boolean;
  eqBands: EqBand[];
};

type EngineSnapshot = {
  status: PlaybackStatus;
  queue: QueueItem[];
  queueIndex: number | null;
  positionSeconds: number;
  durationSeconds: number | null;
  volume: number;
  dspSettings: DspSettings;
  generation: number;
  underrunCallbacks: number;
  error: string | null;
};

const EMPTY_STATE: EngineSnapshot = {
  status: "idle",
  queue: [],
  queueIndex: null,
  positionSeconds: 0,
  durationSeconds: null,
  volume: 1,
  dspSettings: {
    enabled: false,
    eqEnabled: false,
    eqBands: [{ enabled: true, kind: "peak", frequencyHz: 1000, gainDb: 0, q: 1 }],
  },
  generation: 0,
  underrunCallbacks: 0,
  error: null,
};

function formatTime(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "--:--";
  const value = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(value / 60);
  const remaining = value % 60;
  return `${minutes}:${remaining.toString().padStart(2, "0")}`;
}

function App() {
  const [snapshot, setSnapshot] = useState(EMPTY_STATE);
  const [message, setMessage] = useState("选择一些本地歌曲开始验证 Rust 播放内核。");
  const [dragPosition, setDragPosition] = useState<number | null>(null);
  const [sources, setSources] = useState<ListedSource[]>([]);
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [resolverPayload, setResolverPayload] = useState(
    JSON.stringify(
      {
        source: "wy",
        action: "musicUrl",
        info: { type: "128k", musicInfo: { hash: "phase2-track", name: "Phase 2" } },
      },
      null,
      2,
    ),
  );
  const [sourceUrl, setSourceUrl] = useState("");
  const [sourceBackup, setSourceBackup] = useState("");
  const [resolverSourceId, setResolverSourceId] = useState("");

  useEffect(() => {
    void invoke("ui_ready").catch((error) => setMessage(String(error)));
    void refreshSources();
  }, []);

  const refreshSources = async () => {
    try {
      const [nextSources, nextRuntime] = await Promise.all([
        invoke<ListedSource[]>("source_list"),
        invoke<RuntimeStatus>("source_status"),
      ]);
      setSources(nextSources);
      setRuntime(nextRuntime);
    } catch (error) {
      setMessage(String(error));
    }
  };

  useEffect(() => {
    let disposed = false;
    const update = async () => {
      try {
        const next = await invoke<EngineSnapshot>("player_snapshot");
        if (!disposed) setSnapshot(next);
      } catch (error) {
        if (!disposed) setMessage(String(error));
      }
    };
    void update();
    const timer = window.setInterval(update, 150);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

  const current = useMemo(
    () =>
      snapshot.queueIndex === null ? null : snapshot.queue[snapshot.queueIndex] ?? null,
    [snapshot.queue, snapshot.queueIndex],
  );
  const shownPosition = dragPosition ?? snapshot.positionSeconds;
  const duration = snapshot.durationSeconds ?? 0;
  const isPlaying = snapshot.status === "playing" || snapshot.status === "loading";

  const run = async (command: string, args?: Record<string, unknown>) => {
    try {
      await invoke(command, args);
      setMessage("");
    } catch (error) {
      setMessage(String(error));
    }
  };

  const chooseFiles = async () => {
    const selected = await open({
      multiple: true,
      directory: false,
      filters: [
        {
          name: "Audio",
          extensions: ["mp3", "flac", "wav", "m4a", "aac", "ogg"],
        },
      ],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    await run("player_load_local", { paths });
  };

  const chooseSource = async () => {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "LX source script", extensions: ["js"] }],
    });
    if (!selected || Array.isArray(selected)) return;
    await run("source_import_file", { path: selected });
    await refreshSources();
  };

  const resolveAndPlay = async () => {
    try {
      const payload = JSON.parse(resolverPayload) as unknown;
      const request = await invoke("source_resolve", {
        payload,
        quality: "128k",
        sourceId: resolverSourceId || null,
      });
      await invoke("player_load_resolved", { request, title: "LX 在线流验证" });
      setMessage("");
    } catch (error) {
      setMessage(String(error));
    }
  };

  const commitSeek = async (value: number) => {
    setDragPosition(null);
    await run("player_seek", { seconds: value });
  };

  const setDsp = async (settings: DspSettings) => {
    setSnapshot((state) => ({ ...state, dspSettings: settings }));
    await run("player_set_dsp_settings", { settings });
  };

  const firstBand = snapshot.dspSettings.eqBands[0] ?? EMPTY_STATE.dspSettings.eqBands[0];

  return (
    <main className="dev-shell">
      <header>
        <div>
          <p className="eyebrow">GXPlayer · Phase 0 development shell</p>
          <h1>Rust local playback core</h1>
        </div>
        <button className="primary" onClick={chooseFiles}>
          选择音频文件
        </button>
      </header>

      <section className="now-playing" aria-live="polite">
        <div className="track-copy">
          <span className={`status status-${snapshot.status}`}>{snapshot.status}</span>
          <h2>{current?.title ?? "还没有载入歌曲"}</h2>
          <p>{current?.location ?? "本界面只用于验证内核，不代表正式视觉设计。"}</p>
        </div>

        <div className="transport">
          <button onClick={() => run("player_previous")} disabled={!current} aria-label="上一首">
            ◀◀
          </button>
          <button
            className="play"
            onClick={() => run(isPlaying ? "player_pause" : "player_play")}
            disabled={!current}
          >
            {isPlaying ? "暂停" : "播放"}
          </button>
          <button onClick={() => run("player_next")} disabled={!current} aria-label="下一首">
            ▶▶
          </button>
        </div>

        <div className="timeline">
          <span>{formatTime(shownPosition)}</span>
          <input
            aria-label="播放进度"
            type="range"
            min={0}
            max={Math.max(duration, 0.01)}
            step={0.05}
            value={Math.min(shownPosition, Math.max(duration, 0.01))}
            disabled={!current || duration <= 0}
            onChange={(event) => setDragPosition(Number(event.target.value))}
            onPointerUp={(event) => commitSeek(Number(event.currentTarget.value))}
            onKeyUp={(event) => {
              if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
                void commitSeek(Number(event.currentTarget.value));
              }
            }}
          />
          <span>{formatTime(snapshot.durationSeconds)}</span>
        </div>

        <label className="volume">
          <span>音量 {Math.round(snapshot.volume * 100)}%</span>
          <input
            aria-label="音量"
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={snapshot.volume}
            onChange={(event) => {
              const volume = Number(event.target.value);
              setSnapshot((state) => ({ ...state, volume }));
            }}
            onPointerUp={(event) =>
              run("player_set_volume", { volume: Number(event.currentTarget.value) })
            }
            onKeyUp={(event) => {
              if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
                void run("player_set_volume", { volume: Number(event.currentTarget.value) });
              }
            }}
          />
        </label>
      </section>

      {(message || snapshot.error) && (
        <p className="message" role="status">
          {snapshot.error ?? message}
        </p>
      )}

      <section className="diagnostics">
        <div>
          <span>Generation</span>
          <strong>{snapshot.generation}</strong>
        </div>
        <div>
          <span>Underrun callbacks</span>
          <strong>{snapshot.underrunCallbacks}</strong>
        </div>
        <div>
          <span>Queue</span>
          <strong>{snapshot.queue.length}</strong>
        </div>
      </section>

      <section className="dsp-panel">
        <div className="dsp-heading">
          <div>
            <p className="eyebrow">Phase 1</p>
            <h3>透明旁路 + 单段参量 EQ 验证</h3>
          </div>
          <label className="switch-row">
            <input
              type="checkbox"
              checked={snapshot.dspSettings.enabled}
              onChange={(event) =>
                setDsp({ ...snapshot.dspSettings, enabled: event.target.checked })
              }
            />
            DSP 总开关
          </label>
        </div>
        <label className="switch-row">
          <input
            type="checkbox"
            checked={snapshot.dspSettings.eqEnabled}
            disabled={!snapshot.dspSettings.enabled}
            onChange={(event) =>
              setDsp({ ...snapshot.dspSettings, eqEnabled: event.target.checked })
            }
          />
          参量 EQ
        </label>
        <label className="eq-control">
          <span>1 kHz 峰值增益</span>
          <input
            type="range"
            min={-12}
            max={12}
            step={0.5}
            value={firstBand.gainDb}
            disabled={!snapshot.dspSettings.enabled || !snapshot.dspSettings.eqEnabled}
            onChange={(event) => {
              const band = { ...firstBand, gainDb: Number(event.target.value) };
              setSnapshot((state) => ({
                ...state,
                dspSettings: { ...state.dspSettings, eqBands: [band] },
              }));
            }}
            onPointerUp={(event) => {
              const band = { ...firstBand, gainDb: Number(event.currentTarget.value) };
              void setDsp({ ...snapshot.dspSettings, eqBands: [band] });
            }}
          />
          <output>{firstBand.gainDb.toFixed(1)} dB</output>
        </label>
        <p className="dsp-note">
          DSP 关闭时工作线程在任何采样操作前直接返回；自动测试按 f32 位模式比较输入输出。
        </p>
      </section>

      <section className="queue-panel">
        <h3>本地队列</h3>
        {snapshot.queue.length === 0 ? (
          <p className="empty">选择多个文件可验证切歌与自动连播。</p>
        ) : (
          <ol>
            {snapshot.queue.map((item, index) => (
              <li className={index === snapshot.queueIndex ? "active" : ""} key={item.location}>
                <span>{item.title}</span>
                <time>{formatTime(item.durationSeconds)}</time>
              </li>
            ))}
          </ol>
        )}
      </section>

      <section className="queue-panel source-panel">
        <div className="dsp-heading">
          <div>
            <p className="eyebrow">Phase 2</p>
            <h3>LX 隔离运行时与原生在线播放</h3>
          </div>
          <button onClick={chooseSource}>导入脚本</button>
        </div>
        <p>
          Runtime: <strong>{runtime?.state ?? "unknown"}</strong> · generation {runtime?.generation ?? 0}
        </p>
        <div className="source-import-row">
          <input
            aria-label="音源脚本 URL"
            placeholder="https://…/source.js"
            value={sourceUrl}
            onChange={(event) => setSourceUrl(event.target.value)}
          />
          <button
            disabled={!sourceUrl.trim()}
            onClick={async () => {
              await run("source_import_url", { url: sourceUrl.trim() });
              await refreshSources();
            }}
          >
            URL 导入
          </button>
        </div>
        {runtime?.error && <p className="message">{runtime.error}</p>}
        <ul>
          {sources.map((source) => (
            <li key={source.id}>
              <span>{source.metadata.name || source.id}</span>
              <button
                disabled={source.active}
                onClick={async () => {
                  await run("source_activate", { id: source.id });
                  await refreshSources();
                }}
              >
                {source.active ? "已启用" : "启用"}
              </button>
              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={source.updatesEnabled}
                  onChange={async (event) => {
                    await run("source_set_updates_enabled", {
                      id: source.id,
                      enabled: event.target.checked,
                    });
                    await refreshSources();
                  }}
                />
                更新提醒
              </label>
              <button
                onClick={async () => {
                  await run("source_remove", { id: source.id });
                  await refreshSources();
                }}
              >
                删除
              </button>
            </li>
          ))}
        </ul>
        <label className="resolver-payload">
          <span>Resolver payload (JSON)</span>
          <textarea value={resolverPayload} onChange={(event) => setResolverPayload(event.target.value)} />
        </label>
        <label className="resolver-payload">
          <span>本次解析音源（可临时切源，完成后恢复当前源）</span>
          <select value={resolverSourceId} onChange={(event) => setResolverSourceId(event.target.value)}>
            <option value="">当前启用源</option>
            {sources.map((source) => (
              <option key={source.id} value={source.id}>
                {source.metadata.name || source.id}
              </option>
            ))}
          </select>
        </label>
        <button className="primary" disabled={runtime?.state !== "ready"} onClick={resolveAndPlay}>
          解析并交给 Rust 播放
        </button>
        <label className="resolver-payload">
          <span>音源备份 JSON</span>
          <textarea value={sourceBackup} onChange={(event) => setSourceBackup(event.target.value)} />
        </label>
        <div className="source-import-row">
          <button
            onClick={async () => {
              try {
                const backup = await invoke("source_export_backup");
                setSourceBackup(JSON.stringify(backup, null, 2));
              } catch (error) {
                setMessage(String(error));
              }
            }}
          >
            导出备份
          </button>
          <button
            disabled={!sourceBackup.trim()}
            onClick={async () => {
              try {
                await invoke("source_restore_backup", { backup: JSON.parse(sourceBackup) });
                await refreshSources();
              } catch (error) {
                setMessage(String(error));
              }
            }}
          >
            恢复备份
          </button>
        </div>
      </section>
    </main>
  );
}

export default App;
