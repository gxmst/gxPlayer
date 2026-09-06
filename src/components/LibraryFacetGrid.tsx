type LibraryFacetGridProps = {
  items: Array<[string, number]>;
  onSelect: (name: string) => void;
  emptyLabel: string;
};

export function LibraryFacetGrid({ items, onSelect, emptyLabel }: LibraryFacetGridProps) {
  return (
    <div className="library-facet-grid">
      {items.map(([name, count]) => (
        <button
          type="button"
          key={name}
          onClick={() => onSelect(name === emptyLabel ? "" : name)}
        >
          <strong>{name}</strong>
          <small>{count} 首</small>
        </button>
      ))}
    </div>
  );
}
