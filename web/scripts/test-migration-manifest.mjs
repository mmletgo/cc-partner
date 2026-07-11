import { readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const WEB_ROOT = path.resolve(__dirname, '..');
const SRC_ROOT = path.join(WEB_ROOT, 'src');

/**
 * Migration inventory: every src test file (*.test.ts / *.test.tsx) must appear exactly once.
 * runner: 'legacy' - executed by run-legacy-tests.mjs (tsx)
 * runner: 'vitest' - collected by Vitest only
 */
export const TEST_MIGRATION_MANIFEST = [
  { path: 'src/api/mobile.test.ts', runner: 'legacy' },
  { path: 'src/api/orchestrator.test.ts', runner: 'legacy' },
  { path: 'src/api/promptOptimizer.test.ts', runner: 'legacy' },
  { path: 'src/api/workbenchHttp.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/MobileAccessCard/mobileAccessCard.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorTheme.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/WorkbenchHtmlPreview/htmlAssets.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/WorkbenchProjectRail/projectSessionStats.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/WorkbenchProjectRail/workbenchProjectRailStyles.test.ts', runner: 'legacy' },
  { path: 'src/components/domain/WorkbenchRemoteProjectPicker/workbenchRemoteProjectPickerLayout.test.ts', runner: 'legacy' },
  { path: 'src/hooks/workbenchHttpEvents.test.ts', runner: 'legacy' },
  { path: 'src/hooks/workbenchTerminalBuffer.test.ts', runner: 'legacy' },
  { path: 'src/lib/backendLifecycle.test.ts', runner: 'legacy' },
  { path: 'src/lib/claudeCodeAssets.test.ts', runner: 'legacy' },
  { path: 'src/lib/lanFirewallDependency.test.ts', runner: 'legacy' },
  { path: 'src/lib/orchestrator.test.ts', runner: 'legacy' },
  { path: 'src/lib/orchestratorRemote.test.ts', runner: 'legacy' },
  { path: 'src/lib/permissionEntries.test.ts', runner: 'legacy' },
  { path: 'src/lib/platform.test.ts', runner: 'vitest' },
  { path: 'src/lib/workbenchDependency.test.ts', runner: 'legacy' },
  { path: 'src/mobile/MobileAutomationPanel.test.ts', runner: 'legacy' },
  { path: 'src/mobile/MobileWorktreeQuickSwitch.test.ts', runner: 'legacy' },
  { path: 'src/mobile/mobileBrowserPanel.test.ts', runner: 'legacy' },
  { path: 'src/mobile/mobilePanelState.test.ts', runner: 'legacy' },
  { path: 'src/mobile/mobileTerminalReplay.test.ts', runner: 'legacy' },
  { path: 'src/mobile/mobileTerminalTouchScroll.test.ts', runner: 'legacy' },
  { path: 'src/mobile/mobileWorkbenchState.test.ts', runner: 'legacy' },
  { path: 'src/pages/Health/HabitStatsCard.test.ts', runner: 'legacy' },
  { path: 'src/pages/Orchestrator/orchestratorActions.test.ts', runner: 'legacy' },
  { path: 'src/pages/Orchestrator/orchestratorBoard.test.ts', runner: 'legacy' },
  { path: 'src/pages/Settings/HealthPanel.test.ts', runner: 'legacy' },
  { path: 'src/pages/Settings/automationSettingsState.test.ts', runner: 'legacy' },
  { path: 'src/pages/Settings/settingsState.test.ts', runner: 'legacy' },
  { path: 'src/pages/Settings/shortcutRecorder.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/promptOptimizerWidget.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/terminalOptions.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/terminalReplay.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/terminalSessionOrder.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/terminalSizing.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchAutomationView.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchBrowserPreview.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchDeepLink.test.ts', runner: 'vitest' },
  { path: 'src/pages/Workbench/workbenchFiles.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchLayerStyles.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchRemoteProjects.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchWorkspaceSwitch.test.ts', runner: 'legacy' },
  { path: 'src/pages/Workbench/workbenchWorktrees.test.ts', runner: 'legacy' },
];

/** Paths still on the legacy tsx runner; Vitest must exclude these. */
export const legacyTestPaths = TEST_MIGRATION_MANIFEST.filter(
  (entry) => entry.runner === 'legacy',
).map((entry) => entry.path);

/**
 * Recursively collect *.test.ts / *.test.tsx paths under src,
 * returned relative to web/ with POSIX separators, sorted.
 */
async function collectTestFiles(dir = SRC_ROOT, relBase = 'src') {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    const abs = path.join(dir, entry.name);
    const rel = `${relBase}/${entry.name}`;
    if (entry.isDirectory()) {
      files.push(...(await collectTestFiles(abs, rel)));
      continue;
    }
    if (entry.isFile() && /\.test\.tsx?$/.test(entry.name)) {
      files.push(rel.split(path.sep).join('/'));
    }
  }

  return files;
}

const ALLOWED_RUNNERS = new Set(['legacy', 'vitest']);

/**
 * Compare filesystem inventory with the versioned manifest.
 * Exit 1 on invalid runner, duplicate path, or missing/extra path;
 * print N test files accounted for on success.
 */
async function checkManifest() {
  const invalidRunners = [];
  for (const entry of TEST_MIGRATION_MANIFEST) {
    if (!ALLOWED_RUNNERS.has(entry.runner)) {
      invalidRunners.push(`${entry.path} (runner=${JSON.stringify(entry.runner)})`);
    }
  }
  if (invalidRunners.length > 0) {
    console.error('Invalid runner (must be legacy|vitest):');
    for (const line of invalidRunners) console.error(`  ! ${line}`);
    process.exit(1);
  }

  const rawPaths = TEST_MIGRATION_MANIFEST.map((e) => e.path);
  const seen = new Set();
  const duplicates = [];
  for (const p of rawPaths) {
    if (seen.has(p)) {
      if (!duplicates.includes(p)) duplicates.push(p);
    } else {
      seen.add(p);
    }
  }
  if (duplicates.length > 0) {
    console.error('Duplicate paths in manifest (each test must appear exactly once):');
    for (const p of duplicates) console.error(`  * ${p}`);
    process.exit(1);
  }

  const onDisk = (await collectTestFiles()).sort();
  const inManifest = [...rawPaths].sort();

  // Prefer sorted-array equality so length/order mismatches cannot be hidden by Set.
  const diskSet = new Set(onDisk);
  const manifestSet = new Set(inManifest);

  const missing = onDisk.filter((p) => !manifestSet.has(p));
  const extra = inManifest.filter((p) => !diskSet.has(p));

  if (missing.length > 0 || extra.length > 0 || onDisk.length !== inManifest.length) {
    if (missing.length > 0) {
      console.error('Missing from manifest (present on disk):');
      for (const p of missing) console.error(`  + ${p}`);
    }
    if (extra.length > 0) {
      console.error('Extra in manifest (not on disk):');
      for (const p of extra) console.error(`  - ${p}`);
    }
    if (missing.length === 0 && extra.length === 0 && onDisk.length !== inManifest.length) {
      console.error(
        `Path count mismatch: disk=${onDisk.length} manifest=${inManifest.length}`,
      );
    }
    process.exit(1);
  }

  console.log(`${inManifest.length} test files accounted for`);
}

const isMain =
  process.argv[1] &&
  path.resolve(process.argv[1]) === __filename;

if (isMain && process.argv.includes('--check')) {
  await checkManifest();
}
