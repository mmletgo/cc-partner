import type { ClaudeCodeAsset } from '@/lib/types';
import {
  ENABLED_OPTIONS,
  KIND_OPTIONS,
  matchesClaudeCodeAsset,
} from './claudeCodeAssets';
import type { EnabledFilter, KindFilter } from './claudeCodeAssets';

/**
 * Business Logic（为什么需要这个函数）:
 *   matchesClaudeCodeAsset 是 Claude Code 资产页 local/remote 两个 tab 共享的筛选契约，
 *   任意维度判断错误都会导致用户看到与筛选条件不符的资产，必须用断言锁死三维度组合行为。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * 构造一个最小可用的 ClaudeCodeAsset 测试夹具。
 */
function makeAsset(overrides: Partial<ClaudeCodeAsset>): ClaudeCodeAsset {
  return {
    kind: 'skill',
    id: 'skill-1',
    name: 'Test Skill',
    scope: 'user',
    enabled: true,
    source: 'local',
    canEnable: true,
    canUninstall: true,
    canExport: true,
    warnings: [],
    ...overrides,
  };
}

const enabledSkill = makeAsset({ kind: 'skill', id: 's1', name: 'Alpha', enabled: true, source: 'src-a', description: 'desc-a' });
const disabledSkill = makeAsset({ kind: 'skill', id: 's2', name: 'Beta', enabled: false, source: 'src-b', description: '' });
const enabledPlugin = makeAsset({ kind: 'plugin', id: 'p1', name: 'Gamma', enabled: true, source: 'src-c', description: '' });
const disabledMcp = makeAsset({ kind: 'mcp', id: 'm1', name: 'Delta', enabled: false, source: 'src-d', description: 'keyword-zzz' });

// kind 维度
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', '', 'all'),
  'all kind / 空 search / all enabled 应匹配任意资产',
);
assert(
  !matchesClaudeCodeAsset(enabledPlugin, 'skill', '', 'all'),
  'kind=skill 应拒绝 plugin',
);
assert(
  matchesClaudeCodeAsset(enabledPlugin, 'plugin', '', 'all'),
  'kind=plugin 应匹配 plugin',
);

// search 维度（匹配 name / id / source / description，大小写不敏感）
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', 'alpha', 'all'),
  'search 应匹配 name（大小写不敏感）',
);
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', 'src-a', 'all'),
  'search 应匹配 source',
);
assert(
  matchesClaudeCodeAsset(disabledMcp, 'all', 'keyword-zzz', 'all'),
  'search 应匹配 description',
);
assert(
  !matchesClaudeCodeAsset(enabledSkill, 'all', 'not-found', 'all'),
  'search 找不到时应拒绝',
);

// description 为 null/undefined 时不应把 'null'/'undefined' 字面量纳入 haystack
const nullDescAsset = makeAsset({ kind: 'skill', id: 'x1', name: 'Zeta', description: null });
const undefinedDescAsset = makeAsset({ kind: 'skill', id: 'x2', name: 'Eta', description: undefined });
assert(
  !matchesClaudeCodeAsset(nullDescAsset, 'all', 'null', 'all'),
  'description 为 null 时搜索 "null" 不应命中',
);
assert(
  !matchesClaudeCodeAsset(undefinedDescAsset, 'all', 'undefined', 'all'),
  'description 为 undefined 时搜索 "undefined" 不应命中',
);
assert(
  matchesClaudeCodeAsset(nullDescAsset, 'all', 'zeta', 'all'),
  'description 为 null 时仍可按 name 命中',
);

// enabled 维度
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', '', 'enabled'),
  'enabled 筛选应匹配 enabled 资产',
);
assert(
  !matchesClaudeCodeAsset(disabledSkill, 'all', '', 'enabled'),
  'enabled 筛选应拒绝 disabled 资产',
);
assert(
  matchesClaudeCodeAsset(disabledSkill, 'all', '', 'disabled'),
  'disabled 筛选应匹配 disabled 资产',
);
assert(
  !matchesClaudeCodeAsset(enabledSkill, 'all', '', 'disabled'),
  'disabled 筛选应拒绝 enabled 资产',
);

// 三维度组合
assert(
  matchesClaudeCodeAsset(disabledSkill, 'skill', 'beta', 'disabled'),
  '三维度同时命中应匹配',
);
assert(
  !matchesClaudeCodeAsset(disabledSkill, 'skill', 'beta', 'enabled'),
  'enabled 维度不命中时应拒绝，即使其他维度命中',
);
assert(
  !matchesClaudeCodeAsset(disabledMcp, 'skill', 'delta', 'disabled'),
  'kind 维度不命中时应拒绝，即使其他维度命中',
);

// 常量
assert(KIND_OPTIONS[0] === 'all', 'KIND_OPTIONS 首项应为 all');
assert(ENABLED_OPTIONS[0] === 'all', 'ENABLED_OPTIONS 首项应为 all');
assert(
  ENABLED_OPTIONS.length === 3 && ENABLED_OPTIONS.includes('enabled') && ENABLED_OPTIONS.includes('disabled'),
  'ENABLED_OPTIONS 应为 all/enabled/disabled 三态',
);

console.log('claudeCodeAssets.test.ts passed');
