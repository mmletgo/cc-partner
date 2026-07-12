/**
 * check-p2p-route-inventory.mjs — verify docs/p2p-protocol.md stays in sync
 * with the literal /api/ routes registered in src-tauri/src/net/http_server.rs.
 *
 * Business Logic (why this script exists):
 *   docs/p2p-protocol.md is the authoritative retry/idempotency inventory for
 *   every P2P HTTP route. Forgetting to add a new route leaves it without a
 *   documented retry class, and forgetting to remove a deleted route leaves a
 *   stale contract. This script diffs the router against the table and fails
 *   the change (exit 1) on any mismatch.
 *
 * Code Logic (what this script does):
 *   - Extracts literal `/api/...` path strings from http_server.rs (the first
 *     quoted argument to axum `.route(...)`). Dynamic segments like `:id`,
 *     `:previewId` and `*path` are preserved verbatim because they are
 *     documented identically in the table.
 *   - Extracts the path column from the markdown inventory table in
 *     docs/p2p-protocol.md (rows starting with `|` whose path cell starts with
 *     `/api/`).
 *   - Prints the set difference in both directions and exits 1 on mismatch,
 *     0 when the two sets are equal.
 *
 *   Only Node built-ins are used (no dependencies). The script is repo-root
 *   agnostic: it locates files relative to its own URL.
 *
 * Usage:
 *   node scripts/check-p2p-route-inventory.mjs
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, '..');

const httpServerPath = resolve(
  repoRoot,
  'src-tauri/src/net/http_server.rs'
);
const inventoryPath = resolve(repoRoot, 'docs/p2p-protocol.md');

/**
 * Pull every literal `/api/...` route string passed to `.route(...)` in the
 * axum router. The first quoted token after `.route(` is the path; we keep
 * dynamic segments (`:id`, `*path`) exactly as written so they can be mirrored
 * in the markdown table.
 *
 * @param {string} source
 * @returns {Set<string>}
 */
function extractRouterRoutes(source) {
  const routes = new Set();
  const routeCall = /\.route\s*\(\s*"([^"]+)"/g;
  let match;
  while ((match = routeCall.exec(source)) !== null) {
    const path = match[1];
    if (path.startsWith('/api/')) {
      routes.add(path);
    }
  }
  return routes;
}

/**
 * Pull the path column out of the markdown inventory table. Only cells whose
 * value (after stripping surrounding markdown backticks/quotes) starts with
 * `/api/` are counted, so the helper is robust to column reordering and to the
 * other tables in the document (retry classes, capability advertisement).
 *
 * @param {string} markdown
 * @returns {Set<string>}
 */
function extractInventoryRoutes(markdown) {
  const routes = new Set();
  for (const rawLine of markdown.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line.startsWith('|')) continue;
    const cells = line.split('|').map((cell) => cell.trim());
    for (const rawCell of cells) {
      // Strip surrounding markdown inline-code backticks and matching quotes
      // so `/api/health` and `/api/health` both parse to /api/health.
      let cell = rawCell;
      if (
        cell.length >= 2 &&
        cell.startsWith('`') &&
        cell.endsWith('`')
      ) {
        cell = cell.slice(1, -1);
      } else if (
        cell.length >= 2 &&
        ((cell.startsWith('"') && cell.endsWith('"')) ||
          (cell.startsWith("'") && cell.endsWith("'")))
      ) {
        cell = cell.slice(1, -1);
      }
      if (cell.startsWith('/api/')) {
        routes.add(cell);
        break;
      }
    }
  }
  return routes;
}

function main() {
  const httpServer = readFileSync(httpServerPath, 'utf8');
  const inventory = readFileSync(inventoryPath, 'utf8');

  const routerRoutes = extractRouterRoutes(httpServer);
  const inventoryRoutes = extractInventoryRoutes(inventory);

  const missingFromInventory = [...routerRoutes].filter(
    (path) => !inventoryRoutes.has(path)
  );
  const extraInInventory = [...inventoryRoutes].filter(
    (path) => !routerRoutes.has(path)
  );

  const routerCount = routerRoutes.size;
  const inventoryCount = inventoryRoutes.size;

  process.stdout.write(
    `router routes: ${routerCount}; inventory rows: ${inventoryCount}\n`
  );

  let ok = true;
  if (missingFromInventory.length > 0) {
    ok = false;
    process.stdout.write(
      `MISSING from docs/p2p-protocol.md (${missingFromInventory.length}):\n`
    );
    for (const path of missingFromInventory.sort()) {
      process.stdout.write(`  + ${path}\n`);
    }
  }
  if (extraInInventory.length > 0) {
    ok = false;
    process.stdout.write(
      `EXTRA in docs/p2p-protocol.md (no matching router entry, ${extraInInventory.length}):\n`
    );
    for (const path of extraInInventory.sort()) {
      process.stdout.write(`  - ${path}\n`);
    }
  }

  if (ok) {
    process.stdout.write(
      'P2P route inventory matches http_server.rs exactly.\n'
    );
    process.exit(0);
  }
  process.stdout.write(
    'P2P route inventory is OUT OF SYNC with http_server.rs. ' +
      'Add/remove the route row in docs/p2p-protocol.md and re-run.\n'
  );
  process.exit(1);
}

main();
