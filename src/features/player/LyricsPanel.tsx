import type { RefObject } from "react";
import type { LyricDocument } from "../../types";

type LyricsPanelProps = {
  lyrics: LyricDocument | null;
  activeIndex: number;
  offsetMs: number;
  lyricRefs: RefObject<Array<HTMLParagraphElement | null>>;
  onOffsetChange: (offsetMs: number) => void;
  onChooseLocal: () => void;
  onSeek: (seconds: number) => void;
};

export function LyricsPanel({
  lyrics,
  activeIndex,
  offsetMs,
  lyricRefs,
  onOffsetChange,
  onChooseLocal,
  onSeek,
}: LyricsPanelProps) {
  return (
    <section className="lyrics-panel">
      <div className="lyrics-toolbar">
        <div>
          <strong>歌词</strong>
          <small>点击歌词跳转 · 偏移 {offsetMs > 0 ? "+" : ""}{(offsetMs / 1000).toFixed(1)}s</small>
        </div>
        <div>
          <button type="button" onClick={() => onOffsetChange(offsetMs - 500)} aria-label="歌词提前半秒">−0.5s</button>
          <button type="button" onClick={() => onOffsetChange(0)} disabled={offsetMs === 0}>归零</button>
          <button type="button" onClick={() => onOffsetChange(offsetMs + 500)} aria-label="歌词延后半秒">+0.5s</button>
          <button type="button" onClick={onChooseLocal}>本地 LRC</button>
        </div>
      </div>
      <div className="lyrics-scroll">
        {lyrics?.instrumental ? <p className="lyric active">纯音乐</p> : lyrics?.lines.length ? lyrics.lines.map((line, index) => (
          <p
            className={`lyric ${index === activeIndex ? "active" : ""}`}
            key={`${line.timestampMs}-${index}`}
            ref={(element) => { lyricRefs.current[index] = element; }}
          >
            {line.timestampMs === null
              ? line.text
              : <button type="button" onClick={() => onSeek(Math.max(0, (line.timestampMs! - offsetMs) / 1000))}>{line.text}</button>}
          </p>
        )) : <div className="lyrics-empty"><strong>歌词会出现在这里</strong><span>在线歌曲会自动匹配，也可以载入本地 LRC。</span></div>}
      </div>
    </section>
  );
}
