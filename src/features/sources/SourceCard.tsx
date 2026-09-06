import type { ListedSource } from "../../types";
import { ArrowDown, ArrowUp, Pencil, RefreshCw, Trash2 } from "lucide-react";

export function SourceCard({
  source,
  index,
  total,
  dragging,
  busy,
  reimporting,
  onDragStart,
  onDragEnd,
  onDrop,
  onMove,
  onEdit,
  onToggle,
  onReimport,
  onRemove,
}: {
  source: ListedSource;
  index: number;
  total: number;
  dragging: boolean;
  busy: boolean;
  reimporting: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDrop: () => void;
  onMove: (direction: -1 | 1) => void;
  onEdit: () => void;
  onToggle: () => void;
  onReimport: () => void;
  onRemove: () => void;
}) {
  const displayName = source.metadata.name || "未命名音源";
  const effectivePriority = source.effectivePriority === null ? "不参与" : `#${source.effectivePriority + 1}`;
  return (
    <article
      className={`source-card ${source.preferred ? "preferred" : ""} ${source.enabled ? "" : "disabled"} ${dragging ? "dragging" : ""}`.trim()}
      draggable={!busy}
      onDragStart={(event) => {
        if (busy) { event.preventDefault(); return; }
        event.dataTransfer.effectAllowed = "move";
        onDragStart();
      }}
      onDragOver={(event) => { if (!busy) event.preventDefault(); }}
      onDrop={(event) => { event.preventDefault(); if (!busy) onDrop(); }}
      onDragEnd={onDragEnd}
      onDoubleClick={(event) => {
        const target = event.target;
        if (target instanceof HTMLElement && target.closest("button, input, label")) return;
        onEdit();
      }}
      aria-label={`${displayName}，用户顺序第 ${source.userPriority + 1}，实际优先级 ${effectivePriority}`}
    >
      <div className="source-order-column" title="拖动卡片调整偏好顺序">
        <span>{index + 1}</span>
        <small>偏好</small>
        <i aria-hidden="true">⋮⋮</i>
      </div>
      <div className="source-card-main">
        <div className="source-card-heading">
          {source.preferred && <span className="source-badge preferred">当前实际首选</span>}
          <span className={`source-badge ${source.enabled ? "enabled" : "disabled"}`}>{source.enabled ? "已启用" : "已禁用"}</span>
          {source.hasConfig && <span className="source-badge configured">有配置</span>}
          <SourceHealthIndicator health={source.health} />
        </div>
        <h3>{displayName}</h3>
        <p>{source.metadata.author || "未知作者"} · v{source.metadata.version || "?"}</p>
        <SourceCapabilityDetails capabilities={source.capabilities} />
        <div className="source-priority-summary">
          <span>用户顺序 <strong>#{source.userPriority + 1}</strong></span>
          <span>实际优先 <strong>{effectivePriority}</strong></span>
        </div>
      </div>
      <div className="source-actions" draggable={false} onDragStart={(event) => { event.preventDefault(); event.stopPropagation(); }} onDoubleClick={(event) => event.stopPropagation()}>
        <div className="source-order-buttons">
          <button type="button" disabled={busy || index === 0} onClick={() => onMove(-1)} aria-label={`上移 ${displayName}`} title="上移"><ArrowUp size={15} /></button>
          <button type="button" disabled={busy || index === total - 1} onClick={() => onMove(1)} aria-label={`下移 ${displayName}`} title="下移"><ArrowDown size={15} /></button>
        </div>
        <label className="source-enable-toggle"><input type="checkbox" role="switch" checked={source.enabled} disabled={busy} onChange={onToggle} aria-label={`启用 ${displayName}`} /><span>{source.enabled ? "已启用" : "已禁用"}</span></label>
        <button type="button" className="icon-button" disabled={busy} onClick={onEdit} title="编辑配置" aria-label={`编辑 ${displayName}`}><Pencil size={16} /></button>
        <button type="button" className="icon-button" disabled={busy} onClick={onReimport} title="重新导入" aria-label={`重新导入 ${displayName}`}><RefreshCw size={16} className={reimporting ? "spin" : ""} /></button>
        <button type="button" className="icon-button danger" disabled={busy} onClick={onRemove} title="删除音源" aria-label={`删除 ${displayName}`}><Trash2 size={16} /></button>
      </div>
    </article>
  );
}

function SourceCapabilityDetails({ capabilities }: Pick<ListedSource, "capabilities">) {
  const platforms = capabilities.map((capability) => capability.platform);
  const qualities = [...new Set(capabilities.flatMap((capability) => capability.qualities))];
  return <dl className="source-capabilities">
    <div><dt>平台</dt><dd>{platforms.length ? platforms.join(" / ") : "未提供"}</dd></div>
    <div><dt>音质</dt><dd>{qualities.length ? qualities.join(" / ") : "未提供"}</dd></div>
  </dl>;
}

function SourceHealthIndicator({ health }: Pick<ListedSource, "health">) {
  const stateLabels: Record<typeof health.state, string> = {
    unknown: "暂无样本",
    healthy: "稳定",
    degraded: "偶有波动",
    unhealthy: "近期失败较多",
  };
  const detail = health.sampleCount === 0
    ? "尚未发生可统计的真实解析调用"
    : `最近 ${health.sampleCount} 次：成功 ${health.successRatePercent ?? 0}% · 平均 ${health.averageLatencyMs ?? 0} ms · 最近一次 ${health.lastSuccess ? "成功" : "失败"}`;
  return (
    <span className={`source-health source-health-${health.state}`} title={`${stateLabels[health.state]}：${detail}`} aria-label={`音源健康度：${stateLabels[health.state]}`}>
      <i aria-hidden="true" />
      <span>{stateLabels[health.state]}</span>
    </span>
  );
}
