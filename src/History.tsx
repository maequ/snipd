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
  fullUrl: string;
  isVideo: boolean;
  durationMs: number | null;
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
  edited: "Edited",
  recording: "Recording",
};

/** `2:07` — recordings are short enough that hours never come up. */
function formatDuration(ms: number | null): string {
  if (!ms) return "";
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

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

export default function History({
  refreshToken,
  onEdit,
  media,
  confirmDelete,
  onStopConfirming,
}: {
  refreshToken: number;
  /** Hand a capture to the editor, which lives in the main window. */
  onEdit: (entry: HistoryEntry) => void;
  /** Which tab this is: stills or recordings. */
  media: "image" | "video";
  /** Whether deleting asks first. Mirrors the setting of the same name. */
  confirmDelete: boolean;
  /** Called when "Don't ask again" is ticked, so the setting can be saved. */
  onStopConfirming: () => void;
}) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [total, setTotal] = useState(0);
  const [search, setSearch] = useState("");
  const [dateFilter, setDateFilter] = useState<DateFilter>("all");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** The capture the confirmation dialog is asking about. */
  const [pendingDelete, setPendingDelete] = useState<HistoryEntry | null>(null);
  /** Ticked inside the dialog; applied only if the delete goes ahead. */
  const [stopAsking, setStopAsking] = useState(false);
  /** The capture open in the viewer, if any. */
  const [viewing, setViewing] = useState<HistoryEntry | null>(null);

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
            media,
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
    [debouncedSearch, dateFilter, media],
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

  const pin = useCallback(async (path: string) => {
    try {
      await invoke("pin_capture", { path });
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
          {search || dateFilter !== "all" ? (
            <>No {media === "video" ? "recordings" : "captures"} match those filters.</>
          ) : media === "video" ? (
            <>
              No recordings yet.
              <br />
              <span className="library__teach">
                Open the capture overlay, choose <strong>Record</strong>, then drag the area you
                want.
              </span>
            </>
          ) : (
            <>
              No captures yet.
              <br />
              <span className="library__teach">
                Press <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>S</kbd> anywhere, or use the tray
                icon. Everything you capture is saved here automatically.
              </span>
            </>
          )}
        </p>
      ) : (
        <>
          <p className="library__count">
            {entries.length === total
              ? `${total} ${media === "video" ? "recording" : "capture"}${total === 1 ? "" : "s"}`
              : `Showing ${entries.length} of ${total}`}
          </p>

          <ul className="grid">
            {entries.map((entry) => (
              <li key={entry.path} className="tile">
                <button
                  type="button"
                  className="tile__image"
                  title={entry.isVideo ? "Play this recording" : "View this capture"}
                  onClick={() => setViewing(entry)}
                >
                  {entry.isVideo ? (
                    <span className="tile__video">
                      <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                        <path d="M8 5.5v13l11-6.5z" />
                      </svg>
                      <span>{formatDuration(entry.durationMs) || "Recording"}</span>
                    </span>
                  ) : (
                    <img src={entry.thumbnailUrl} alt={entry.fileName} loading="lazy" />
                  )}
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
                  <button
                    type="button"
                    className="danger"
                    onClick={() => {
                      if (confirmDelete) {
                        setStopAsking(false);
                        setPendingDelete(entry);
                      } else {
                        void remove(entry.path);
                      }
                    }}
                  >
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>

          <div ref={sentinel} className="library__sentinel">
            {loading ? "Loading…" : ""}
          </div>
        </>
      )}

      {pendingDelete && (
        <ConfirmDelete
          entry={pendingDelete}
          stopAsking={stopAsking}
          onStopAskingChange={setStopAsking}
          onCancel={() => setPendingDelete(null)}
          onConfirm={() => {
            const path = pendingDelete.path;
            if (stopAsking) onStopConfirming();
            setPendingDelete(null);
            void remove(path);
          }}
        />
      )}

      {viewing && (
        <Viewer
          entry={viewing}
          onClose={() => setViewing(null)}
          onEdit={() => {
            onEdit(viewing);
            setViewing(null);
          }}
          onCopy={() => void copy(viewing.path)}
          onReveal={() => void reveal(viewing.path)}
          onPin={() => {
            void pin(viewing.path);
            setViewing(null);
          }}
        />
      )}
    </section>
  );
}

/** Full-size preview of one capture, with what to do next. */
function Viewer({
  entry,
  onClose,
  onEdit,
  onCopy,
  onReveal,
  onPin,
}: {
  entry: HistoryEntry;
  onClose: () => void;
  onEdit: () => void;
  onCopy: () => void;
  onReveal: () => void;
  onPin: () => void;
}) {
  // Escape closes, as it does in every image viewer.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    // Clicking the backdrop closes; clicks inside the panel must not bubble up
    // to it, or choosing a button would dismiss the viewer.
    <div className="viewer" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="viewer__panel" onClick={(e) => e.stopPropagation()}>
        <div className="viewer__stage">
          {entry.isVideo ? (
            <video src={entry.fullUrl} controls autoPlay className="viewer__video" />
          ) : (
            // The frame matters: a capture that is a single flat colour is
            // indistinguishable from a failed load without a visible boundary.
            <img className="viewer__image" src={entry.fullUrl} alt={entry.fileName} />
          )}
        </div>

        <div className="viewer__side">
          <p className="viewer__name">{entry.fileName}</p>

          <dl className="viewer__facts">
            <dt>Taken</dt>
            <dd>{formatWhen(entry.takenAtMs)}</dd>

            <dt>Mode</dt>
            <dd>{entry.kind ? (KIND_LABELS[entry.kind] ?? entry.kind) : "Unknown"}</dd>

            {entry.durationMs ? (
              <>
                <dt>Length</dt>
                <dd>{formatDuration(entry.durationMs)}</dd>
              </>
            ) : null}

            {entry.width && entry.height ? (
              <>
                <dt>Size</dt>
                <dd>
                  {entry.width} x {entry.height} px
                </dd>
              </>
            ) : null}

            <dt>File</dt>
            <dd>{formatBytes(entry.bytes)}</dd>

            {entry.source ? (
              <>
                <dt>Source</dt>
                <dd>{entry.source}</dd>
              </>
            ) : null}
          </dl>

          <div className="viewer__actions">
            {!entry.isVideo && (
              <>
                <button type="button" className="primary" onClick={onEdit}>
                  Open in editor
                </button>
                <button type="button" onClick={onPin}>
                  Pin on top
                </button>
                <button type="button" onClick={onCopy}>
                  Copy to clipboard
                </button>
              </>
            )}
            <button type="button" onClick={onReveal}>
              Show in folder
            </button>
            <button type="button" onClick={onClose}>
              Close
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}


/**
 * Confirmation before a capture is removed.
 *
 * A real dialog rather than the button quietly turning into a second button.
 * That pattern put "delete for good" exactly where the mouse already was, which
 * is precisely how an accidental double-click destroys something.
 *
 * It also says where the file is going. "Delete" and "moved to the Recycle Bin,
 * where you can get it back" are very different promises, and only one of them
 * is true here.
 */
function ConfirmDelete({
  entry,
  stopAsking,
  onStopAskingChange,
  onCancel,
  onConfirm,
}: {
  entry: HistoryEntry;
  stopAsking: boolean;
  onStopAskingChange: (value: boolean) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  // Escape cancels, so the dialog can never be a trap.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <div className="dialog" role="presentation" onClick={onCancel}>
      <div
        className="dialog__panel"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-delete-title"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 id="confirm-delete-title">Delete this {entry.isVideo ? "recording" : "capture"}?</h2>
        <p className="dialog__name">{entry.fileName}</p>
        <p className="dialog__body">
          It will be moved to the Recycle Bin, so you can still get it back.
        </p>

        <label className="dialog__again">
          <input
            type="checkbox"
            checked={stopAsking}
            onChange={(event) => onStopAskingChange(event.target.checked)}
          />
          <span>Don&rsquo;t ask me again</span>
        </label>

        <div className="dialog__actions">
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="primary" onClick={onConfirm} autoFocus>
            Move to Recycle Bin
          </button>
        </div>
      </div>
    </div>
  );
}
