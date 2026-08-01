import assert from 'node:assert/strict';
import test from 'node:test';
import {
  certificateExtractionArgs,
  parseCodesignMetadata,
  validateSigningMetadata,
} from './check-macos-signing-contract.mjs';

test('binds certificate prefix to the codesign long option', () => {
  assert.deepEqual(certificateExtractionArgs('/tmp/signing/cert', '/Applications/cc-partner.app'), [
    '-d',
    '--extract-certificates=/tmp/signing/cert',
    '/Applications/cc-partner.app',
  ]);
});

test('rejects ad-hoc cdhash-only signing when fixed signing was requested', () => {
  assert.throws(
    () =>
      validateSigningMetadata(
        {
          identifier: 'com.cc-partner.app',
          authorities: [],
          requirement: 'designated => cdhash H"ABCDEF"',
          certSha256: 'AA11',
        },
        {
          expectedIdentifier: 'com.cc-partner.app',
          expectedCertSha256: 'AA11',
        },
      ),
    /ad-hoc/i,
  );
});

test('rejects bundle id and certificate fingerprint drift', () => {
  const stable = {
    identifier: 'com.cc-partner.app.dev',
    authorities: ['cc-partner Internal Code Signing'],
    requirement:
      'designated => identifier "com.cc-partner.app.dev" and certificate leaf[subject.CN] = "cc-partner Internal Code Signing"',
    certSha256: 'AA11',
  };
  assert.throws(
    () =>
      validateSigningMetadata(stable, {
        expectedIdentifier: 'com.cc-partner.app',
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

test('accepts stable certificate requirement for the canonical release identity', () => {
  const parsed = parseCodesignMetadata(`Executable=/Applications/cc-partner.app/Contents/MacOS/cc-partner
Identifier=com.cc-partner.app
Authority=cc-partner Internal Code Signing
TeamIdentifier=not set`);
  const metadata = {
    ...parsed,
    requirement:
      'designated => identifier "com.cc-partner.app" and certificate leaf[subject.CN] = "cc-partner Internal Code Signing"',
    certSha256: 'AA:11',
  };

  assert.doesNotThrow(() =>
    validateSigningMetadata(metadata, {
      expectedIdentifier: 'com.cc-partner.app',
      expectedCertSha256: 'aa11',
    }),
  );
});
