/**
 * Annotation editor.
 *
 * Opens when a capture is clicked in the library. Annotations are kept as a list
 * of shapes in *image* coordinates and the canvas is redrawn from scratch on
 * every change, which is what makes undo and redo trivially correct: there is no
 * accumulated pixel state to unwind, only a shorter list to redraw.
 *
 * Keeping shapes in image coordinates rather than screen coordinates means the
 * canvas can be displayed at any size without the export ever being affected.
 *
 * Saving writes a *copy*. The original capture was auto-saved the instant it was
 * taken and is the thing this app promises never to lose, so an edit is not
 * allowed to overwrite it.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import "./editor.css";

export interface EditorProps {
  /** Image to edit, served by the app's image protocol. */
  imageUrl: string;
  fileName: string;
  /** Absolute path, needed to write an edit back over the original. */
  path: string;
  /**
   * True when this is a capture the user has just taken.
   *
   * Reviewing writes back over the capture and can rename it, because it is
   * seconds old and the marked-up version simply is what they wanted. Editing
   * something from the library later writes a copy instead, so a file that may
   * already have been shared is never rewritten underneath them.
   */
  reviewing: boolean;
  /**
   * Return to the library.
   *
   * Given the saved path when a review was kept, so the library can confirm it
   * by name instead of the editor closing with nothing said.
   */
  onClose: (savedPath?: string) => void;
}

interface Pt {
  x: number;
  y: number;
}

type Tool = "pen" | "arrow" | "rect" | "ellipse" | "text" | "redact" | "crop";

type Shape =
  | { t: "pen"; pts: Pt[]; color: string; w: number }
  | { t: "arrow"; a: Pt; b: Pt; color: string; w: number }
  | { t: "rect"; a: Pt; b: Pt; color: string; w: number; fill: boolean }
  | { t: "ellipse"; a: Pt; b: Pt; color: string; w: number; fill: boolean }
  | { t: "text"; p: Pt; text: string; color: string; size: number }
  | { t: "redact"; a: Pt; b: Pt };

const COLORS = ["#e5484d", "#f5a524", "#2f9e44", "#2f5fd8", "#111318", "#ffffff"];
const WIDTHS = [2, 4, 7, 12];

const TOOLS: Array<{ id: Tool; label: string }> = [
  { id: "pen", label: "Pen" },
  { id: "arrow", label: "Arrow" },
  { id: "rect", label: "Rectangle" },
  { id: "ellipse", label: "Ellipse" },
  { id: "text", label: "Text" },
  { id: "redact", label: "Redact" },
  { id: "crop", label: "Crop" },
];

function norm(a: Pt, b: Pt) {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.abs(b.x - a.x),
    h: Math.abs(b.y - a.y),
  };
}

/**
 * Pixelate a region, for hiding passwords, emails and the like.
 *
 * Pixelation rather than a blur: a Gaussian blur of small text can sometimes be
 * partially recovered, and more importantly it *looks* like something that might
 * be recoverable. Averaging whole blocks is visibly destructive, which is the
 * right signal for a redaction tool.
 */
function pixelate(ctx: CanvasRenderingContext2D, a: Pt, b: Pt): void {
  const { x, y, w, h } = norm(a, b);
  if (w < 2 || h < 2) return;

  const block = Math.max(6, Math.round(Math.min(w, h) / 10));
  const tw = Math.max(1, Math.round(w / block));
  const th = Math.max(1, Math.round(h / block));

  const scratch = document.createElement("canvas");
  scratch.width = tw;
  scratch.height = th;
  const sctx = scratch.getContext("2d");
  if (!sctx) return;

  // Down to a handful of pixels, then back up with smoothing off so the blocks
  // stay hard-edged instead of being interpolated back into readable shapes.
  sctx.drawImage(ctx.canvas, x, y, w, h, 0, 0, tw, th);
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(scratch, 0, 0, tw, th, x, y, w, h);
  ctx.imageSmoothingEnabled = true;
}

