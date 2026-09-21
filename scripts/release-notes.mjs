/**
 * Pull one version's section out of CHANGELOG.md, for release notes.
 *
 * Run as: node scripts/release-notes.mjs v0.3.0
 *
 * The release workflow uses this so that what is published on a release is the
 * same text kept in the changelog, written deliberately, rather than a list of
 * commit subjects. It *fails* when a version has no section, which is the point:
 * a release going out with no description of what changed should break the
 * build, not quietly ship boilerplate.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const requested = process.argv[2];
if (!requested) {
  console.error("usage: node scripts/release-notes.mjs <tag>");
  process.exit(1);
}

// Tags are written `v0.3.0`; the changelog heading is `## [0.3.0] - date`.
const version = requested.replace(/^v/, "");
const changelog = readFileSync(join(root, "CHANGELOG.md"), "utf8");

const lines = changelog.split(/\r?\n/);
const start = lines.findIndex((line) => line.startsWith(`## [${version}]`));

if (start === -1) {
  console.error(
    `CHANGELOG.md has no section for ${version}.\n` +
      `Add a "## [${version}] - YYYY-MM-DD" heading describing what changed, ` +
      `then tag again.`,
  );
  process.exit(1);
}

// Everything up to the next version heading.
let end = lines.length;
for (let i = start + 1; i < lines.length; i += 1) {
  if (lines[i].startsWith("## ")) {
    end = i;
    break;
  }
}

const body = lines
  .slice(start + 1, end)
  .join("\n")
  .trim();

if (!body) {
  console.error(`The ${version} section in CHANGELOG.md is empty.`);
  process.exit(1);
}

const installing = `
## Installing

Download **\`Snipd-Setup-${version}.exe\`** below and run it. Installing over an
earlier version keeps your settings and captures.

\`snipd.exe\` is the same application without the installer, for anyone who would
rather not run one.

The installer is not code-signed, so Windows SmartScreen will warn on first run.
Choose **More info → Run anyway**.
`.trim();

process.stdout.write(`${body}\n\n${installing}\n`);
