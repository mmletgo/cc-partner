/**
 * 项目级原生提示词文件编辑视图测试。
 *
 * Business Logic: 共用文件展示 shared-by；Grok 等可在 AGENTS.md / CLAUDE.md 之间切换。
 * Code Logic: 注入 labels + stub controller；扫描源文件禁止 @/api。
 */

// @vitest-environment jsdom

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { AgentTarget } from '@/lib/types/agentHub';
import { PROJECT_INSTRUCTION_FILES, filesForAgent } from './projectInstructionFiles';
import {
  ProjectInstructionFilesView,
  type ProjectInstructionFilesViewLabels,
} from './ProjectInstructionFilesView';
import type {
  ProjectInstructionFileState,
  UseProjectInstructionFilesControllerResult,
} from './useProjectInstructionFilesController';

const viewDir = dirname(fileURLToPath(import.meta.url));

afterEach(() => {
  cleanup();
});

const labels: ProjectInstructionFilesViewLabels = {
  title: 'Project instruction files',
  loading: 'Loading',
  retry: 'Retry',
  save: 'Save file',
  unsaved: 'Unsaved',
  missing: 'Missing file',
  editorAria: 'Editor',
  placeholder: 'Placeholder',
  pathLabel: 'File',
  filesAria: 'Files',
  truncated: 'Truncated',
  sharedBy: (agents) => `${agents} share this file`,
  exclusiveTo: (agent) => `Only ${agent} uses this file`,
  agentSeparator: ', ',
  agentName: (agent) => agent,
};

function fileState(
  id: 'agents' | 'claude' | 'gemini',
  overrides: Partial<ProjectInstructionFileState> = {},
): ProjectInstructionFileState {
  const spec = PROJECT_INSTRUCTION_FILES.find((file) => file.id === id)!;
  return {
    spec,
    diskPath: spec.path,
    exists: true,
    draft: 'body',
    savedContent: 'body',
    baseHash: 'hash',
    truncated: false,
    notice: null,
    dirty: false,
    ...overrides,
  };
}

function stubController(
  agent: AgentTarget,
  overrides: Partial<UseProjectInstructionFilesControllerResult> = {},
): UseProjectInstructionFilesControllerResult {
  const files = filesForAgent(agent).map((spec) => fileState(spec.id));
  return {
    files,
    activeFile: files[0] ?? null,
    activeFileId: files[0]?.spec.id ?? null,
    loading: false,
    refreshing: false,
    error: null,
    actionError: null,
    actionBusy: false,
    busyAction: null,
    dirty: false,
    selectFile: vi.fn(),
    editActiveFile: vi.fn(),
    saveActiveFile: vi.fn(async () => true),
    saveAllDirty: vi.fn(async () => true),
    refresh: vi.fn(async () => undefined),
    discardDraftForContextChange: vi.fn(),
    shouldGuardContextChange: vi.fn(() => false),
    ...overrides,
  };
}

describe('ProjectInstructionFilesView', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(viewDir, './ProjectInstructionFilesView.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('Codex shows the shared AGENTS.md editor without extra file tabs', () => {
    render(
      <ProjectInstructionFilesView
        labels={labels}
        controller={stubController('codex')}
        agent="codex"
      />,
    );
    expect(screen.getByTestId('project-instruction-files')).toBeTruthy();
    expect(screen.getByTestId('project-instruction-path').textContent).toBe('AGENTS.md');
    expect(screen.getByTestId('project-instruction-shared-by').textContent).toContain(
      'share this file',
    );
    expect(screen.queryByTestId('project-instruction-file-tabs')).toBeNull();
    expect(screen.getByTestId('project-instruction-editor')).toBeTruthy();
  });

  test('Grok can switch between AGENTS.md and CLAUDE.md', () => {
    const selectFile = vi.fn();
    render(
      <ProjectInstructionFilesView
        labels={labels}
        controller={stubController('grok', { selectFile })}
        agent="grok"
      />,
    );
    expect(screen.getByTestId('project-instruction-file-tab-agents')).toBeTruthy();
    expect(screen.getByTestId('project-instruction-file-tab-claude')).toBeTruthy();
    fireEvent.click(screen.getByTestId('project-instruction-file-tab-claude'));
    expect(selectFile).toHaveBeenCalledWith('claude');
  });

  test('Gemini exclusive file copy does not claim a shared AGENTS.md', () => {
    render(
      <ProjectInstructionFilesView
        labels={labels}
        controller={stubController('gemini')}
        agent="gemini"
      />,
    );
    expect(screen.getByTestId('project-instruction-path').textContent).toBe('GEMINI.md');
    expect(screen.getByTestId('project-instruction-shared-by').textContent).toBe(
      'Only gemini uses this file',
    );
  });
});