function drawArrow(ctx: CanvasRenderingContext2D, a: Pt, b: Pt, color: string, w: number): void {
  const angle = Math.atan2(b.y - a.y, b.x - a.x);
  const head = Math.max(10, w * 3.5);

  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = w;
  ctx.lineCap = "round";

  // Stop the shaft short of the tip so it does not poke through the head.
  ctx.beginPath();
  ctx.moveTo(a.x, a.y);
  ctx.lineTo(b.x - Math.cos(angle) * head * 0.8, b.y - Math.sin(angle) * head * 0.8);
  ctx.stroke();

  ctx.beginPath();
  ctx.moveTo(b.x, b.y);
  ctx.lineTo(b.x - head * Math.cos(angle - 0.4), b.y - head * Math.sin(angle - 0.4));
  ctx.lineTo(b.x - head * Math.cos(angle + 0.4), b.y - head * Math.sin(angle + 0.4));
  ctx.closePath();
  ctx.fill();
}

function drawShape(ctx: CanvasRenderingContext2D, s: Shape): void {
  if (s.t === "redact") return; // handled in its own pass

  ctx.save();
  ctx.lineCap = "round";
  ctx.lineJoin = "round";

  if (s.t === "pen") {
    ctx.strokeStyle = s.color;
    ctx.lineWidth = s.w;
    ctx.beginPath();
    s.pts.forEach((p, i) => (i === 0 ? ctx.moveTo(p.x, p.y) : ctx.lineTo(p.x, p.y)));
    ctx.stroke();
  } else if (s.t === "arrow") {
    drawArrow(ctx, s.a, s.b, s.color, s.w);
  } else if (s.t === "rect") {
    const { x, y, w, h } = norm(s.a, s.b);
    ctx.strokeStyle = s.color;
    ctx.fillStyle = s.color;
    ctx.lineWidth = s.w;
    if (s.fill) ctx.fillRect(x, y, w, h);
    else ctx.strokeRect(x, y, w, h);
  } else if (s.t === "ellipse") {
    const { x, y, w, h } = norm(s.a, s.b);
    ctx.strokeStyle = s.color;
    ctx.fillStyle = s.color;
    ctx.lineWidth = s.w;
    ctx.beginPath();
    ctx.ellipse(x + w / 2, y + h / 2, w / 2, h / 2, 0, 0, Math.PI * 2);
    if (s.fill) ctx.fill();
    else ctx.stroke();
  } else if (s.t === "text") {
    ctx.fillStyle = s.color;
    ctx.font = `600 ${s.size}px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif`;
    ctx.textBaseline = "top";
    // A dark halo so light text stays legible over a light screenshot.
    ctx.strokeStyle = "rgba(0,0,0,0.55)";
    ctx.lineWidth = Math.max(2, s.size / 10);
    ctx.strokeText(s.text, s.p.x, s.p.y);
    ctx.fillText(s.text, s.p.x, s.p.y);
  }

  ctx.restore();
}

