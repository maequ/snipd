/**
 * Settings.
 *
 * Everything the installer wizard asks on first run is editable here too — the
 * wizard is only where these are *first* set, never the only place.
 *
 * Changes are applied on save rather than per-keystroke, because several of them
 * have real side effects (re-registering global shortcuts, adding or removing a
 * Windows startup entry, creating a folder) that should not fire while someone
 * is still typing.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

export interface Settings {
  version: number;
  saveDirectory: string;
  format: "png" | "jpeg";
  jpegQuality: number;
  theme: "system" | "light" | "dark";
  naming: {
    mode: "datetime" | "prefix";
    datetimePattern: string;
    prefix: string;
    counter: number;
    counterPadding: number;
  };
  clipboard: { autoCopy: boolean };
  startup: { launchOnLogin: boolean; startMinimised: boolean };
  window: { closeToTray: boolean };
  notifications: { showSavedToast: boolean };
  shortcuts: {
    capture: string;
    region: string;
    fullScreen: string;
    activeWindow: string;
  };
  retention: { enabled: boolean; days: number };
}

const GITHUB_URL = "https://github.com/snipd-app/snipd";
const APP_VERSION = "0.1.0";

const SHORTCUT_LABELS: Array<{ key: keyof Settings["shortcuts"]; label: string; hint: string }> = [
  { key: "capture", label: "New capture", hint: "Opens the overlay, where you pick the mode" },
  { key: "region", label: "Region", hint: "Opens the overlay with the rectangle tool selected" },
  { key: "fullScreen", label: "Full screen", hint: "Captures immediately, no overlay" },
  { key: "activeWindow", label: "Active window", hint: "Captures immediately, no overlay" },
];

export default function SettingsPanel({
  initial,
  onSaved,
}: {
  initial: Settings;
  onSaved: (settings: Settings) => void;
}) {
  const [draft, setDraft] = useState<Settings>(initial);
  const [preview, setPreview] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);

  // Re-derived from Rust rather than reimplemented in TypeScript, so the preview
  // cannot drift from the filenames actually produced.
  useEffect(() => {
    let cancelled = false;
    void invoke<string>("filename_preview", {
      naming: draft.naming,
      extension: draft.format === "png" ? "png" : "jpg",
    })
      .then((name) => {
        if (!cancelled) setPreview(name);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [draft.naming, draft.format]);

  const patch = useCallback((changes: Partial<Settings>) => {
    setDraft((current) => ({ ...current, ...changes }));
    setStatus(null);
  }, []);

  const browse = useCallback(async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: draft.saveDirectory,
        title: "Choose where Snipd saves captures",
      });
      if (typeof picked === "string") patch({ saveDirectory: picked });
    } catch (err) {
      setStatus(String(err));
    }
  }, [draft.saveDirectory, patch]);

  const save = useCallback(async () => {
    setSaving(true);
    try {
      const result = await invoke<string[]>("update_settings", { settings: draft });
      setWarnings(result);
      setStatus(result.length > 0 ? "Saved, but some shortcuts could not be bound." : "Saved.");
      onSaved(draft);
    } catch (err) {
      setStatus(String(err));
    } finally {
      setSaving(false);
    }
  }, [draft, onSaved]);

  return (
    <section className="settings">
      <Group title="Appearance">
        <Row label="Theme">
          <select
            value={draft.theme}
            onChange={(e) => patch({ theme: e.target.value as Settings["theme"] })}
          >
            <option value="system">Match Windows</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </Row>
      </Group>

      <Group title="Saving">
        <Row label="Folder">
          <div className="settings__inline">
            <input
              type="text"
              value={draft.saveDirectory}
              onChange={(e) => patch({ saveDirectory: e.target.value })}
            />
            <button type="button" onClick={() => void browse()}>
              Browse
            </button>
          </div>
        </Row>

        <Row label="Format">
          <select
            value={draft.format}
            onChange={(e) => patch({ format: e.target.value as Settings["format"] })}
          >
            <option value="png">PNG — lossless, larger</option>
            <option value="jpeg">JPEG — smaller, lossy</option>
          </select>
        </Row>

        {draft.format === "jpeg" && (
          <Row label="JPEG quality">
            <div className="settings__inline">
              <input
                type="range"
                min={40}
                max={100}
                value={draft.jpegQuality}
                onChange={(e) => patch({ jpegQuality: Number(e.target.value) })}
              />
              <span className="settings__value">{draft.jpegQuality}</span>
            </div>
          </Row>
        )}

        <Row label="Naming">
          <select
            value={draft.naming.mode}
            onChange={(e) =>
              patch({
                naming: { ...draft.naming, mode: e.target.value as "datetime" | "prefix" },
              })
            }
          >
            <option value="datetime">Date and time</option>
            <option value="prefix">Custom prefix with numbering</option>
          </select>
        </Row>

        {draft.naming.mode === "prefix" && (
          <>
            <Row label="Prefix">
              <input
                type="text"
                value={draft.naming.prefix}
                onChange={(e) => patch({ naming: { ...draft.naming, prefix: e.target.value } })}
              />
            </Row>
            <Row label="Next number">
              <input
                type="number"
                min={1}
                value={draft.naming.counter}
                onChange={(e) =>
                  patch({
                    naming: { ...draft.naming, counter: Math.max(1, Number(e.target.value)) },
                  })
                }
              />
            </Row>
          </>
        )}

        <Row label="Preview">
          <code className="settings__preview">{preview || "…"}</code>
        </Row>
      </Group>

      <Group title="Behaviour">
        <Toggle
          label="Copy every capture to the clipboard"
          checked={draft.clipboard.autoCopy}
          onChange={(v) => patch({ clipboard: { autoCopy: v } })}
        />
        <Toggle
          label="Show a notification after saving"
          checked={draft.notifications.showSavedToast}
          onChange={(v) => patch({ notifications: { showSavedToast: v } })}
        />
        <Toggle
          label="Closing the window keeps Snipd in the tray"
          checked={draft.window.closeToTray}
          onChange={(v) => patch({ window: { closeToTray: v } })}
        />
        <Toggle
          label="Start Snipd when Windows starts"
          checked={draft.startup.launchOnLogin}
          onChange={(v) => patch({ startup: { ...draft.startup, launchOnLogin: v } })}
        />
        <Toggle
          label="When started by Windows, start minimised to the tray"
          checked={draft.startup.startMinimised}
          onChange={(v) => patch({ startup: { ...draft.startup, startMinimised: v } })}
        />
      </Group>

      <Group title="Shortcuts">
        <p className="settings__note">
          Use names like <code>Ctrl</code>, <code>Alt</code>, <code>Shift</code> joined with{" "}
          <code>+</code>. Leave a box empty to unbind it.
        </p>
        {SHORTCUT_LABELS.map(({ key, label, hint }) => (
          <Row key={key} label={label} hint={hint}>
            <input
              type="text"
              value={draft.shortcuts[key]}
              onChange={(e) =>
                patch({ shortcuts: { ...draft.shortcuts, [key]: e.target.value } })
              }
            />
          </Row>
        ))}
      </Group>

      <Group title="History">
        <Toggle
          label="Automatically delete old captures"
          checked={draft.retention.enabled}
          onChange={(v) => patch({ retention: { ...draft.retention, enabled: v } })}
        />
        {draft.retention.enabled && (
          <Row label="Delete after">
            <div className="settings__inline">
              <input
                type="number"
                min={1}
                value={draft.retention.days}
                onChange={(e) =>
                  patch({
                    retention: { ...draft.retention, days: Math.max(1, Number(e.target.value)) },
                  })
                }
              />
              <span className="settings__value">days</span>
            </div>
          </Row>
        )}
        {draft.retention.enabled && (
          <p className="settings__warn">
            Captures older than this will be deleted permanently. Off by default, and nothing is
            ever removed unless you turn this on.
          </p>
        )}
      </Group>

      <Group title="About">
        <Row label="Version">
          <span>Snipd {APP_VERSION}</span>
        </Row>
        <Row label="Source">
          <a href={GITHUB_URL} target="_blank" rel="noreferrer">
            {GITHUB_URL}
          </a>
        </Row>
      </Group>

      {warnings.length > 0 && (
        <div className="banner banner--warn">
          {warnings.map((w) => (
            <p key={w}>{w}</p>
          ))}
        </div>
      )}

      <div className="settings__footer">
        <button type="button" className="primary" onClick={() => void save()} disabled={saving}>
          {saving ? "Saving…" : "Save settings"}
        </button>
        {status && <span className="settings__status">{status}</span>}
      </div>
    </section>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="settings__group">
      <h2>{title}</h2>
      {children}
    </div>
  );
}

function Row({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="settings__row">
      <div className="settings__label">
        <span>{label}</span>
        {hint && <small>{hint}</small>}
      </div>
      <div className="settings__control">{children}</div>
    </div>
  );
}

function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className="settings__toggle">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>{label}</span>
    </label>
  );
}
