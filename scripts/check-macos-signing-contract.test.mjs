import assert from 'node:assert/strict';
import test from 'node:test';
import {
  parseCodesignMetadata,
  validateSigningMetadata,
} from './check-macos-signing-contract.mjs';

test('rejects ad-hoc cdhash-only signing', () => {
  assert.throws(
    () =>
      validateSigningMetadata(
        {
          identifier: 'com.cc-partner.app.internal',
          authorities: [],
          requirement: 'designated => cdhash H"ABCDEF"',
          certSha256: 'AA11',
        },
        {
          expectedIdentifier: 'com.cc-partner.app.internal',
          expectedCertSha256: 'AA11',
        },
      ),
    /ad-hoc/i,
  );
});

test('rejects bundle id and certificate fingerprint drift', () => {
  const stable = {
    identifier: 'com.cc-partner.app.internal.dev',
    authorities: ['cc-partner Internal Code Signing'],
    requirement:
      'designated => identifier "com.cc-partner.app.internal.dev" and certificate leaf[subject.CN] = "cc-partner Internal Code Signing"',
    certSha256: 'AA11',
  };
  assert.throws(
    () =>
      validateSigningMetadata(stable, {
        expectedIdentifier: 'com.cc-partner.app.internal',
        expectedCertSha256: 'AA11',
      }),
    /Bundle ID/,
  );
  assert.throws(
    () =>
      validateSigningMetadata(stable, {
        expectedIdentifier: stable.identifier,
        expectedCertSha256: 'BB22',
      }),
    /fingerprint/i,
  );
});

test('accepts stable certificate requirement and parses codesign output', () => {
  const parsed = parseCodesignMetadata(`Executable=/Applications/cc-partner Internal.app/Contents/MacOS/cc-partner
Identifier=com.cc-partner.app.internal
Authority=cc-partner Internal Code Signing
TeamIdentifier=not set`);
  const metadata = {
    ...parsed,
    requirement:
      'designated => identifier "com.cc-partner.app.internal" and certificate leaf[subject.CN] = "cc-partner Internal Code Signing"',
    certSha256: 'AA:11',
  };

  assert.doesNotThrow(() =>
    validateSigningMetadata(metadata, {
      expectedIdentifier: 'com.cc-partner.app.internal',
      expectedCertSha256: 'aa11',
    }),
  );
});
