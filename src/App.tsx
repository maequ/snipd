/**
 * The main window.
 *
 * Capture does not really start here — it starts with the global shortcut, the
 * tray, or the one button below, all of which open the same overlay where the
 * mode is chosen. This window is the library, the editor, and settings.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import History, { type HistoryEntry } from "./History";
import Editor from "./editor";
import SettingsPanel, { type Settings } from "./Settings";
import Stats from "./Stats";
import "./App.css";

interface CaptureRecord {
  fileName: string;
  path: string;
  width: number;
  height: number;
  copiedToClipboard: boolean;
  warnings: string[];
  fullUrl: string;
}

interface RecordingOutcome {
  fileName: string;
  width: number;
  height: number;
  durationMs: number;
  dropped: number;
}

type Tab = "library" | "recordings" | "settings";

const TABS: Array<{ id: Tab; label: string }> = [
  { id: "library", label: "Library" },
  { id: "recordings", label: "Recordings" },
  { id: "settings", label: "Settings" },
];

/** Turn an accelerator string into something readable on Windows. */
function prettyShortcut(accelerator: string): string {
  return accelerator.replace(/CommandOrControl/gi, "Ctrl").replace(/\+/g, " + ");
}

export default function App() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [tab, setTab] = useState<Tab>("library");
  const [last, setLast] = useState<CaptureRecord | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [refreshToken, setRefreshToken] = useState(0);
  const [recording, setRecording] = useState(false);

  /**
   * The capture open in the editor.
   *
   * The editor is a view in this window rather than a window of its own: a
   * separately created window would not load its bundle at all and rendered
   * blank, and this side-steps that entirely.
   */
  const [editing, setEditing] = useState<HistoryEntry | null>(null);
  /** True when the editor was opened by a capture rather than from the library. */
  const [reviewing, setReviewing] = useState(false);

  const loadSettings = useCallback(async () => {
    try {
      const [config, shortcutWarnings] = await Promise.all([
        invoke<Settings>("get_settings"),
        invoke<string[]>("get_shortcut_warnings"),
      ]);
      setSettings(config);
      setWarnings(shortcutWarnings);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  // Apply the theme by flipping an attribute the stylesheet keys off, so
  // "Match Windows" simply means leaving the OS preference to decide.
  useEffect(() => {
    const root = document.documentElement;
    if (!settings || settings.theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", settings.theme);
  }, [settings]);

  useEffect(() => {
    const completed = listen<CaptureRecord>("capture-complete", (event) => {
      const record = event.payload;
      setLast(record);
      setError(null);
      setRefreshToken((token) => token + 1);

      // The capture is already on disk either way. Reviewing only decides
      // whether it opens for mark-up and renaming first.
      if (settings?.capture.after === "review" && record.fullUrl) {
        setReviewing(true);
        setEditing({
          path: record.path,
          fileName: record.fileName,
          takenAtMs: Date.now(),
          bytes: 0,
          width: record.width,
          height: record.height,
          kind: null,
          source: null,
          thumbnailUrl: "",
          fullUrl: record.fullUrl,
          isVideo: false,
          durationMs: null,
        });
      }
    });

    const failed = listen<string>("capture-failed", (event) => setError(event.payload));

    const recorded = listen<RecordingOutcome>("recording-complete", (event) => {
      setRecording(false);
      setRefreshToken((token) => token + 1);
      setTab("recordings");
      const { fileName, durationMs, dropped } = event.payload;
      const seconds = Math.round(durationMs / 1000);
      setLast(null);
      setError(
        dropped > 0
          ? `Saved ${fileName} (${seconds}s) — ${dropped} frames were dropped. Lower the resolution or frame rate in Settings if that keeps happening.`
          : null,
      );
    });

    return () => {
      void completed.then((un) => un());
      void failed.then((un) => un());
      void recorded.then((un) => un());
    };
  }, [settings]);

  const newCapture = useCallback(async () => {
    setError(null);
    try {
      // Rust hides this window before freezing the screen, so it does not end
      // up in its own screenshot.
      await invoke("begin_capture", {});
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const closeEditor = useCallback(() => {
    setEditing(null);
    setReviewing(false);
    setRefreshToken((token) => token + 1);
  }, []);

  if (editing) {
    return (
      <Editor
        imageUrl={editing.fullUrl}
        fileName={editing.fileName}
        path={editing.path}
        reviewing={reviewing}
        onClose={closeEditor}
      />
    );
  }

  return (
    <main className="app">
      <header className="app__header">
        <div>
          <h1>Snipd</h1>
          {settings && (
            <p className="app__subtitle">
              Saving to {settings.saveDirectory} · {settings.format.toUpperCase()} ·{" "}
              {settings.clipboard.autoCopy ? "auto-copying" : "not auto-copying"}
            </p>
          )}
        </div>

        <div className="app__actions">
          <button
            type="button"
            className="primary"
            onClick={() => void newCapture()}
            disabled={recording}
          >
            New capture
          </button>
          {settings && (
            <span className="app__shortcut">
              or press {prettyShortcut(settings.shortcuts.capture)} anywhere
            </span>
          )}
        </div>
      </header>

      <nav className="tabs">
        {TABS.map((entry) => (
          <button
            key={entry.id}
            type="button"
            aria-pressed={tab === entry.id}
            onClick={() => setTab(entry.id)}
          >
            {entry.label}
          </button>
        ))}
      </nav>

      {error && <p className="banner banner--error">{error}</p>}

      {warnings.length > 0 && tab !== "settings" && (
        <div className="banner banner--warn">
          {warnings.map((warning) => (
            <p key={warning}>{warning}</p>
          ))}
        </div>
      )}

      {last && tab === "library" && (
        <p className="banner banner--ok">
          Saved {last.fileName} · {last.width} x {last.height}
          {last.copiedToClipboard ? " · copied to clipboard" : ""}
          {last.warnings.length > 0 ? ` · ${last.warnings.join(" ")}` : ""}
        </p>
      )}

      {tab !== "settings" && <Stats refreshToken={refreshToken} />}

      {tab === "library" && (
        <History
          media="image"
          refreshToken={refreshToken}
          onEdit={(entry) => {
            setReviewing(false);
            setEditing(entry);
          }}
        />
      )}

      {tab === "recordings" && (
        <History
          media="video"
          refreshToken={refreshToken}
          onEdit={(entry) => {
            setReviewing(false);
            setEditing(entry);
          }}
        />
      )}

      {tab === "settings" &&
        (settings ? (
          <SettingsPanel initial={settings} onSaved={setSettings} />
        ) : (
          <p className="library__empty">Loading settings…</p>
        ))}
    </main>
  );
}