export default function Editor({ imageUrl, fileName, path, reviewing, onClose }: EditorProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);

  const [shapes, setShapes] = useState<Shape[]>([]);
  const [redo, setRedo] = useState<Shape[]>([]);
  const [tool, setTool] = useState<Tool>("pen");
  const [color, setColor] = useState(COLORS[0]);
  const [width, setWidth] = useState(WIDTHS[1]);
  const [filled, setFilled] = useState(false);
  const [crop, setCrop] = useState<{ a: Pt; b: Pt } | null>(null);
  const [draft, setDraft] = useState<Shape | null>(null);
  const [typing, setTyping] = useState<{ p: Pt; value: string } | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * Bumped when a new image finishes decoding.
   *
   * The image lives in a ref (it is not render state), so this is what tells the
   * paint effect to run again. It also keeps `load` free of any dependency on
   * `render` — which previously formed a cycle, since `render` depends on
   * `shapes` and `load` resets them, so the effect re-fired forever and the
   * window never painted at all.
   */
  const [imageVersion, setImageVersion] = useState(0);
  /** Filename without its extension, editable during review. */
  const [name, setName] = useState(() => fileName.replace(/\.[^.]+$/, ""));

  const dragging = useRef(false);
  const startPt = useRef<Pt>({ x: 0, y: 0 });
  const saveRef = useRef<(() => Promise<void>) | null>(null);
  const copyRef = useRef<(() => Promise<void>) | null>(null);

  /** Repaint everything from the shape list. */
  const render = useCallback(() => {
    const canvas = canvasRef.current;
    const img = imageRef.current;
    if (!canvas || !img) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(img, 0, 0);

    const all = draft ? [...shapes, draft] : shapes;

    // Redactions run first so later annotations are never pixelated along with
    // the content underneath them.
    for (const s of all) if (s.t === "redact") pixelate(ctx, s.a, s.b);
    for (const s of all) drawShape(ctx, s);

    if (crop) {
      const { x, y, w, h } = norm(crop.a, crop.b);
      ctx.save();
      ctx.fillStyle = "rgba(10,12,16,0.55)";
      // Four bands around the keep-area, rather than a clip path.
      ctx.fillRect(0, 0, canvas.width, y);
      ctx.fillRect(0, y + h, canvas.width, canvas.height - y - h);
      ctx.fillRect(0, y, x, h);
      ctx.fillRect(x + w, y, canvas.width - x - w, h);
      ctx.strokeStyle = "#fff";
      ctx.lineWidth = 2;
      ctx.strokeRect(x, y, w, h);
      ctx.restore();
    }
  }, [shapes, draft, crop, imageVersion]);

  useEffect(() => {
    render();
  }, [render]);

  // Load whenever a different capture is opened.
  useEffect(() => {
    setShapes([]);
    setRedo([]);
    setCrop(null);
    setStatus(null);

    const img = new Image();
    // Requested in CORS mode, and the protocol answers with
    // Access-Control-Allow-Origin. Both halves are required: without them the
    // image is cross-origin, the canvas it is drawn onto becomes tainted, and
    // every read of that canvas throws a SecurityError — which is what made
    // saving an annotated capture fail without any visible error.
    img.crossOrigin = "anonymous";
    img.onload = () => {
      const canvas = canvasRef.current;
      if (canvas) {
        canvas.width = img.naturalWidth;
        canvas.height = img.naturalHeight;
      }
      imageRef.current = img;
      // Triggers the paint effect. Calling `render` directly here would make
      // this effect depend on it, which is the cycle described above.
      setImageVersion((v) => v + 1);
    };
    img.onerror = () => setStatus("That capture could not be loaded.");
    img.src = imageUrl;
  }, [imageUrl]);

  // A different capture opened in the same editor brings its own name.
  useEffect(() => {
    setName(fileName.replace(/\.[^.]+$/, ""));
  }, [fileName]);

  /** Screen point to image point. The canvas is displayed scaled to fit. */
  const toImage = useCallback((e: React.MouseEvent): Pt => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    return {
      x: ((e.clientX - rect.left) / rect.width) * canvas.width,
      y: ((e.clientY - rect.top) / rect.height) * canvas.height,
    };
  }, []);

  const commit = useCallback((s: Shape) => {
    setShapes((current) => [...current, s]);
    // Any new edit invalidates the redo branch, as in every editor.
    setRedo([]);
  }, []);

  const onDown = (e: React.MouseEvent) => {
    if (!imageRef.current || typing) return;
    const p = toImage(e);
    startPt.current = p;
    dragging.current = true;

    if (tool === "text") {
      dragging.current = false;
      setTyping({ p, value: "" });
      return;
    }
    if (tool === "pen") setDraft({ t: "pen", pts: [p], color, w: width });
  };

  const onMove = (e: React.MouseEvent) => {
    if (!dragging.current) return;
    const p = toImage(e);
    const a = startPt.current;

    if (tool === "pen") {
      setDraft((d) => (d && d.t === "pen" ? { ...d, pts: [...d.pts, p] } : d));
    } else if (tool === "crop") {
      setCrop({ a, b: p });
    } else if (tool === "arrow") {
      setDraft({ t: "arrow", a, b: p, color, w: width });
    } else if (tool === "rect") {
      setDraft({ t: "rect", a, b: p, color, w: width, fill: filled });
    } else if (tool === "ellipse") {
      setDraft({ t: "ellipse", a, b: p, color, w: width, fill: filled });
    } else if (tool === "redact") {
      setDraft({ t: "redact", a, b: p });
    }
  };

  const onUp = () => {
    if (!dragging.current) return;
    dragging.current = false;
    if (draft) {
      // A click with no drag is not a shape. Text never arrives as a draft, but
      // the type does not know that, so it is handled explicitly.
      let worthKeeping: boolean;
      if (draft.t === "pen") worthKeeping = draft.pts.length > 1;
      else if (draft.t === "text") worthKeeping = draft.text.trim().length > 0;
      else worthKeeping = norm(draft.a, draft.b).w > 3;

      if (worthKeeping) commit(draft);
      setDraft(null);
    }
  };

  const commitText = () => {
    if (typing && typing.value.trim()) {
      commit({
        t: "text",
        p: typing.p,
        text: typing.value,
        color,
        size: Math.max(16, width * 6),
      });
    }
    setTyping(null);
  };

  const undo = useCallback(() => {
    setShapes((current) => {
      if (current.length === 0) return current;
      const last = current[current.length - 1];
      setRedo((r) => [...r, last]);
      return current.slice(0, -1);
    });
  }, []);

  const redoLast = useCallback(() => {
    setRedo((stack) => {
      if (stack.length === 0) return stack;
      const next = stack[stack.length - 1];
      setShapes((current) => [...current, next]);
      return stack.slice(0, -1);
    });
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typing) return;
      if (e.ctrlKey && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) redoLast();
        else undo();
      }
      if (e.ctrlKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void saveRef.current?.();
      }
      if (e.ctrlKey && e.key.toLowerCase() === "c") {
        e.preventDefault();
        void copyRef.current?.();
      }
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [undo, redoLast, typing, onClose]);

  /**
   * Flatten to a PNG, honouring the crop rectangle if one is set.
   *
   * Throws rather than returning null for anything that is a genuine failure.
   * Returning a bare null made every cause — no image, a zero-sized crop, a
   * tainted canvas — look identical to the caller, which reported all of them
   * as "Nothing to save." while the real problem went unnamed.
   */
  const exportPng = useCallback(async (): Promise<string> => {
    const canvas = canvasRef.current;
    if (!canvas) throw new Error("The editor canvas is not ready yet.");

    // Re-render without the crop chrome, so the dimming never ends up baked in.
    const out = document.createElement("canvas");
    const region = crop
      ? norm(crop.a, crop.b)
      : { x: 0, y: 0, w: canvas.width, h: canvas.height };
    if (region.w < 1 || region.h < 1) {
      throw new Error("That crop is too small to save.");
    }

    out.width = Math.round(region.w);
    out.height = Math.round(region.h);
    const octx = out.getContext("2d");
    const img = imageRef.current;
    if (!octx) throw new Error("This system could not provide a drawing canvas.");
    if (!img) throw new Error("The capture has not finished loading.");

    octx.translate(-region.x, -region.y);
    octx.drawImage(img, 0, 0);
    for (const s of shapes) if (s.t === "redact") pixelate(octx, s.a, s.b);
    for (const s of shapes) drawShape(octx, s);

    let blob: Blob | null;
    try {
      blob = await new Promise<Blob | null>((resolve, reject) => {
        try {
          out.toBlob(resolve, "image/png");
        } catch (err) {
          reject(err);
        }
      });
    } catch (err) {
      // Reading a canvas that has a cross-origin image drawn on it throws a
      // SecurityError. That should no longer be reachable, but if it ever is,
      // the message must say what actually went wrong rather than leaving
      // someone staring at raw DOM wording.
      const detail = err instanceof Error ? err.message : String(err);
      throw new Error(
        `Could not read the edited image back from the canvas (${detail}). ` +
          "This is a bug in Snipd, not something you did.",
      );
    }
    if (!blob) throw new Error("The edited image could not be encoded as a PNG.");

    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = "";
    // Chunked: spreading a multi-megabyte array into one call blows the stack.
    for (let i = 0; i < bytes.length; i += 0x8000) {
      binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    }
    return btoa(binary);
  }, [crop, shapes]);

  const copyToClipboard = async () => {
    setBusy(true);
    try {
      const png = await exportPng();
      if (!png) {
        setStatus("Nothing to copy.");
        return;
      }
      await invoke("copy_edited", { png });
      setStatus("Copied to the clipboard.");
    } catch (err) {
      setStatus(String(err));
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    setBusy(true);
    setStatus(null);
    try {
      const png = await exportPng();

      if (reviewing) {
        const saved = await invoke<string>("apply_edit", {
          path,
          png,
          newName: name.trim() || null,
        });
        // Closing is the confirmation: reviewing is a step in taking a capture,
        // and the library it returns to shows the capture saved under its final
        // name. Handing back the path means the caller can say so precisely
        // rather than the editor simply vanishing.
        onClose(saved);
        return;
      }

      const record = await invoke<{ fileName: string }>("save_edited", { png });
      setStatus(`Saved a copy as ${record.fileName}`);
    } catch (err) {
      // Never swallowed. Saving failing without a word is what made an
      // annotated capture look like it had been kept when it had not.
      setStatus(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  saveRef.current = save;
  copyRef.current = copyToClipboard;

  const cursor = tool === "text" ? "text" : "crosshair";

  return (
    <div className="editor">
      <div className="bar">
        <div className="bar__group">
          {TOOLS.map((t) => (
            <button
              key={t.id}
              type="button"
              aria-pressed={tool === t.id}
              onClick={() => setTool(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        <div className="bar__group">
          {COLORS.map((c) => (
            <button
              key={c}
              type="button"
              className="swatch"
              aria-pressed={color === c}
              aria-label={`Colour ${c}`}
              style={{ background: c }}
              onClick={() => setColor(c)}
            />
          ))}
        </div>

        <div className="bar__group">
          {WIDTHS.map((w) => (
            <button key={w} type="button" aria-pressed={width === w} onClick={() => setWidth(w)}>
              {w}px
            </button>
          ))}
          <button type="button" aria-pressed={filled} onClick={() => setFilled((f) => !f)}>
            Filled
          </button>
        </div>

        <div className="bar__group">
          <button type="button" onClick={undo} disabled={shapes.length === 0}>
            Undo
          </button>
          <button type="button" onClick={redoLast} disabled={redo.length === 0}>
            Redo
          </button>
          {crop && (
            <button type="button" onClick={() => setCrop(null)}>
              Clear crop
            </button>
          )}
        </div>

        <div className="bar__group bar__group--end">
          {reviewing && (
            <label className="bar__name">
              <span className="sr-only">File name</span>
              <input
                type="text"
                value={name}
                spellCheck={false}
                placeholder="File name"
                onChange={(e) => setName(e.target.value)}
              />
            </label>
          )}
          <button type="button" onClick={() => onClose()}>
            {reviewing ? "Discard changes" : "Back to library"}
          </button>
          <button type="button" onClick={() => void copyToClipboard()} disabled={busy}>
            Copy
          </button>
          <button type="button" className="primary" onClick={() => void save()} disabled={busy}>
            {busy ? "Working…" : reviewing ? "Save" : "Save a copy"}
          </button>
        </div>
      </div>

      {status && <p className="status">{status}</p>}

      <div className="stage">
        <div className="stage__inner">
          <canvas
            ref={canvasRef}
            style={{ cursor }}
            onMouseDown={onDown}
            onMouseMove={onMove}
            onMouseUp={onUp}
            onMouseLeave={onUp}
          />
          {typing && (
            <input
              className="text-input"
              autoFocus
              // Percentages, so the box sits where it was clicked no matter
              // what size the canvas is being displayed at.
              style={{
                left: `${(typing.p.x / (canvasRef.current?.width || 1)) * 100}%`,
                top: `${(typing.p.y / (canvasRef.current?.height || 1)) * 100}%`,
              }}
              value={typing.value}
              placeholder="Type, then Enter"
              onChange={(e) => setTyping({ ...typing, value: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitText();
                if (e.key === "Escape") setTyping(null);
              }}
              onBlur={commitText}
            />
          )}
        </div>
      </div>

      <p className="filename">{reviewing ? path : fileName}</p>
    </div>
  );
}
