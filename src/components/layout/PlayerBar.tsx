import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { AudioLines, Check, ListMusic, Maximize2, Pause, Play, Repeat, Repeat1, Shuffle, SkipBack, SkipForward, SlidersHorizontal, Volume1, Volume2, VolumeX, X } from "lucide-react";
import type { PlayMode } from "../../types";

const modeNames: Record<PlayMode, string> = { sequential: "顺序播放", repeat_all: "列表循环", repeat_one: "单曲循环", shuffle: "随机播放" };
function time(value: number | null) { const seconds = Math.max(0, Math.floor(value ?? 0)); return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`; }

export function PlayerBar(props: {
  title: string; artist: string; cover: ReactNode; playing: boolean; canPlay: boolean; canSeek: boolean;
  position: number; duration: number | null; mode: PlayMode; volume: number; adjustingVolume: boolean;
  queueOpen: boolean; queueCount: number; presetLabel: string; favorite?: ReactNode; details: ReactNode;
  onOpenPlayer: () => void; onPlayPause: () => void; onPrevious: () => void; onNext: () => void;
  onCycleMode: () => void; onSeekPreview: (value: number) => void; onSeek: (value: number) => void;
  onVolumePreview: (value: number) => void; onVolumeCommit: (value: number) => void; onMute: () => void;
  onQueue: () => void; onPreset: () => void; onMini: () => void;
}) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  const detailsRef = useRef<HTMLDivElement>(null);
  const detailsButton = useRef<HTMLButtonElement>(null);
  const seeking = useRef(false);
  const commitSeek = (value: number) => { seeking.current = false; props.onSeek(value); };
  useEffect(() => {
    if (!detailsOpen) return;
    detailsRef.current?.querySelector<HTMLButtonElement>('[aria-label="关闭播放选项"]')?.focus();
    const close = (event: PointerEvent) => { if (!detailsRef.current?.contains(event.target as Node)) setDetailsOpen(false); };
    const key = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || document.querySelector('[aria-modal="true"]')) return;
      event.preventDefault();
      event.stopPropagation();
      setDetailsOpen(false);
      detailsButton.current?.focus();
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", key, true);
    return () => { document.removeEventListener("pointerdown", close); document.removeEventListener("keydown", key, true); };
  }, [detailsOpen]);
  const ModeIcon = props.mode === "shuffle" ? Shuffle : props.mode === "repeat_one" ? Repeat1 : props.mode === "repeat_all" ? Repeat : ListMusic;
  const VolumeIcon = props.volume < 0.001 ? VolumeX : props.volume < 0.5 ? Volume1 : Volume2;
  return <footer className="playback-bar" aria-label="播放器">
    <button type="button" className="playback-track" onClick={props.onOpenPlayer} title="打开播放页">
      <span className="playback-artwork">{props.cover}{props.playing && <span className="playback-live"><AudioLines size={13} /></span>}</span>
      <span><strong>{props.title}</strong><small>{props.artist}</small></span>
    </button>
    <div className="playback-center">
      <div className="playback-transport">
        <button type="button" className={`icon-button ${props.mode !== "sequential" ? "active" : ""}`} onClick={props.onCycleMode} aria-label={modeNames[props.mode]} title={modeNames[props.mode]}><ModeIcon size={17} /></button>
        <button type="button" className="icon-button" onClick={props.onPrevious} aria-label="上一首" title="上一首"><SkipBack size={20} /></button>
        <button type="button" className="playback-toggle" onClick={props.onPlayPause} disabled={!props.canPlay} aria-label={props.playing ? "暂停" : "播放"} title={props.playing ? "暂停" : "播放"}>{props.playing ? <Pause size={19} fill="currentColor" /> : <Play size={19} fill="currentColor" />}</button>
        <button type="button" className="icon-button" onClick={props.onNext} aria-label="下一首" title="下一首"><SkipForward size={20} /></button>
        <button type="button" className="icon-button playback-expand" onClick={props.onMini} aria-label="切换迷你模式" title="切换迷你模式"><Maximize2 size={16} /></button>
      </div>
      <div className="playback-timeline"><time>{time(props.position)}</time><input aria-label="播放进度" type="range" min={0} max={Math.max(props.duration ?? 0, 0.01)} step={0.05}
        value={Math.min(props.position, Math.max(props.duration ?? 0, 0.01))} disabled={!props.canSeek}
        style={{ "--fill": `${props.duration ? Math.min(props.position / props.duration, 1) * 100 : 0}%` } as CSSProperties}
        onChange={(event) => { seeking.current = true; props.onSeekPreview(Number(event.target.value)); }}
        onPointerUp={(event) => commitSeek(Number(event.currentTarget.value))}
        onPointerCancel={(event) => commitSeek(Number(event.currentTarget.value))}
        onBlur={(event) => { if (seeking.current) commitSeek(Number(event.currentTarget.value)); }}
        onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) commitSeek(Number(event.currentTarget.value)); }} /><time>{time(props.duration)}</time></div>
    </div>
    <div className="playback-tools">
      {props.favorite}
      <div className="playback-volume"><button type="button" className="icon-button" onClick={props.onMute} aria-label={props.volume > 0.001 ? "静音" : "取消静音"} title={props.volume > 0.001 ? "静音" : "取消静音"}><VolumeIcon size={18} /></button>
        <input type="range" aria-label="音量" min={0} max={1} step={0.01} value={props.volume} style={{ "--fill": `${props.volume * 100}%` } as CSSProperties}
          onChange={(event) => props.onVolumePreview(Number(event.target.value))}
          onPointerUp={(event) => props.onVolumeCommit(Number(event.currentTarget.value))} onPointerCancel={(event) => props.onVolumeCommit(Number(event.currentTarget.value))}
          onBlur={(event) => { if (props.adjustingVolume) props.onVolumeCommit(Number(event.currentTarget.value)); }}
          onKeyUp={(event) => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) props.onVolumeCommit(Number(event.currentTarget.value)); }} />
      </div>
      <div className="playback-options" ref={detailsRef}>
        <button type="button" ref={detailsButton} className={`icon-button ${detailsOpen ? "active" : ""}`} aria-label="播放选项" title="播放选项" aria-expanded={detailsOpen} onClick={() => setDetailsOpen(!detailsOpen)}><SlidersHorizontal size={18} /></button>
        {detailsOpen && <section className="playback-popover" role="dialog" aria-label="播放选项"><div className="popover-heading"><strong>播放选项</strong><button type="button" className="icon-button" aria-label="关闭播放选项" onClick={() => { setDetailsOpen(false); detailsButton.current?.focus(); }}><X size={16} /></button></div>
          {props.details}<button type="button" className="playback-preset" onClick={() => { props.onPreset(); setDetailsOpen(false); }}><AudioLines size={17} /><span>音效 · {props.presetLabel}</span><Check size={15} /></button>
        </section>}
      </div>
      <button type="button" className={`icon-button playback-queue ${props.queueOpen ? "active" : ""}`} onClick={props.onQueue} aria-label="播放队列" title={`播放队列 · ${props.queueCount}`} aria-expanded={props.queueOpen}><ListMusic size={20} />{props.queueCount > 0 && <span>{props.queueCount}</span>}</button>
    </div>
  </footer>;
}
