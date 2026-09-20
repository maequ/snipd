import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import { resolve } from "node:path";
// @ts-expect-error type error without @types/node package
import { fileURLToPath } from "node:url";

const host = process.env.TAURI_DEV_HOST;
const root = fileURLToPath(new URL(".", import.meta.url));

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react()],

  build: {
    // Tauri serves the app over a custom protocol, where Vite's `modulepreload`
    // links and their `crossorigin` attribute trigger CORS-checked fetches that
    // can be rejected outright -- the script then never executes and the window
    // renders blank, with no error to catch. The preloads are only a latency
    // optimisation for a page loading from a real network, which this is not.
    modulePreload: false,
    rollupOptions: {
      // The region-selection overlay is a separate top-level window with its own
      // document, so it needs its own entry point rather than being a route
      // inside the main app.
      input: {
        main: resolve(root, "index.html"),
        overlay: resolve(root, "overlay.html"),
        pin: resolve(root, "pin.html"),
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
