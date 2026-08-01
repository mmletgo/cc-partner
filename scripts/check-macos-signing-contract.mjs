#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 规范化 SHA-256 指纹，忽略大小写与冒号。 */
function normalizeFingerprint(value) {
  return String(value ?? '').replaceAll(':', '').replaceAll(' ', '').toUpperCase();
}

/** 从 `codesign -dvvv` stderr 提取 Identifier 与 Authority 列表。 */
export function parseCodesignMetadata(output) {
  const lines = String(output).split(/\r?\n/u);
  const identifier = lines.find((line) => line.startsWith('Identifier='))?.slice(11) ?? '';
  const authorities = lines
    .filter((line) => line.startsWith('Authority='))
    .map((line) => line.slice(10).trim())
    .filter(Boolean);
  return { identifier, authorities };
}

/** 生成证书链导出参数；prefix 必须与长选项写在同一参数中，避免被解析为代码路径。 */
export function certificateExtractionArgs(prefix, appPath) {
  return ['-d', `--extract-certificates=${prefix}`, appPath];
}

/** 验证固定签名的稳定身份合同；不读取或输出私钥。 */
export function validateSigningMetadata(metadata, expected) {
  if (metadata.identifier !== expected.expectedIdentifier) {
    throw new Error(
      `Bundle ID mismatch: expected ${expected.expectedIdentifier}, got ${metadata.identifier || '<none>'}`,
    );
  }
  if (
    metadata.authorities.length === 0 ||
    /designated\s*=>\s*cdhash\b/iu.test(metadata.requirement)
  ) {
    throw new Error('ad-hoc signing is forbidden when fixed macOS signing was requested');
  }
  const actualFingerprint = normalizeFingerprint(metadata.certSha256);
  const expectedFingerprint = normalizeFingerprint(expected.expectedCertSha256);
  if (!expectedFingerprint) {
    throw new Error('expected certificate fingerprint is required');
  }
  if (actualFingerprint !== expectedFingerprint) {
    throw new Error(
      `certificate fingerprint mismatch: expected ${expectedFingerprint}, got ${actualFingerprint || '<none>'}`,
    );
  }
  if (!metadata.requirement.includes(`identifier "${expected.expectedIdentifier}"`)) {
    throw new Error('designated requirement does not bind the expected Bundle ID');
  }
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options });
  if (result.status !== 0) {
    throw new Error(
      `${command} failed: ${String(result.stderr || result.stdout || `exit ${result.status}`).trim()}`,
    );
  }
  return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

/** 读取 `.app` 的签名元数据并验证 nested code。 */
export function inspectSignedApp(appPath) {
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', appPath]);
  const details = parseCodesignMetadata(run('codesign', ['-dvvv', appPath]));
  const requirementOutput = run('codesign', ['-d', '-r-', appPath]);
  const requirement = requirementOutput
    .split(/\r?\n/u)
    .find((line) => line.includes('designated =>'))
    ?.trim() ?? '';

  const certDir = mkdtempSync(join(tmpdir(), 'cc-partner-signing-'));
  try {
    const certPrefix = join(certDir, 'cert');
    run('codesign', certificateExtractionArgs(certPrefix, appPath));
    const certPath = `${certPrefix}0`;
    if (!existsSync(certPath)) throw new Error('codesign did not extract a leaf certificate');
    const fingerprintOutput = run('openssl', [
      'x509',
      '-inform',
      'DER',
      '-in',
      certPath,
      '-noout',
      '-fingerprint',
      '-sha256',
    ]);
    const certSha256 = fingerprintOutput.split('=').at(-1)?.trim() ?? '';
    return { ...details, requirement, certSha256 };
  } finally {
    rmSync(certDir, { recursive: true, force: true });
  }
}

function main() {
  const [, , appArg, identifierArg] = process.argv;
  const expectedCertSha256 = process.env.CC_PARTNER_INTERNAL_CERT_SHA256 ?? '';
  if (!appArg || !identifierArg) {
    throw new Error('usage: check-macos-signing-contract.mjs <app-path> <expected-bundle-id>');
  }
  const appPath = resolve(appArg);
  const metadata = inspectSignedApp(appPath);
  validateSigningMetadata(metadata, {
    expectedIdentifier: identifierArg,
    expectedCertSha256,
  });
  const shortFingerprint = normalizeFingerprint(metadata.certSha256).slice(0, 12);
  console.log(
    `macOS signing contract passed: identifier=${metadata.identifier} authority=${metadata.authorities[0]} fingerprint=${shortFingerprint}… requirement=certificate`,
  );
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    main();
  } catch (error) {
    console.error(
      `[check-macos-signing-contract] ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exit(1);
  }
}
