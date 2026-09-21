/**
 * A quiet summary strip above the library.
 *
 * It exists because an app whose window is mostly an empty grid says nothing
 * about itself. This gives it something true to say from the first capture
 * onwards, and makes the retention setting concrete — "1.2 GB across 340
 * captures" is a reason to think about it; an abstract toggle is not.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface LibraryStats {
  captures: number;
  recordings: number;
  bytes: number;
  thisWeek: number;
  oldestMs: number | null;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** "since March" reads better than a date nobody will parse. */
function formatSince(ms: number | null): string {
  if (!ms) return "—";
  const date = new Date(ms);
  const now = new Date();
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString(undefined, { month: "long" });
  }
  return date.toLocaleDateString(undefined, { month: "short", year: "numeric" });
}

export default function Stats({ refreshToken }: { refreshToken: number }) {
  const [stats, setStats] = useState<LibraryStats | null>(null);

  const load = useCallback(async () => {
    try {
      setStats(await invoke<LibraryStats>("library_stats"));
    } catch {
      // The strip is decoration over the real content; failing to load it is
      // not worth an error banner.
      setStats(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, refreshToken]);

  // Nothing captured yet means nothing worth saying.
  if (!stats || stats.captures + stats.recordings === 0) return null;

  return (
    <div className="stats">
      <div className="stats__item">
        <span className="stats__value">{stats.captures.toLocaleString()}</span>
        <span className="stats__label">capture{stats.captures === 1 ? "" : "s"}</span>
      </div>

      {stats.recordings > 0 && (
        <div className="stats__item">
          <span className="stats__value">{stats.recordings.toLocaleString()}</span>
          <span className="stats__label">recording{stats.recordings === 1 ? "" : "s"}</span>
        </div>
      )}

      <div className="stats__item">
        <span className="stats__value">{formatBytes(stats.bytes)}</span>
        <span className="stats__label">on disk</span>
      </div>

      <div className="stats__item">
        <span className="stats__value">{stats.thisWeek.toLocaleString()}</span>
        <span className="stats__label">this week</span>
      </div>

      <div className="stats__item stats__item--end">
        <span className="stats__value">{formatSince(stats.oldestMs)}</span>
        <span className="stats__label">oldest</span>
      </div>
    </div>
  );
}
