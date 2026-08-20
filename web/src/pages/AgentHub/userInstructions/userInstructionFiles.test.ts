/**
 * 用户级原生提示词路径解析测试。
 *
 * Business Logic: 原始栏只认各 Agent 配置目录里的 AGENTS.md / CLAUDE.md / GEMINI.md。
 * Code Logic: nativeOriginalForAgent 排除 Hub exclusive/override；OpenCode 缺文件时回退 Claude。
 */

import { describe, expect, test } from 'vitest';
import type {
  UserInstructionSourceDto,
  UserInstructionTargetDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';
import {
  canonicalUserFileId,
  nativeOriginalForAgent,
} from './userInstructionFiles';

function source(
  overrides: Partial<UserInstructionSourceDto> & Pick<UserInstructionSourceDto, 'sourceId' | 'path'>,
): UserInstructionSourceDto {
  return {
    role: 'native',
    active: true,
    exists: true,
    nonEmpty: true,
    hash: 'hash',
    modifiedAt: null,
    ownership: 'external',
    ...overrides,
  };
}

function target(
  overrides: Partial<UserInstructionTargetDto> & Pick<UserInstructionTargetDto, 'target'>,
): UserInstructionTargetDto {
  return {
    cli: { installed: true, version: '1.0', configRoot: `/config/${overrides.target}` },
    sources: [],
    effectiveSourceId: null,
    managedTargetPath: null,
    managementMode: 'unmanaged',
    capability: {
      scan: 'supported',
      write: 'supported',
      remove: 'supported',
      activate: 'newSession',
      reasonCode: null,
      evidenceIds: [],
    },
    projection: {
      state: 'none',
      desiredRevisionId: null,
      appliedRevisionId: null,
      observedHash: null,
      lastErrorCode: null,
    },
    availableActions: [],
    ...overrides,
  };
}

function workspace(targets: UserInstructionTargetDto[]): UserInstructionWorkspaceDto {
  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'configured',
    healthState: 'healthy',
    inventorySnapshotHash: 'inventory-1',
    refreshedAt: '2026-08-08T00:00:00.000Z',
    canonical: null,
    targets,
  };
}

describe('nativeOriginalForAgent', () => {
  test('Codex home AGENTS.md is not the same file as OpenCode AGENTS.md', () => {
    const dto = workspace([
      target({
        target: 'codex',
        managedTargetPath: '/home/user/.codex/AGENTS.md',
      }),
      target({
        target: 'opencode',
        cli: { installed: true, version: '1.0', configRoot: '/home/user/.config/opencode' },
        sources: [
          source({
            sourceId: 'oc',
            path: '/home/user/.config/opencode/AGENTS.md',
          }),
        ],
      }),
    ]);
    const codex = nativeOriginalForAgent(dto, 'codex');
    const opencode = nativeOriginalForAgent(dto, 'opencode');
    expect(codex.path).toBe('/home/user/.codex/AGENTS.md');
    expect(opencode.path).toBe('/home/user/.config/opencode/AGENTS.md');
    expect(canonicalUserFileId(codex.path ?? '')).not.toBe(
      canonicalUserFileId(opencode.path ?? ''),
    );
  });

  test('OpenCode without AGENTS.md shares Claude CLAUDE.md', () => {
    const dto = workspace([
      target({
        target: 'claude',
        sources: [
          source({
            sourceId: 'claude',
            path: '/home/user/.claude/CLAUDE.md',
            content: '# Claude',
          }),
        ],
        managedTargetPath: '/home/user/.claude/CLAUDE.md',
      }),
      target({
        target: 'opencode',
        cli: { installed: true, version: '1.0', configRoot: '/home/user/.config/opencode' },
        sources: [
          source({
            sourceId: 'fallback',
            path: '/home/user/.claude/CLAUDE.md',
            role: 'fallback',
          }),
        ],
      }),
    ]);
    expect(nativeOriginalForAgent(dto, 'opencode').path).toBe('/home/user/.claude/CLAUDE.md');
    expect(nativeOriginalForAgent(dto, 'claude').path).toBe('/home/user/.claude/CLAUDE.md');
  });

  test('Cursor has no user-level AGENTS.md original', () => {
    const dto = workspace([
      target({
        target: 'cursor',
        sources: [
          source({
            sourceId: 'rules',
            path: '/home/user/.cursor/rules/cc-partner.mdc',
          }),
        ],
        managedTargetPath: '/home/user/.cursor/rules/cc-partner.mdc',
      }),
    ]);
    expect(nativeOriginalForAgent(dto, 'cursor')).toEqual({ path: null, source: null });
  });
});
