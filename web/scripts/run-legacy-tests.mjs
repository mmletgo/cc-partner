import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { TEST_MIGRATION_MANIFEST } from './test-migration-manifest.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const WEB_ROOT = path.resolve(__dirname, '..');

const legacyEntries = TEST_MIGRATION_MANIFEST.filter((entry) => entry.runner === 'legacy');

if (legacyEntries.length === 0) {
  console.log('No legacy tests remain');
  process.exit(0);
}

for (const entry of legacyEntries) {
  const testPath = path.join(WEB_ROOT, entry.path);
  console.log(`legacy: ${entry.path}`);
  const result = spawnSync(process.execPath, ['--import', 'tsx', testPath], {
    stdio: 'inherit',
    cwd: WEB_ROOT,
  });

  if (result.error) {
    console.error(result.error);
    process.exit(1);
  }

  const code = result.status ?? 1;
  if (code !== 0) {
    process.exit(code);
  }
}

console.log(`legacy: ${legacyEntries.length} files passed`);
