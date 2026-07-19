import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  resolveDevAppPath,
  resolveDevSigningChannel,
} from './prepare-macos-dev-app.mjs';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');

test('uses community unavailable identity when internal signing env is absent', () => {
  assert.deepEqual(resolveDevSigningChannel({}), {
    appName: 'cc-partner-dev.app',
    bundleId: 'com.cc-partner.app.dev',
    displayName: 'cc-partner (Dev)',
    signingIdentity: '-',
    internal: false,
  });
});

test('uses fixed internal dev identity without ad-hoc fallback', () => {
  assert.deepEqual(
    resolveDevSigningChannel({
      CC_PARTNER_INTERNAL_SIGNING_IDENTITY: 'cc-partner Internal Code Signing',
    }),
    {
      appName: 'cc-partner Internal (Dev).app',
      bundleId: 'com.cc-partner.app.internal.dev',
      displayName: 'cc-partner Internal (Dev)',
      signingIdentity: 'cc-partner Internal Code Signing',
      internal: true,
    },
  );
});

test('stages internal dev app at a stable user Applications path', () => {
  const internalChannel = resolveDevSigningChannel({
    CC_PARTNER_INTERNAL_SIGNING_IDENTITY: 'cc-partner Internal Code Signing',
  });
  const communityChannel = resolveDevSigningChannel({});

  assert.equal(
    resolveDevAppPath(internalChannel, '/tmp/debug', '/Users/tester'),
    '/Users/tester/Applications/cc-partner Internal (Dev).app',
  );
  assert.equal(
    resolveDevAppPath(communityChannel, '/tmp/debug', '/Users/tester'),
    '/tmp/debug/cc-partner-dev.app',
  );
});

test('runner matches the friendly app path literally instead of as a regular expression', () => {
  const runner = readFileSync(
    join(repoRoot, 'scripts', 'macos-dev-cargo-runner.sh'),
    'utf8',
  );

  assert.doesNotMatch(runner, /p(?:g|k)rep\s+-f/u);
  assert.match(runner, /index\(command, expected " "\) == 1/u);
});

test('keeps internal stable identity and updater feed isolated from public releases', () => {
  const config = JSON.parse(
    readFileSync(join(repoRoot, 'src-tauri', 'tauri.internal.conf.json'), 'utf8'),
  );
  assert.equal(config.identifier, 'com.cc-partner.app.internal');
  assert.equal(
    config.bundle.macOS.signingIdentity,
    'cc-partner Internal Code Signing',
  );
  assert.equal(config.bundle.createUpdaterArtifacts, false);
  assert.match(config.plugins.updater.endpoints[0], /\/internal-macos\/latest\.json$/u);

  const publicRelease = readFileSync(
    join(repoRoot, '.github', 'workflows', 'release-tauri.yml'),
    'utf8',
  );
  assert.doesNotMatch(publicRelease, /platform:\s*['"]macos-/u);
  assert.doesNotMatch(publicRelease, /"darwin-(?:aarch64|x86_64)"/u);
});
