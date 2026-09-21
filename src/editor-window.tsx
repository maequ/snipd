/**
 * The editor, as its own window.
 *
 * Snipd's editor behaves like the Snipping Tool's: taking a capture puts a
 * compact editor in front of you, and the library window is left alone rather
 * than being turned into an editor and back again.
 *
 * Which capture to show comes from Rust rather than from the URL. A path in a
 * query string would have to be escaped correctly for every Windows path that
 * exists, and would let anything that could reach this page name an arbitrary
 * file; asking the backend avoids both problems.
 */

import { StrictMode, useCallback, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

import Editor from "./editor";
import "./tokens.css";

interface EditorTarget {
  path: string;
  fileName: string;
  imageUrl: string;
  /** True when this opened from a fresh capture rather than the library. */
  reviewing: boolean;
  theme: "system" | "light" | "dark";
}

function EditorWindow() {
  const [target, setTarget] = useState<EditorTarget | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void invoke<EditorTarget>("editor_target")
      .then((value) => {
        if (cancelled) return;
        setTarget(value);
        const root = document.documentElement;
        if (value.theme === "system") root.removeAttribute("data-theme");
        else root.setAttribute("data-theme", value.theme);
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Closing the editor closes its window. The library window is a separate
  // thing that was never disturbed.
  const close = useCallback((savedPath?: string) => {
    void invoke("editor_finished", { savedPath: savedPath ?? null }).catch(
      () => undefined,
    );
    void getCurrentWindow().close();
  }, []);

  if (error) {
    return (
      <main style={{ padding: 24 }}>
        <p className="banner banner--error">{error}</p>
        <button type="button" onClick={() => close()}>
          Close
        </button>
      </main>
    );
  }

  if (!target) {
    return <p style={{ padding: 24, color: "var(--text-muted)" }}>Loading the capture…</p>;
  }

  return (
    <Editor
      imageUrl={target.imageUrl}
      fileName={target.fileName}
      path={target.path}
      reviewing={target.reviewing}
      onClose={close}
    />
  );
}

const container = document.getElementById("root");
if (container) {
  // Tells the fallback in editor.html that the bundle really did run, so it
  // does not replace a working editor with a failure notice.
  container.setAttribute("data-mounted", "yes");
  createRoot(container).render(
    <StrictMode>
      <EditorWindow />
    </StrictMode>,
  );
}
