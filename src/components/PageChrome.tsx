import { useId, type ReactNode } from "react";
import { FolderOpen, RotateCw, Search } from "lucide-react";

export function PageHeading({ eyebrow, title, copy, action }: { eyebrow?: string; title: string; copy?: string; action?: ReactNode }) {
  return <header className="page-heading">
    <div>{eyebrow && <p className="eyebrow">{eyebrow}</p>}<h1>{title}</h1>{copy && <p>{copy}</p>}</div>
    {action && <div className="page-heading-actions">{action}</div>}
  </header>;
}

export function TabBar<T extends string>({ label, value, items, onChange }: {
  label: string; value: T; items: ReadonlyArray<{ id: T; label: string; count?: number }>;
  onChange: (value: T) => void;
}) {
  const id = useId();
  return <div className="view-tabs" role="tablist" aria-label={label}>
    {items.map((item, index) => <button type="button" role="tab" key={item.id} id={`${id}-${item.id}`}
      aria-selected={value === item.id} tabIndex={value === item.id ? 0 : -1}
      onClick={() => onChange(item.id)} onKeyDown={(event) => {
        const next = event.key === "ArrowRight" ? (index + 1) % items.length
          : event.key === "ArrowLeft" ? (index + items.length - 1) % items.length
            : event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : null;
        if (next === null) return;
        event.preventDefault();
        onChange(items[next].id);
        document.getElementById(`${id}-${items[next].id}`)?.focus();
      }}>
      {item.label}{item.count !== undefined && <span className="tab-count">{item.count}</span>}
    </button>)}
  </div>;
}

export function EmptyState({ title, copy, action, onAction }: { title: string; copy?: string; action?: string; onAction?: () => void }) {
  return <div className="empty-state"><FolderOpen size={28} strokeWidth={1.5} aria-hidden="true" /><h3>{title}</h3>{copy && <p>{copy}</p>}
    {action && onAction && <button type="button" onClick={onAction}>{action}</button>}
  </div>;
}

export function ErrorState({ title, copy, onRetry }: { title: string; copy: string; onRetry: () => void }) {
  return <div className="empty-state error-state" role="alert"><h3>{title}</h3><p>{copy}</p><button type="button" onClick={onRetry}><RotateCw size={15} />重新读取</button></div>;
}

export function LoadingState() {
  return <div className="loading-state" role="status" aria-label="正在读取"><Search size={18} aria-hidden="true" /><span>正在读取…</span></div>;
}
