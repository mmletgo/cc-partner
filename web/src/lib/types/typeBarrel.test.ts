/**
 * 类型 barrel 与域模块边界 contract。
 *
 * Business Logic（为什么需要这个测试）:
 *   `lib/types.ts` 巨型 monolith 按 core/settings/workbench/orchestrator/attention 拆分后，
 *   旧路径 `@/lib/types` 必须继续导出相同公共类型；域模块禁止回指兼容 barrel，避免循环依赖。
 *
 * Code Logic（这个测试做什么）:
 *   1) 从旧 barrel 与新域路径导入代表性类型，用 expectTypeOf 断言形状一致；
 *   2) 读取域源文件，断言不出现 `from '../types'` / `from '@/lib/types'` 依赖。
 */

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, expectTypeOf, test } from 'vitest';

import type {
  AppConfig as BarrelAppConfig,
  AttentionSnapshot as BarrelAttentionSnapshot,
  Device as BarrelDevice,
  OrchestratorTask as BarrelOrchestratorTask,
  PortableInventorySnapshotDto as BarrelPortableInventorySnapshotDto,
  Prompt as BarrelPrompt,
  WorkbenchProject as BarrelWorkbenchProject,
} from '@/lib/types';
import type { Device as CoreDevice, Prompt as CorePrompt } from '@/lib/types/core';
import type { AppConfig as SettingsAppConfig } from '@/lib/types/settings';
import type { WorkbenchProject as WorkbenchProjectType } from '@/lib/types/workbench';
import type { OrchestratorTask as OrchestratorTaskType } from '@/lib/types/orchestrator';
import type { AttentionSnapshot as AttentionSnapshotType } from '@/lib/types/attention';
import type { PortableInventorySnapshotDto as PortableInventorySnapshotType } from '@/lib/types/portableInventory';

const typesDir = dirname(fileURLToPath(import.meta.url));

const DOMAIN_MODULES = [
  'core.ts',
  'settings.ts',
  'workbench.ts',
  'orchestrator.ts',
  'attention.ts',
  'portableInventory.ts',
  'index.ts',
] as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   域拆分后任何回指兼容 barrel 的 import 都会在大仓里埋下循环依赖与模糊边界。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 types 目录下非测试的 .ts 源，匹配禁止的 barrel import 路径。
 */
function listDomainSourceFiles(): string[] {
  return readdirSync(typesDir)
    .filter((name) => name.endsWith('.ts') && !name.endsWith('.test.ts'))
    .map((name) => join(typesDir, name));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要把「禁止 barrel 回指」写成可自动失败的断言。
 *
 * Code Logic（这个函数做什么）:
 *   对源文件内容做正则匹配 `from '../types'` 与 `from '@/lib/types'`（含 type-only）。
 */
function findForbiddenBarrelImports(source: string): string[] {
  const patterns = [
    /from\s+['"]\.\.\/types['"]/g,
    /from\s+['"]@\/lib\/types['"]/g,
  ];
  const hits: string[] = [];
  for (const pattern of patterns) {
    const matches = source.match(pattern);
    if (matches) hits.push(...matches);
  }
  return hits;
}

describe('type barrel domain split', () => {
  test('domain modules exist on disk', () => {
    for (const name of DOMAIN_MODULES) {
      expect(existsSync(join(typesDir, name)), `missing domain module: ${name}`).toBe(true);
    }
  });

  test('legacy barrel types match domain module shapes', () => {
    // expectTypeOf 主要在 tsc / vitest typecheck 阶段生效；配合 npm run build 的 tsc -b 锁形状。
    expectTypeOf<BarrelPrompt>().toEqualTypeOf<CorePrompt>();
    expectTypeOf<BarrelDevice>().toEqualTypeOf<CoreDevice>();
    expectTypeOf<BarrelAppConfig>().toEqualTypeOf<SettingsAppConfig>();
    expectTypeOf<BarrelWorkbenchProject>().toEqualTypeOf<WorkbenchProjectType>();
    expectTypeOf<BarrelOrchestratorTask>().toEqualTypeOf<OrchestratorTaskType>();
    expectTypeOf<BarrelAttentionSnapshot>().toEqualTypeOf<AttentionSnapshotType>();
    expectTypeOf<BarrelPortableInventorySnapshotDto>().toEqualTypeOf<PortableInventorySnapshotType>();
  });

  test('domain source files never import the compatibility barrel', () => {
    const sources = listDomainSourceFiles();
    expect(sources.length).toBeGreaterThan(0);

    const violations: string[] = [];
    for (const filePath of sources) {
      const base = filePath.split(/[/\\]/).pop() ?? filePath;
      // 域源与 index barrel 都禁止回指兼容层 `../types` 或 `@/lib/types`。
      const source = readFileSync(filePath, 'utf8');
      const hits = findForbiddenBarrelImports(source);
      for (const hit of hits) {
        violations.push(`${base}: ${hit}`);
      }
    }

    expect(violations, `forbidden barrel imports: ${violations.join('; ')}`).toEqual([]);
  });
});
