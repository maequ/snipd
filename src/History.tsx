/**
 * The capture library.
 *
 * Thumbnails are served as ordinary image URLs and marked `loading="lazy"`, and
 * rows are fetched a page at a time, so a folder with thousands of captures
 * never decodes more than what is actually on screen.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface HistoryEntry {
  path: string;
  fileName: string;
  takenAtMs: number;
  bytes: number;
  width: number | null;
  height: number | null;
  kind: "fullScreen" | "activeWindow" | "region" | "freeform" | null;
  source: string | null;
  thumbnailUrl: string;
}

interface HistoryPage {
  entries: HistoryEntry[];
  total: number;
}

type DateFilter = "all" | "today" | "week" | "month";

/** Rows fetched per request. */
const PAGE_SIZE = 60;

const KIND_LABELS: Record<string, string> = {
  fullScreen: "Full screen",
  activeWindow: "Window",
  region: "Region",
  freeform: "Freeform",
};

const DATE_LABELS: Record<DateFilter, string> = {
  all: "All time",
  today: "Today",
  week: "Last 7 days",
  month: "Last 30 days",
};

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatWhen(ms: number): string {
  const date = new Date(ms);
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return sameDay ? `Today ${time}` : `${date.toLocaleDateString()} ${time}`;
}

/** Start of the window a date filter covers, in epoch milliseconds. */
function filterFrom(filter: DateFilter): number | null {
  if (filter === "all") return null;
  const start = new Date();
  start.setHours(0, 0, 0, 0);
  if (filter === "today") return start.getTime();
  const days = filter === "week" ? 7 : 30;
  return start.getTime() - days * 24 * 60 * 60 * 1000;
}

export default function History({ refreshToken }: { refreshToken: number }) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [total, setTotal] = useState(0);
  const [search, setSearch] = useState("");
  const [dateFilter, setDateFilter] = useState<DateFilter>("all");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  // Debounced so typing in the search box does not fire a request per keystroke.
  const [debouncedSearch, setDebouncedSearch] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedSearch(search), 200);
    return () => window.clearTimeout(timer);
  }, [search]);

  const fetchPage = useCallback(
    async (offset: number, replace: boolean) => {
      setLoading(true);
      try {
        const page = await invoke<HistoryPage>("history_list", {
          query: {
            search: debouncedSearch || null,
            fromMs: filterFrom(dateFilter),
            toMs: null,
            offset,
            limit: PAGE_SIZE,
          },
        });
        setTotal(page.total);
        setEntries((current) => (replace ? page.entries : [...current, ...page.entries]));
        setError(null);
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    },
    [debouncedSearch, dateFilter],
  );

  // Filters changing resets to the first page; refreshToken bumps after a new
  // capture so the grid picks it up.
  useEffect(() => {
    void fetchPage(0, true);
  }, [fetchPage, refreshToken]);

  // Load the next page when the sentinel at the end of the grid scrolls in.
  const sentinel = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const node = sentinel.current;
    if (!node || entries.length >= total || loading) return;

    const observer = new IntersectionObserver((records) => {
      if (records.some((record) => record.isIntersecting)) {
        void fetchPage(entries.length, false);
      }
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [entries.length, total, loading, fetchPage]);

  const open = useCallback(async (path: string) => {
    try {
      await invoke("open_editor", { path });
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const copy = useCallback(async (path: string) => {
    try {
      await invoke("copy_capture", { path });
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const reveal = useCallback(async (path: string) => {
    try {
      await invoke("reveal_in_explorer", { path });
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const remove = useCallback(async (path: string) => {
    try {
      await invoke("history_delete", { path });
      setEntries((current) => current.filter((entry) => entry.path !== path));
      setTotal((current) => Math.max(0, current - 1));
      setPendingDelete(null);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  return (
    <section className="library">
      <div className="library__bar">
        <input
          type="search"
          className="library__search"
          placeholder="Search filenames"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />

        <div className="library__filters">
          {(Object.keys(DATE_LABELS) as DateFilter[]).map((key) => (
            <button
              key={key}
              type="button"
              className="chip"
              aria-pressed={dateFilter === key}
              onClick={() => setDateFilter(key)}
            >
              {DATE_LABELS[key]}
            </button>
          ))}
        </div>
      </div>

      {error && <p className="library__error">{error}</p>}

      {!loading && entries.length === 0 ? (
        <p className="library__empty">
          {search || dateFilter !== "all"
            ? "No captures match those filters."
            : "No captures yet. Everything you capture appears here automatically."}
        </p>
      ) : (
        <>
          <p className="library__count">
            {entries.length === total
              ? `${total} capture${total === 1 ? "" : "s"}`
              : `Showing ${entries.length} of ${total}`}
          </p>

          <ul className="grid">
            {entries.map((entry) => (
              <li key={entry.path} className="tile">
                <button
                  type="button"
                  className="tile__image"
                  title="Open in the editor"
                  onClick={() => void open(entry.path)}
                >
                  <img src={entry.thumbnailUrl} alt={entry.fileName} loading="lazy" />
                </button>

                <div className="tile__meta">
                  <p className="tile__name" title={entry.fileName}>
                    {entry.fileName}
                  </p>
                  <p className="tile__when">
                    {formatWhen(entry.takenAtMs)}
                    {entry.kind ? ` · ${KIND_LABELS[entry.kind] ?? entry.kind}` : ""}
                  </p>
                  <p className="tile__size">
                    {entry.width && entry.height ? `${entry.width} x ${entry.height} · ` : ""}
                    {formatBytes(entry.bytes)}
                  </p>
                </div>

                <div className="tile__actions">
                  <button type="button" onClick={() => void copy(entry.path)}>
                    Copy
                  </button>
                  <button type="button" onClick={() => void reveal(entry.path)}>
                    Show in folder
                  </button>
                  {pendingDelete === entry.path ? (
                    <>
                      <button
                        type="button"
                        className="danger"
                        onClick={() => void remove(entry.path)}
                      >
                        Delete for good
                      </button>
                      <button type="button" onClick={() => setPendingDelete(null)}>
                        Keep
                      </button>
                    </>
                  ) : (
                    <button type="button" onClick={() => setPendingDelete(entry.path)}>
                      Delete
                    </button>
                  )}
                </div>
              </li>
            ))}
          </ul>

          <div ref={sentinel} className="library__sentinel">
            {loading ? "Loading…" : ""}
          </div>
        </>
      )}
    </section>
  );
}
