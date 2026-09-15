// Build-time helper: embed real TUI evidence frames into the landing page.
//
// Reads the exact bytes committed under .loop/evidence/ (the same files the e2e
// slices assert), HTML-escapes them, and returns { name, source, content }.
// Fails the build loudly if a source frame is missing — a landing whose captures
// cannot be traced to evidence is a landing that does not ship.
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

// Repo root = first ancestor of the build cwd holding Cargo.toml.
// (import.meta.url is unreliable: Astro bundles this module into dist/.)
function findRepoRoot() {
  let dir = process.cwd();
  for (let i = 0; i < 6; i++) {
    if (existsSync(join(dir, 'Cargo.toml'))) return dir;
    const parent = join(dir, '..');
    if (parent === dir) break;
    dir = parent;
  }
  throw new Error('frames: repo root (Cargo.toml) not found above ' + process.cwd());
}

const REPO = findRepoRoot();

export function loadFrame(relativePath) {
  const abs = join(REPO, relativePath);
  if (!existsSync(abs)) {
    throw new Error(`frames: missing evidence source ${relativePath} (build refused)`);
  }
  const raw = readFileSync(abs, 'utf8');
  return {
    name: relativePath.split('/').pop(),
    source: relativePath,
    content: raw
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;'),
  };
}
