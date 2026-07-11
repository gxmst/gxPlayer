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
  path: string;
  title: string;
  durationSeconds: number | null;
};

type EngineSnapshot = {
  status: PlaybackStatus;
  queue: QueueItem[];
  queueIndex: number | null;
  positionSeconds: number;
  durationSeconds: number | null;
  volume: number;
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

  useEffect(() => {
    void invoke("ui_ready").catch((error) => setMessage(String(error)));
  }, []);

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

  const commitSeek = async (value: number) => {
    setDragPosition(null);
    await run("player_seek", { seconds: value });
  };

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
          <p>{current?.path ?? "本界面只用于验证内核，不代表正式视觉设计。"}</p>
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

      <section className="queue-panel">
        <h3>本地队列</h3>
        {snapshot.queue.length === 0 ? (
          <p className="empty">选择多个文件可验证切歌与自动连播。</p>
        ) : (
          <ol>
            {snapshot.queue.map((item, index) => (
              <li className={index === snapshot.queueIndex ? "active" : ""} key={item.path}>
                <span>{item.title}</span>
                <time>{formatTime(item.durationSeconds)}</time>
              </li>
            ))}
          </ol>
        )}
      </section>
    </main>
  );
}

export default App;
