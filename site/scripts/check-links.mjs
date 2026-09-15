// Post-build gate: every #anchor link resolves, and no active install.sh URL ships.
// Run: npm run check-links (after npm run build).
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const html = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), '..', 'dist', 'index.html'),
  'utf8',
);

let fail = 0;

// 1. Internal anchors resolve.
const ids = new Set([...html.matchAll(/ id="([^"]+)"/g)].map((m) => m[1]));
for (const m of html.matchAll(/ href="#([^"]+)"/g)) {
  if (!ids.has(m[1])) {
    console.error(`check-links: dead anchor #${m[1]}`);
    fail = 1;
  }
}

// 2. No live one-liner installer before a signed release exists (T-0042).
// The line may only appear inside an HTML comment.
const withoutComments = html.replaceAll(/<!--[\s\S]*?-->/g, '');
if (withoutComments.includes('arreo.dev/install')) {
  console.error('check-links: active arreo.dev/install URL — remove until a signed release ships');
  fail = 1;
}

// 3. No premature claims.
for (const claim of ['v1.0.0', 'externally audited', 'security audited']) {
  if (withoutComments.includes(claim)) {
    console.error(`check-links: premature claim "${claim}"`);
    fail = 1;
  }
}

console.log(
  fail === 0
    ? `check-links: PASS (${ids.size} anchors, no installer URL, no premature claims)`
    : 'check-links: FAIL',
);
process.exit(fail);
