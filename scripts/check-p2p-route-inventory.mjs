/**
 * check-p2p-route-inventory.mjs — verify docs/p2p-protocol.md stays in sync
 * with the literal /api/ routes registered in src-tauri/src/net/http_server.rs,
 * including the HTTP method (Finding 4: previously only paths were checked,
 * which let a POST route be documented as GET, or an `any(...)` proxy be
 * documented as `read-only` GET, without the checker noticing).
 *
 * Business Logic (why this script exists):
 *   docs/p2p-protocol.md is the authoritative retry/idempotency inventory for
 *   every P2P HTTP route. Forgetting to add a new route leaves it without a
 *   documented retry class, and forgetting to remove a deleted route leaves a
 *   stale contract. A path-only check also misses method drift (e.g. a mutating
 *   POST mis-documented as a safe GET). This script diffs the router against
 *   the table on (method, path) pairs and fails the change (exit 1) on mismatch.
 *
 * Code Logic (what this script does):
 *   - Extracts (method, path) pairs from http_server.rs by scanning for
 *     `.route("...", <method>(...))` calls, supporting both single-line and
 *     multi-line forms. Methods are normalized to uppercase; `any(...)` is
 *     preserved as `ANY` because the browser proxy intentionally accepts all
 *     methods.
 *   - Extracts the (method, path) columns from the markdown inventory table.
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

const httpServerPath = resolve(repoRoot, 'src-tauri/src/net/http_server.rs');
const inventoryPath = resolve(repoRoot, 'docs/p2p-protocol.md');

/**
 * Pull every (method, path) pair passed to `.route(...)` in the axum router.
 *
 * The first quoted token after `.route(` is the path; the method handler is the
 * second argument, which is a call like `get(...)`, `post(...)`, `any(...)`,
 * `put(...)`, `delete(...)` or `patch(...)`. Both single-line and multi-line
 * forms are supported: the regex anchors on `.route(` + the path literal, then
 * captures the first method-handler identifier that follows on the same or a
 * subsequent line.
 *
 * @param {string} source
 * @returns {Set<string>} set of `"METHOD /api/path"` strings.
 */
function extractRouterRoutes(source) {
  const routes = new Set();
  // Match `.route("...", <optional whitespace/newlines> method(` — the method
  // handler call may sit on the next line for multi-line route definitions.
  const routeCall =
    /\.route\s*\(\s*"([^"]+)"\s*,\s*([a-z]+)\s*\(/gi;
  let match;
  while ((match = routeCall.exec(source)) !== null) {
    const path = match[1];
    const method = match[2].toUpperCase();
    if (path.startsWith('/api/')) {
      routes.add(`${method} ${path}`);
    }
  }
  return routes;
}

/**
 * Pull the (method, path) pair out of each row of the markdown inventory table.
 *
 * The table's first two cells are `Method` and `Path`. Only rows whose Path
 * cell (after stripping surrounding markdown backticks/quotes) starts with
 * `/api/` are counted, so the helper is robust to column reordering and to the
 * other tables in the document (retry classes, capability advertisement).
 *
 * @param {string} markdown
 * @returns {Set<string>} set of `"METHOD /api/path"` strings.
 */
function extractInventoryRoutes(markdown) {
  const routes = new Set();
  for (const rawLine of markdown.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line.startsWith('|')) continue;
    const cells = line.split('|').map((cell) => cell.trim());
    // Find the first cell that is an HTTP method, then the first subsequent
    // cell that starts with `/api/`. This tolerates a leading empty cell from
    // the leading `|`.
    let method = null;
    for (const rawCell of cells) {
      let cell = stripQuotes(rawCell);
      if (method === null) {
        const upper = cell.toUpperCase();
        if (
          upper === 'GET' ||
          upper === 'POST' ||
          upper === 'PUT' ||
          upper === 'DELETE' ||
          upper === 'PATCH' ||
          upper === 'ANY' ||
          upper === 'HEAD' ||
          upper === 'OPTIONS'
        ) {
          method = upper;
          continue;
        }
        // Not a method yet; keep scanning (skips the empty leading cell and
        // any non-method prose cells before the method column).
        continue;
      }
      if (cell.startsWith('/api/')) {
        routes.add(`${method} ${cell}`);
        break;
      }
    }
  }
  return routes;
}

/**
 * Strip surrounding markdown inline-code backticks or matching quotes so
 * `/api/health`, `\"/api/health\"` and `'/api/health'` all parse to /api/health.
 */
function stripQuotes(cell) {
  if (cell.length >= 2 && cell.startsWith('`') && cell.endsWith('`')) {
    return cell.slice(1, -1);
  }
  if (
    cell.length >= 2 &&
    ((cell.startsWith('"') && cell.endsWith('"')) ||
      (cell.startsWith("'") && cell.endsWith("'")))
  ) {
    return cell.slice(1, -1);
  }
  return cell;
}

function main() {
  const httpServer = readFileSync(httpServerPath, 'utf8');
  const inventory = readFileSync(inventoryPath, 'utf8');

  const routerRoutes = extractRouterRoutes(httpServer);
  const inventoryRoutes = extractInventoryRoutes(inventory);

  const missingFromInventory = [...routerRoutes].filter(
    (route) => !inventoryRoutes.has(route)
  );
  const extraInInventory = [...inventoryRoutes].filter(
    (route) => !routerRoutes.has(route)
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
    for (const route of missingFromInventory.sort()) {
      process.stdout.write(`  + ${route}\n`);
    }
  }
  if (extraInInventory.length > 0) {
    ok = false;
    process.stdout.write(
      `EXTRA in docs/p2p-protocol.md (no matching router entry, ${extraInInventory.length}):\n`
    );
    for (const route of extraInInventory.sort()) {
      process.stdout.write(`  - ${route}\n`);
    }
  }

  if (ok) {
    process.stdout.write(
      'P2P route inventory (method + path) matches http_server.rs exactly.\n'
    );
    process.exit(0);
  }
  process.stdout.write(
    'P2P route inventory is OUT OF SYNC with http_server.rs (method or path drift). ' +
      'Add/remove the route row in docs/p2p-protocol.md and re-run.\n'
  );
  process.exit(1);
}

main();
