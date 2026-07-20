import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'vitest';
import {
  formatMobileAccessChipLabel,
  resolveMobileAccessEntries,
  resolveSelectedMobileAccessEntry,
  selectDefaultMobileAccessEntryId,
} from './mobileAccessSelection';
import { selectPrimaryMobileUrl } from './mobileQr';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 未启用 Node 类型，测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让用例失败。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   风险文案测试需要读取 locale 文件，确认固定无身份声明已落地。
 *
 * Code Logic（这个函数做什么）:
 *   相对本测试文件定位 locales 路径并按 UTF-8 读取完整 JSON 文本。
 */
function readLocale(relativeFromLocales: string): string {
  const here = dirname(fileURLToPath(import.meta.url));
  const path = resolve(here, '../../../i18n/locales', relativeFromLocales);
  return readFileSync(path, 'utf8');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   固定 LAN 风险措辞禁止出现误导性“安全/已认证/可信设备”等词。
 *
 * Code Logic（这个函数做什么）:
 *   扫描文本，命中任一禁用词时抛错。
 */
function assertNoForbiddenRiskWords(text: string, label: string): void {
  const forbidden = ['安全', '已认证', '可信设备', 'trusted' + ' device', 'authenticated' + ' device'];
  for (const word of forbidden) {
    if (text.includes(word)) {
      throw new Error(`${label} must not contain forbidden risk word: ${word}`);
    }
  }
}

describe('mobileAccessCard', () => {
  test('selects the primary LAN URL for the QR code', () => {
    assertEqual(
      selectPrimaryMobileUrl(['http://192.168.1.23:51842/mobile']),
      'http://192.168.1.23:51842/mobile',
      'first LAN URL should be selected',
    );
    assertEqual(
      selectPrimaryMobileUrl(['', '  ', 'http://10.0.0.8:51842/mobile']),
      'http://10.0.0.8:51842/mobile',
      'blank URLs should be skipped',
    );
    assertEqual(selectPrimaryMobileUrl([]), null, 'empty URL list should return null');
  });

  test('resolveMobileAccessEntries prefers entries and falls back to urls', () => {
    const fromEntries = resolveMobileAccessEntries({
      deviceName: 'd',
      port: 1,
      urls: ['http://1.1.1.1:1/mobile'],
      entries: [
        {
          id: '192.168.1.2',
          url: 'http://192.168.1.2:1/mobile',
          host: '192.168.1.2',
          role: 'wifi',
          isDefault: true,
        },
      ],
    });
    assertEqual(fromEntries.length, 1, 'entries length');
    assertEqual(fromEntries[0]?.host, '192.168.1.2', 'host');

    const fromUrls = resolveMobileAccessEntries({
      deviceName: 'd',
      port: 1,
      urls: ['http://10.0.0.2:1/mobile', ''],
      entries: [],
    });
    assertEqual(fromUrls.length, 1, 'url fallback length');
    assertEqual(fromUrls[0]?.host, '10.0.0.2', 'url host');
    assertEqual(fromUrls[0]?.isDefault, true, 'first url default');
  });

  test('selectDefaultMobileAccessEntryId prefers isDefault', () => {
    const id = selectDefaultMobileAccessEntryId([
      { id: 'a', url: 'u1', host: 'a', isDefault: false },
      { id: 'b', url: 'u2', host: 'b', isDefault: true },
    ]);
    assertEqual(id, 'b', 'default id');
  });

  test('formatMobileAccessChipLabel uses role or bare ip', () => {
    const labels = {
      wifi: (ip: string) => `Wi‑Fi · ${ip}`,
      wired: (ip: string) => `有线 · ${ip}`,
    };
    assertEqual(
      formatMobileAccessChipLabel(
        { id: '1', url: 'u', host: '1.2.3.4', role: 'wifi', isDefault: false },
        labels,
      ),
      'Wi‑Fi · 1.2.3.4',
      'wifi label',
    );
    assertEqual(
      formatMobileAccessChipLabel(
        { id: '1', url: 'u', host: '1.2.3.4', isDefault: false },
        labels,
      ),
      '1.2.3.4',
      'bare ip',
    );
  });

  test('resolveSelectedMobileAccessEntry keeps selection or falls back', () => {
    const entries = [
      { id: 'a', url: 'u1', host: 'a', isDefault: true },
      { id: 'b', url: 'u2', host: 'b', isDefault: false },
    ];
    assertEqual(resolveSelectedMobileAccessEntry(entries, 'b')?.id, 'b', 'keep');
    assertEqual(resolveSelectedMobileAccessEntry(entries, 'missing')?.id, 'a', 'fallback default');
  });

  test('fixed unauthenticated lan risk wording is present without mode claims', () => {
    const zh = JSON.parse(readLocale('zh/settings.json')) as {
      mobileAccess: { warning: string };
      lanFirewall: { riskNotice: string };
    };
    const en = JSON.parse(readLocale('en/settings.json')) as {
      mobileAccess: { warning: string };
      lanFirewall: { riskNotice: string };
    };
    const zhFixed =
      '同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份';
    const enFixed =
      'Any device on the same reachable network can read, write, and execute; the system does not verify caller identity';

    if (!zh.mobileAccess.warning.includes(zhFixed)) {
      throw new Error('zh mobileAccess.warning must include fixed no-identity statement');
    }
    if (!zh.lanFirewall.riskNotice.includes(zhFixed)) {
      throw new Error('zh lanFirewall.riskNotice must include fixed no-identity statement');
    }
    if (!en.mobileAccess.warning.includes(enFixed)) {
      throw new Error('en mobileAccess.warning must include fixed no-identity statement');
    }
    if (!en.lanFirewall.riskNotice.includes(enFixed)) {
      throw new Error('en lanFirewall.riskNotice must include fixed no-identity statement');
    }
    if (
      zh.mobileAccess.warning.includes('可信局域网') ||
      en.mobileAccess.warning.toLowerCase().includes('trusted personal lan')
    ) {
      throw new Error('vague trusted-LAN wording must be replaced');
    }
    // 只扫描风险声明本身，避免误伤系统产品名（如 Windows Defender “高级安全”）。
    assertNoForbiddenRiskWords(zh.mobileAccess.warning, 'zh mobileAccess.warning');
    assertNoForbiddenRiskWords(zh.lanFirewall.riskNotice, 'zh lanFirewall.riskNotice');
    assertNoForbiddenRiskWords(en.mobileAccess.warning, 'en mobileAccess.warning');
    assertNoForbiddenRiskWords(en.lanFirewall.riskNotice, 'en lanFirewall.riskNotice');

    // MobileAccessCard still renders warning + QR/URL grid when URLs exist (markup contract).
    const cardSource = readFileSync(
      resolve(dirname(fileURLToPath(import.meta.url)), './MobileAccessCard.tsx'),
      'utf8',
    );
    if (!cardSource.includes("t('mobileAccess.warning')")) {
      throw new Error('MobileAccessCard must keep the existing warning area');
    }
    if (!cardSource.includes('primaryUrl') || !cardSource.includes('qrSvg')) {
      throw new Error('MobileAccessCard must still show QR/URLs when LAN URLs exist');
    }
    if (!cardSource.includes('role="radiogroup"')) {
      throw new Error('MobileAccessCard must expose a radiogroup for multi-LAN chips');
    }
    if (!cardSource.includes('entries.length >= 2') && !cardSource.includes('entries.length >=2')) {
      throw new Error('MobileAccessCard must show network chips only when entries.length >= 2');
    }
    if (!cardSource.includes('resolveSelectedMobileAccessEntry')) {
      throw new Error('MobileAccessCard must resolve a single selected entry for primaryUrl');
    }
    if (cardSource.includes('(info?.urls ?? []).map')) {
      throw new Error('MobileAccessCard must not map all info.urls for display');
    }

    const firewallCard = readFileSync(
      resolve(
        dirname(fileURLToPath(import.meta.url)),
        '../LanFirewallDependencyCard/LanFirewallDependencyCard.tsx',
      ),
      'utf8',
    );
    if (!firewallCard.includes("t('settings:lanFirewall.riskNotice')")) {
      throw new Error('LanFirewallDependencyCard must surface fixed risk notice');
    }
    if (
      !firewallCard.includes('status.lanIp') ||
      !firewallCard.includes('status.httpPort') ||
      !firewallCard.includes('status.mdnsPort')
    ) {
      throw new Error('LanFirewallDependencyCard must still show LAN IP / HTTP port / UDP 5353');
    }
  });
});
