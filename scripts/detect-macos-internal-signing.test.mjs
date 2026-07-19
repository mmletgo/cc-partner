import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  detectAndPinInternalSigning,
  INTERNAL_SIGNING_IDENTITY,
} from './detect-macos-internal-signing.mjs';

const SHA1 = '532FCA9394006D32871E9EED0F1A4EE6DA919D4F';
const SHA256 = '4ED3E49F0185198292992593A9EBB17387E3BD0560E6D1EB58BBF72140E0E380';

function identityOutput(count = 1) {
  return Array.from(
    { length: count },
    (_, index) => `  ${index + 1}) ${SHA1} "${INTERNAL_SIGNING_IDENTITY}"`,
  ).join('\n');
}

function certificateOutput() {
  return `SHA-256 hash: ${SHA256}\nSHA-1 hash: ${SHA1}\nkeychain: "/tmp/login.keychain-db"`;
}

function securityStub({ identities = identityOutput(), certificates = certificateOutput() } = {}) {
  return (args) => {
    if (args[0] === 'find-identity') return identities;
    if (args[0] === 'find-certificate') return certificates;
    throw new Error(`unexpected security args: ${args.join(' ')}`);
  };
}

test('returns community mode when the fixed identity is absent', () => {
  const result = detectAndPinInternalSigning({
    platform: 'darwin',
    pinPath: join(mkdtempSync(join(tmpdir(), 'cc-signing-test-')), 'pin'),
    runSecurity: securityStub({ identities: '0 valid identities found' }),
  });
  assert.equal(result, null);
});

test('detects the unique identity and creates a stable local fingerprint pin', () => {
  const pinPath = join(mkdtempSync(join(tmpdir(), 'cc-signing-test-')), 'signing', 'internal-cert.sha256');
  const result = detectAndPinInternalSigning({
    platform: 'darwin',
    pinPath,
    runSecurity: securityStub(),
  });

  assert.deepEqual(result, {
    identity: INTERNAL_SIGNING_IDENTITY,
    fingerprint: SHA256,
    pinPath,
    pinCreated: true,
  });
  assert.equal(readFileSync(pinPath, 'utf8'), `${SHA256}\n`);
});

test('reuses the pinned fingerprint without rotating the TCC subject', () => {
  const pinPath = join(mkdtempSync(join(tmpdir(), 'cc-signing-test-')), 'internal-cert.sha256');
  writeFileSync(pinPath, `${SHA256.toLowerCase()}\n`, 'utf8');

  const result = detectAndPinInternalSigning({
    platform: 'darwin',
    pinPath,
    runSecurity: securityStub(),
  });
  assert.equal(result?.fingerprint, SHA256);
  assert.equal(result?.pinCreated, false);
});

test('fails closed when the fixed identity is ambiguous', () => {
  assert.throws(
    () =>
      detectAndPinInternalSigning({
        platform: 'darwin',
        pinPath: join(mkdtempSync(join(tmpdir(), 'cc-signing-test-')), 'pin'),
        runSecurity: securityStub({ identities: identityOutput(2) }),
      }),
    /多个同名/,
  );
});

test('fails closed when the installed certificate drifts from the local pin', () => {
  const pinPath = join(mkdtempSync(join(tmpdir(), 'cc-signing-test-')), 'internal-cert.sha256');
  writeFileSync(pinPath, `${'A'.repeat(64)}\n`, 'utf8');

  assert.throws(
    () =>
      detectAndPinInternalSigning({
        platform: 'darwin',
        pinPath,
        runSecurity: securityStub(),
      }),
    /指纹与本地 pin 不一致/,
  );
});
