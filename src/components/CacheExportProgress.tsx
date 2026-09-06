import type { CacheExportProgress as CacheExportProgressType } from "../types";

type CacheExportProgressProps = {
  progress: CacheExportProgressType;
};

export function CacheExportProgress({ progress }: CacheExportProgressProps) {
  return (
    <div className="cache-export-progress" role="status" aria-live="polite">
      <div className="cache-export-progress-copy">
        <strong>
          正在导出 {progress.completed} / {progress.total}
        </strong>
        <span title={progress.current || undefined}>
          {progress.current ? `正在写入 ${progress.current}` : "正在准备缓存文件"}
        </span>
      </div>
      <progress
        aria-label="缓存导出进度"
        aria-valuetext={`${progress.completed} / ${progress.total}`}
        max={Math.max(progress.total, 1)}
        value={Math.min(progress.completed, progress.total)}
      />
    </div>
  );
}
