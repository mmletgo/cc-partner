/**
 * 项目级原生提示词文件控制器测试。
 *
 * Business Logic: 共用 AGENTS.md 必须共用草稿；缺失文件可创建；切项目丢弃旧缓存。
 * Code Logic: mock workbenchApi.files；renderHook 覆盖 load/edit/save/agent 切换。
 */

// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import type { TFunction } from 'i18next';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { WorkbenchFileNode, WorkbenchOpenFile } from '@/lib/types/workbench';
import { DEFAULT_AGENT_HUB_CONTEXT } from '../context/agentHubContext';

const filesApi = vi.hoisted(() => ({
  listDir: vi.fn(),
  open: vi.fn(),
  createFile: vi.fn(),
  saveText: vi.fn(),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: { files: filesApi },
}));

import { useProjectInstructionFilesController } from './useProjectInstructionFilesController';

const t = ((key: string) => key) as TFunction<['agentHub', 'common']>;

afterEach(() => {
  vi.clearAllMocks();
});

function fileNode(name: string): WorkbenchFileNode {
  return {
    name,
    path: name,
    kind: 'file',
    size: 12,
    modifiedAt: null,
  };
}

function opened(path: string, content: string, hash = `hash-${path}`): WorkbenchOpenFile {
  return {
    metadata: {
      name: path,
      path,
      kind: 'file',
      size: content.length,
      modifiedAt: null,
    },
    detectedType: 'markdown',
    capabilities: {
      canPreview: true,
      canEdit: true,
      canFormat: false,
      mustValidateBeforeSave: false,
      defaultMode: 'source',
      availableModes: ['source'],
    },
    text: { content, baseHash: hash, baseModifiedAt: null },
    image: null,
    csv: null,
    sqlite: null,
    truncated: false,
    notice: null,
  };
}

describe('useProjectInstructionFilesController', () => {
  test('Codex and OpenCode share one AGENTS.md draft', async () => {
    filesApi.listDir.mockResolvedValue([fileNode('AGENTS.md')]);
    filesApi.open.mockResolvedValue(opened('AGENTS.md', 'shared body'));

    const { result, rerender } = renderHook(
      (agent: 'codex' | 'opencode') =>
        useProjectInstructionFilesController({
          projectKey: 'proj-1',
          agent,
          enabled: true,
          active: true,
          t,
        }),
      { initialProps: 'codex' as 'codex' | 'opencode' },
    );

    await waitFor(() => {
      expect(result.current.activeFile?.draft).toBe('shared body');
    });
    expect(filesApi.open).toHaveBeenCalledTimes(1);

    act(() => {
      result.current.editActiveFile('edited shared');
    });

    rerender('opencode');
    await waitFor(() => {
      expect(result.current.activeFileId).toBe('agents');
      expect(result.current.activeFile?.draft).toBe('edited shared');
    });
    expect(filesApi.open).toHaveBeenCalledTimes(1);
    expect(
      result.current.shouldGuardContextChange({
        ...DEFAULT_AGENT_HUB_CONTEXT,
        scope: 'project',
        projectKey: 'proj-1',
        tab: 'instructions',
        agent: 'grok',
      }),
    ).toBe(false);
    expect(
      result.current.shouldGuardContextChange({
        ...DEFAULT_AGENT_HUB_CONTEXT,
        scope: 'project',
        projectKey: 'proj-1',
        tab: 'instructions',
        agent: 'claude',
      }),
    ).toBe(true);
  });

  test('saves a missing AGENTS.md by creating then writing', async () => {
    filesApi.listDir.mockResolvedValue([]);
    filesApi.createFile.mockResolvedValue({
      name: 'AGENTS.md',
      path: 'AGENTS.md',
      kind: 'file',
      size: 0,
      modifiedAt: null,
    });
    filesApi.open.mockResolvedValue(opened('AGENTS.md', '', 'empty-hash'));
    filesApi.saveText.mockResolvedValue({
      metadata: {
        name: 'AGENTS.md',
        path: 'AGENTS.md',
        kind: 'file',
        size: 4,
        modifiedAt: null,
      },
      baseHash: 'saved-hash',
      baseModifiedAt: null,
    });

    const { result } = renderHook(() =>
      useProjectInstructionFilesController({
        projectKey: 'proj-1',
        agent: 'codex',
        enabled: true,
        active: true,
        t,
      }),
    );

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
      expect(result.current.activeFile?.exists).toBe(false);
    });

    act(() => {
      result.current.editActiveFile('new');
    });

    let saved = false;
    await act(async () => {
      saved = await result.current.saveActiveFile();
    });
    expect(saved).toBe(true);
    expect(filesApi.createFile).toHaveBeenCalledWith('proj-1', '', 'AGENTS.md', null);
    expect(filesApi.saveText).toHaveBeenCalledWith(
      'proj-1',
      'AGENTS.md',
      'new',
      'empty-hash',
      null,
    );
    expect(result.current.dirty).toBe(false);
    expect(result.current.activeFile?.exists).toBe(true);
  });
});
