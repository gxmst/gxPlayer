type Props = {
  visible: boolean;
  title: string;
  detail?: string;
  onCancel: () => void;
};

/** Bottom-right resolving indicator with cancel (also covered by client timeout). */
export function ResolveBanner({ visible, title, detail, onCancel }: Props) {
  if (!visible) return null;
  return (
    <div className="resolve-banner" role="status" aria-live="polite">
      <div className="resolve-banner-copy">
        <strong>{title}</strong>
        {detail ? <small>{detail}</small> : null}
      </div>
      <i className="search-spinner" aria-hidden="true" />
      <button type="button" className="resolve-cancel" onClick={onCancel}>
        取消解析
      </button>
    </div>
  );
}
