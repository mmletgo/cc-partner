/**
 * 项目级原生提示词文件目录纯函数测试。
 */

import { describe, expect, test } from 'vitest';
import {
  filesForAgent,
  matchProjectInstructionNodeName,
  resolveActiveFileId,
  shouldGuardProjectInstructionContextChange,
} from './projectInstructionFiles';

describe('projectInstructionFiles', () => {
  test('most agents share AGENTS.md while Claude and Gemini keep their own files', () => {
    expect(filesForAgent('codex').map((file) => file.path)).toEqual(['AGENTS.md']);
    expect(filesForAgent('opencode').map((file) => file.path)).toEqual(['AGENTS.md']);
    expect(filesForAgent('claude').map((file) => file.path)).toEqual(['CLAUDE.md']);
    expect(filesForAgent('gemini').map((file) => file.path)).toEqual(['GEMINI.md']);
    expect(filesForAgent('grok').map((file) => file.path)).toEqual(['AGENTS.md', 'CLAUDE.md']);
    expect(filesForAgent('cursor').map((file) => file.path)).toEqual(['AGENTS.md', 'CLAUDE.md']);
    expect(filesForAgent('pi').map((file) => file.path)).toEqual(['AGENTS.md', 'CLAUDE.md']);
  });

  test('switching among AGENTS.md consumers keeps the shared file selected', () => {
    expect(resolveActiveFileId('opencode', 'agents')).toBe('agents');
    expect(resolveActiveFileId('grok', 'agents')).toBe('agents');
    expect(resolveActiveFileId('claude', 'agents')).toBe('claude');
    expect(resolveActiveFileId('gemini', 'claude')).toBe('gemini');
    expect(resolveActiveFileId('claude', 'claude')).toBe('claude');
    expect(resolveActiveFileId('grok', 'claude')).toBe('claude');
  });

  test('dirty AGENTS.md does not guard Codex → OpenCode, but guards Codex → Claude', () => {
    expect(
      shouldGuardProjectInstructionContextChange({
        dirtyFileIds: ['agents'],
        currentProjectKey: 'p1',
        nextTab: 'instructions',
        nextAgent: 'opencode',
        nextScope: 'project',
        nextProjectKey: 'p1',
      }),
    ).toBe(false);
    expect(
      shouldGuardProjectInstructionContextChange({
        dirtyFileIds: ['agents'],
        currentProjectKey: 'p1',
        nextTab: 'instructions',
        nextAgent: 'claude',
        nextScope: 'project',
        nextProjectKey: 'p1',
      }),
    ).toBe(true);
  });

  test('leaving instructions or the project always guards dirty files', () => {
    expect(
      shouldGuardProjectInstructionContextChange({
        dirtyFileIds: ['agents'],
        currentProjectKey: 'p1',
        nextTab: 'skill',
        nextAgent: 'codex',
        nextScope: 'project',
        nextProjectKey: 'p1',
      }),
    ).toBe(true);
    expect(
      shouldGuardProjectInstructionContextChange({
        dirtyFileIds: ['claude'],
        currentProjectKey: 'p1',
        nextTab: 'instructions',
        nextAgent: 'claude',
        nextScope: 'user',
        nextProjectKey: null,
      }),
    ).toBe(true);
  });

  test('matches instruction file names case-insensitively', () => {
    expect(matchProjectInstructionNodeName(['README.md', 'Claude.md'], 'CLAUDE.md')).toBe(
      'Claude.md',
    );
    expect(matchProjectInstructionNodeName(['AGENTS.md'], 'AGENTS.md')).toBe('AGENTS.md');
    expect(matchProjectInstructionNodeName(['README.md'], 'AGENTS.md')).toBeNull();
  });
});
