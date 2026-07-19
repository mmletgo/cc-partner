#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const INTERNAL_SIGNING_IDENTITY = 'cc-partner Internal Code Signing';
export const DEFAULT_INTERNAL_CERT_PIN_PATH = join(
  homedir(),
  'Library',
  'Application Support',
  'cc-partner',
  'signing',
  'internal-cert.sha256',
);

/** 规范化证书指纹，拒绝长度错误或非十六进制内容。 */
function normalizeFingerprint(value, length, label) {
  const normalized = String(value ?? '').replaceAll(':', '').replaceAll(' ', '').trim().toUpperCase();
  if (!new RegExp(`^[A-F0-9]{${length}}$`, 'u').test(normalized)) {
    throw new Error(`${label}格式无效`);
  }
  return normalized;
}

/** 从 `security find-identity` 中提取固定名称的代码签名 identity。 */
export function parseMatchingIdentities(output) {
  const escapedIdentity = INTERNAL_SIGNING_IDENTITY.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
  const pattern = new RegExp(
    `^\\s*\\d+\\)\\s+([A-Fa-f0-9]{40})\\s+"${escapedIdentity}"\\s*$`,
    'gmu',
  );
  return [...String(output).matchAll(pattern)].map((match) => match[1].toUpperCase());
}

/** 从 `security find-certificate -Z` 中按 SHA-1 identity 定位 leaf SHA-256。 */
export function certificateSha256ForIdentity(output, identitySha1) {
  const records = String(output).split(/(?=SHA-256 hash:)/u);
  for (const record of records) {
    const sha256 = record.match(/SHA-256 hash:\s*([A-Fa-f0-9:]+)/u)?.[1];
    const sha1 = record.match(/SHA-1 hash:\s*([A-Fa-f0-9:]+)/u)?.[1];
    if (!sha256 || !sha1) continue;
    if (normalizeFingerprint(sha1, 40, '证书 SHA-1 ') === identitySha1) {
      return normalizeFingerprint(sha256, 64, '证书 SHA-256 ');
    }
  }
  throw new Error('固定代码签名 identity 找不到对应证书');
}

/** 调用 macOS security CLI，并把失败收口为不含凭据的错误。 */
function runSecurity(args) {
  const result = spawnSync('security', args, { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(
      `security ${args[0]} 失败: ${String(result.stderr || result.stdout || `exit ${result.status}`).trim()}`,
    );
  }
  return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

/**
 * 自动发现并固定本机内部签名身份。
 *
 * 首次发现唯一 identity 时写入非敏感 SHA-256 pin；以后同名证书发生漂移会 fail closed，
 * 避免开发者无感切换 TCC 主体。没有 identity 时返回 null，供开源贡献者使用社区 Dev 壳。
 */
export function detectAndPinInternalSigning({
  platform = process.platform,
  pinPath = DEFAULT_INTERNAL_CERT_PIN_PATH,
  runSecurity: securityRunner = runSecurity,
} = {}) {
  if (platform !== 'darwin') return null;

  const identities = parseMatchingIdentities(
    securityRunner(['find-identity', '-v', '-p', 'codesigning']),
  );
  if (identities.length === 0) return null;
  if (identities.length > 1) {
    throw new Error(`Keychain 中存在多个同名代码签名 identity: ${INTERNAL_SIGNING_IDENTITY}`);
  }

  const fingerprint = certificateSha256ForIdentity(
    securityRunner(['find-certificate', '-a', '-c', INTERNAL_SIGNING_IDENTITY, '-Z']),
    identities[0],
  );

  let pinCreated = false;
  if (existsSync(pinPath)) {
    const pinned = normalizeFingerprint(readFileSync(pinPath, 'utf8'), 64, '本地证书 pin ');
    if (pinned !== fingerprint) {
      throw new Error(
        `已安装证书指纹与本地 pin 不一致；如确需轮换证书，请按 macOS 内部签名手册执行 TCC 主体迁移`,
      );
    }
  } else {
    mkdirSync(dirname(pinPath), { recursive: true, mode: 0o700 });
    try {
      writeFileSync(pinPath, `${fingerprint}\n`, {
        encoding: 'utf8',
        flag: 'wx',
        mode: 0o600,
      });
      pinCreated = true;
    } catch (error) {
      if (!(error instanceof Error) || !('code' in error) || error.code !== 'EEXIST') throw error;
      const pinned = normalizeFingerprint(readFileSync(pinPath, 'utf8'), 64, '本地证书 pin ');
      if (pinned !== fingerprint) {
        throw new Error('并发创建的本地证书 pin 与已安装证书不一致');
      }
    }
  }

  return {
    identity: INTERNAL_SIGNING_IDENTITY,
    fingerprint,
    pinPath,
    pinCreated,
  };
}

function main() {
  const result = detectAndPinInternalSigning();
  if (result) process.stdout.write(`${result.fingerprint}\n`);
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    main();
  } catch (error) {
    console.error(
      `[detect-macos-internal-signing] ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exit(1);
  }
}
