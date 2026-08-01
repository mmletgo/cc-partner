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

test('uses the canonical dev identity when fixed signing is absent', () => {
  assert.deepEqual(resolveDevSigningChannel({}), {
    appName: 'cc-partner (Dev).app',
    bundleId: 'com.cc-partner.app.dev',
    displayName: 'cc-partner (Dev)',
    signingIdentity: '-',
    fixedSigning: false,
  });
});

test('fixed signing keeps the same canonical dev product identity', () => {
  assert.deepEqual(
    resolveDevSigningChannel({
      CC_PARTNER_INTERNAL_SIGNING_IDENTITY: 'cc-partner Internal Code Signing',
    }),
    {
      appName: 'cc-partner (Dev).app',
      bundleId: 'com.cc-partner.app.dev',
      displayName: 'cc-partner (Dev)',
      signingIdentity: 'cc-partner Internal Code Signing',
      fixedSigning: true,
    },
  );
});

test('stages every dev build at the stable Applications path', () => {
  assert.equal(
    resolveDevAppPath('/Users/tester'),
    '/Users/tester/Applications/cc-partner (Dev).app',
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

test('fixed macOS signing overlay inherits the canonical product identity and public updater feed', () => {
  const config = JSON.parse(
    readFileSync(join(repoRoot, 'src-tauri', 'tauri.internal.conf.json'), 'utf8'),
  );
  assert.equal(config.productName, undefined);
  assert.equal(config.identifier, undefined);
  assert.equal(
    config.bundle.macOS.signingIdentity,
    'cc-partner Internal Code Signing',
  );
  assert.equal(config.bundle.createUpdaterArtifacts, false);
  assert.equal(config.plugins, undefined);

  const publicRelease = readFileSync(
    join(repoRoot, '.github', 'workflows', 'release-tauri.yml'),
    'utf8',
  );
  assert.doesNotMatch(publicRelease, /platform:\s*['"]macos-/u);
  assert.doesNotMatch(publicRelease, /"darwin-(?:aarch64|x86_64)"/u);
});
