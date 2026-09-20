/**
 * The main window.
 *
 * Capture no longer starts here in any meaningful sense — it starts with the
 * global shortcut, the tray, or the one button below, all of which open the
 * same overlay where the mode is actually chosen. This window is the library and
 * the place results land.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import History from "./History";
import "./App.css";

interface CaptureRecord {
  fileName: string;
  path: string;
  width: number;
  height: number;
  copiedToClipboard: boolean;
  warnings: string[];
}

interface Settings {
  saveDirectory: string;
  format: "png" | "jpeg";
  clipboard: { autoCopy: boolean };
  shortcuts: {
    capture: string;
    region: string;
    fullScreen: string;
    activeWindow: string;
  };
}

/** Turn an accelerator string into something readable on Windows. */
function prettyShortcut(accelerator: string): string {
  return accelerator
    .replace(/CommandOrControl/gi, "Ctrl")
    .replace(/\+/g, " + ");
}

export default function App() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [last, setLast] = useState<CaptureRecord | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  /** Bumped after each capture so the library refetches. */
  const [refreshToken, setRefreshToken] = useState(0);

  useEffect(() => {
    void (async () => {
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
    })();
  }, []);

  useEffect(() => {
    const completed = listen<CaptureRecord>("capture-complete", (event) => {
      setLast(event.payload);
      setError(null);
      setRefreshToken((token) => token + 1);
    });
    const failed = listen<string>("capture-failed", (event) => {
      setError(event.payload);
    });

    return () => {
      void completed.then((un) => un());
      void failed.then((un) => un());
    };
  }, []);

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
          <button type="button" className="primary" onClick={() => void newCapture()}>
            New capture
          </button>
          {settings && (
            <span className="app__shortcut">
              or press {prettyShortcut(settings.shortcuts.capture)} anywhere
            </span>
          )}
        </div>
      </header>

      {error && <p className="banner banner--error">{error}</p>}

      {warnings.length > 0 && (
        <div className="banner banner--warn">
          {warnings.map((warning) => (
            <p key={warning}>{warning}</p>
          ))}
        </div>
      )}

      {last && (
        <p className="banner banner--ok">
          Saved {last.fileName} · {last.width} x {last.height}
          {last.copiedToClipboard ? " · copied to clipboard" : ""}
          {last.warnings.length > 0 ? ` · ${last.warnings.join(" ")}` : ""}
        </p>
      )}

      <History refreshToken={refreshToken} />
    </main>
  );
}
