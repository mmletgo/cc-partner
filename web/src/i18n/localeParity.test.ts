/**
 * localeParity — zh/en 资源 key 对等合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   新增文案必须中英同提交；一侧缺 key 会在运行时回退 key 名或英文，破坏本地化体验。
 *
 * Code Logic（这个测试做什么）:
 *   递归展平 locales/en 与 locales/zh 下各 namespace JSON 的 leaf key 路径；
 *   归一化 i18next 复数后缀后断言两侧 key 集合一致。
 */

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, test } from 'vitest';

const localesRoot = join(dirname(fileURLToPath(import.meta.url)), 'locales');

/** i18next 复数后缀，parity 时折叠到基 key。 */
const PLURAL_SUFFIX_RE = /_(zero|one|two|few|many|other)$/;

/**
 * Business Logic（为什么需要这个函数）:
 *   嵌套 JSON 需要稳定的路径集合才能做跨语言 diff。
 *
 * Code Logic（这个函数做什么）:
 *   深度优先收集 leaf key 路径（dot 连接）。
 *
 * @param value 任意 JSON 值
 * @param prefix 当前路径前缀
 * @returns leaf 路径列表
 */
function flattenKeys(value: unknown, prefix = ''): string[] {
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    const entries = Object.entries(value as Record<string, unknown>);
    if (entries.length === 0) {
      return prefix ? [prefix] : [];
    }
    return entries.flatMap(([key, child]) => {
      const path = prefix ? `${prefix}.${key}` : key;
      return flattenKeys(child, path);
    });
  }
  return prefix ? [prefix] : [];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   英文常用 _one/_other，中文通常只有基 key；parity 应比较语义 key 而非原始后缀。
 *
 * Code Logic（这个函数做什么）:
 *   去掉 i18next 复数后缀得到基 key 集合。
 *
 * @param keys leaf 路径
 * @returns 归一化后的 Set
 */
function normalizeKeySet(keys: string[]): Set<string> {
  return new Set(keys.map((key) => key.replace(PLURAL_SUFFIX_RE, '')));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要读取某语言下全部 namespace 文件。
 *
 * Code Logic（这个函数做什么）:
 *   读取 locales/<lang>/*.json，返回 namespace → 对象映射。
 *
 * @param lang en | zh
 */
function loadLocaleNamespaceMap(lang: 'en' | 'zh'): Record<string, unknown> {
  const dir = join(localesRoot, lang);
  /** @type {Record<string, unknown>} */
  const map: Record<string, unknown> = {};
  for (const file of readdirSync(dir).sort()) {
    if (!file.endsWith('.json')) continue;
    const ns = file.replace(/\.json$/, '');
    map[ns] = JSON.parse(readFileSync(join(dir, file), 'utf8')) as unknown;
  }
  return map;
}

describe('localeParity', () => {
  test('en and zh expose the same namespace files', () => {
    const enFiles = readdirSync(join(localesRoot, 'en'))
      .filter((f) => f.endsWith('.json'))
      .sort();
    const zhFiles = readdirSync(join(localesRoot, 'zh'))
      .filter((f) => f.endsWith('.json'))
      .sort();
    expect(zhFiles).toEqual(enFiles);
  });

  test('en and zh leaf keys match after plural suffix normalization', () => {
    const enMap = loadLocaleNamespaceMap('en');
    const zhMap = loadLocaleNamespaceMap('zh');
    const namespaces = Object.keys(enMap).sort();

    const mismatches: string[] = [];
    for (const ns of namespaces) {
      const enKeys = normalizeKeySet(flattenKeys(enMap[ns]));
      const zhKeys = normalizeKeySet(flattenKeys(zhMap[ns]));
      for (const key of enKeys) {
        if (!zhKeys.has(key)) mismatches.push(`missing zh ${ns}:${key}`);
      }
      for (const key of zhKeys) {
        if (!enKeys.has(key)) mismatches.push(`missing en ${ns}:${key}`);
      }
    }

    expect(mismatches).toEqual([]);
  });
});
