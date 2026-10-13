import { memo, useMemo, useState } from "react";
import { File, Folder, Search, X } from "../icons";
import type { Selection } from "../types";
import { size } from "../selection";

export const FileQueue = memo(function FileQueue({
  selection,
  disabled,
  change,
}: {
  selection: Selection;
  disabled: boolean;
  change: (selection: Selection | null) => void;
}) {
  const [filter, setFilter] = useState("");
  const [limit, setLimit] = useState(100);
  const matching = useMemo(() => {
    const query = filter.toLowerCase();
    return query
      ? selection.items.filter((item) => item.path.toLowerCase().includes(query))
      : selection.items;
  }, [selection, filter]);
  function remove(path: string) {
    const items = selection.items.filter(
      (item) => item.path !== path && !item.path.startsWith(path + "/"),
    );
    change(items.length ? { ...selection, items } : null);
  }
  return (
    <div className="queue">
      <div className="queue-heading">
        <strong>
          Selected items <span>{selection.items.length}</span>
        </strong>
        <button id="clear" className="quiet" disabled={disabled} onClick={() => change(null)}>
          Clear all
        </button>
      </div>
      {selection.items.length > 6 && (
        <label className="queue-search">
          <Search size={15} />
          <span className="sr-only">Filter selected items</span>
          <input
            placeholder="Find a file…"
            value={filter}
            onChange={(event) => {
              setFilter(event.target.value);
              setLimit(100);
            }}
          />
        </label>
      )}
      <ul className="file-queue" aria-label="Selected items">
        {matching.slice(0, limit).map((item, index) => (
          <li key={`${index}:${item.path}`}>
            <span className={`file-symbol ${item.directory ? "folder" : ""}`}>
              {item.directory ? <Folder size={17} /> : <File size={17} />}
            </span>
            <div>
              <strong title={item.path}>{item.path.split("/").at(-1)}</strong>
              <small title={item.path}>{item.path.includes("/") ? item.path : "File"}</small>
            </div>
            <span className="file-size">
              {item.directory ? "Folder" : size(item.file?.size ?? 0)}
            </span>
            {item.path !== selection.root && (
              <button
                className="icon-button"
                aria-label={`Remove ${item.path}`}
                disabled={disabled}
                onClick={() => remove(item.path)}
              >
                <X size={14} />
              </button>
            )}
          </li>
        ))}
      </ul>
      {!matching.length && <p className="queue-empty">No items match “{filter}”.</p>}
      {matching.length > limit && (
        <button className="queue-more quiet" onClick={() => setLimit((value) => value + 100)}>
          Show more · {matching.length - limit} remaining
        </button>
      )}
    </div>
  );
});
