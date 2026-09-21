/**
 * Rasterise assets/icon.svg into everything Windows and Tauri need.
 *
 * Run with: npm run icon
 *
 * The SVG is the source of truth — edit that, not the generated files. Keeping
 * generation in a script rather than committing hand-exported images means the
 * icon can never quietly drift from its source, and anyone forking the project
 * can restyle it without design tooling.
 */

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";
import pngToIco from "png-to-ico";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(join(root, "assets", "icon.svg"));
const iconDir = join(root, "src-tauri", "icons");

mkdirSync(iconDir, { recursive: true });

/** Render the SVG at a given pixel width. */
function render(width) {
  const resvg = new Resvg(source, { fitTo: { mode: "width", value: width } });
  return resvg.render().asPng();
}

/**
 * Sizes Tauri's bundler expects, plus the Windows Store logo sizes it copies
 * verbatim. Missing any of these makes `tauri build` fail at bundle time.
 */
const PNG_TARGETS = [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512],
  ["Square30x30Logo.png", 30],
  ["Square44x44Logo.png", 44],
  ["Square71x71Logo.png", 71],
  ["Square89x89Logo.png", 89],
  ["Square107x107Logo.png", 107],
  ["Square142x142Logo.png", 142],
  ["Square150x150Logo.png", 150],
  ["Square284x284Logo.png", 284],
  ["Square310x310Logo.png", 310],
  ["StoreLogo.png", 50],
];

for (const [name, size] of PNG_TARGETS) {
  writeFileSync(join(iconDir, name), render(size));
}

/**
 * The .ico carries several sizes so Windows can pick per context — 16px in the
 * tray and title bar, 256px in file properties. Rendering each from the vector
 * rather than downscaling one bitmap keeps the small sizes crisp, which is the
 * whole reason the icon was designed to shed detail gracefully.
 */
const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];
const ico = await pngToIco(ICO_SIZES.map(render));
writeFileSync(join(iconDir, "icon.ico"), ico);

console.log(
  `Wrote ${PNG_TARGETS.length} PNGs and a ${ICO_SIZES.length}-size .ico to src-tauri/icons`,
);
