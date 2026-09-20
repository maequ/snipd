/**
 * Phase 1 shell.
 *
 * Deliberately plain: this exists to exercise the capture engine and show that
 * every capture lands on disk with the right name in the right place. The visual
 * identity, history grid, annotation editor and settings all arrive in later
 * phases and will replace most of this.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import "./App.css";

interface MonitorInfo {
  id: string;
  label: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scaleFactor: number;
  isPrimary: boolean;
}

interface CaptureRecord {
  id: string;
  path: string;
  fileName: string;
  width: number;
  height: number;
  takenAt: string;
  kind: "fullScreen" | "activeWindow" | "region";
  source: string | null;
  bytes: number;
  copiedToClipboard: boolean;
  warnings: string[];
}

interface Settings {
  saveDirectory: string;
  format: "png" | "jpeg";
  naming: {
    mode: "datetime" | "prefix";
    prefix: string;
    counter: number;
  };
  clipboard: { autoCopy: boolean };
}

/** `1.4 MB` — captures are big enough that bytes alone are unreadable. */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const KIND_LABELS: Record<CaptureRecord["kind"], string> = {
  fullScreen: "Full screen",
  activeWindow: "Window",
  region: "Region",
};

export default function App() {
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [selectedMonitor, setSelectedMonitor] = useState<string>("all");
  const [last, setLast] = useState<CaptureRecord | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [displays, config] = await Promise.all([
        invoke<MonitorInfo[]>("list_monitors"),
        invoke<Settings>("get_settings"),
      ]);
      setMonitors(displays);
      setSettings(config);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Region captures finish in the overlay window, so their result arrives as an
  // event rather than as the return value of a call this window made.
  useEffect(() => {
    const completed = listen<CaptureRecord>("capture-complete", (event) => {
      setLast(event.payload);
      setError(null);
      setBusy(false);
      void refresh();
    });
    const failed = listen<string>("capture-failed", (event) => {
      setError(event.payload);
      setBusy(false);
    });

    return () => {
      void completed.then((un) => un());
      void failed.then((un) => un());
    };
  }, [refresh]);

  const runCapture = useCallback(
    async (request: Record<string, unknown>) => {
      setBusy(true);
      setError(null);
      try {
        const record = await invoke<CaptureRecord>("capture", { request });
        setLast(record);
        await refresh();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  const captureFullScreen = useCallback(() => {
    void runCapture({
      mode: "fullScreen",
      monitorId: selectedMonitor === "all" ? null : selectedMonitor,
    });
  }, [runCapture, selectedMonitor]);

  const captureWindow = useCallback(() => {
    void runCapture({ mode: "activeWindow" });
  }, [runCapture]);

  const captureRegion = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("begin_region_capture");
      // The overlay takes over from here; `capture-complete` or a cancellation
      // ends the busy state.
      window.setTimeout(() => setBusy(false), 500);
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }, []);

  const reveal = useCallback(async (path: string) => {
    try {
      await invoke("reveal_in_explorer", { path });
    } catch (err) {
      setError(String(err));
    }
  }, []);

  return (
    <main className="app">
      <header className="app__header">
        <h1>Snipd</h1>
        <p className="app__subtitle">
          Phase 1 — capture engine. Every capture is written to disk the instant it is taken.
        </p>
      </header>

      <section className="panel">
        <h2>Capture</h2>

        <div className="controls">
          <label className="field">
            <span>Display</span>
            <select
              value={selectedMonitor}
              onChange={(event) => setSelectedMonitor(event.target.value)}
            >
              <option value="all">All displays (entire desktop)</option>
              {monitors.map((monitor) => (
                <option key={monitor.id} value={monitor.id}>
                  {monitor.label}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="actions">
          <button type="button" onClick={captureFullScreen} disabled={busy}>
            Full screen
          </button>
          <button type="button" onClick={captureWindow} disabled={busy}>
            Active window
          </button>
          <button type="button" onClick={() => void captureRegion()} disabled={busy}>
            Region
          </button>
        </div>
      </section>

      {error && (
        <section className="panel panel--error">
          <h2>Capture failed</h2>
          <p>{error}</p>
        </section>
      )}

      {last && (
        <section className="panel">
          <h2>Last capture</h2>
          <dl className="details">
            <dt>File</dt>
            <dd>{last.fileName}</dd>

            <dt>Mode</dt>
            <dd>
              {KIND_LABELS[last.kind]}
              {last.source ? ` — ${last.source}` : ""}
            </dd>

            <dt>Size</dt>
            <dd>
              {last.width} x {last.height} px · {formatBytes(last.bytes)}
            </dd>

            <dt>Clipboard</dt>
            <dd>{last.copiedToClipboard ? "Copied" : "Not copied"}</dd>

            <dt>Saved to</dt>
            <dd className="details__path">{last.path}</dd>
          </dl>

          {last.warnings.length > 0 && (
            <ul className="warnings">
              {last.warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
          )}

          <div className="actions">
            <button type="button" onClick={() => void reveal(last.path)}>
              Show in folder
            </button>
          </div>
        </section>
      )}

      {settings && (
        <section className="panel panel--quiet">
          <h2>Current configuration</h2>
          <dl className="details">
            <dt>Save folder</dt>
            <dd className="details__path">{settings.saveDirectory}</dd>

            <dt>Format</dt>
            <dd>{settings.format.toUpperCase()}</dd>

            <dt>Naming</dt>
            <dd>
              {settings.naming.mode === "datetime"
                ? "Date and time"
                : `Prefix "${settings.naming.prefix}", next number ${settings.naming.counter}`}
            </dd>

            <dt>Auto-copy</dt>
            <dd>{settings.clipboard.autoCopy ? "On" : "Off"}</dd>
          </dl>
        </section>
      )}
    </main>
  );
}
