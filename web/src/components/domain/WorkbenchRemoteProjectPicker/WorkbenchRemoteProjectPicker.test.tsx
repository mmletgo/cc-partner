/**
 * WorkbenchRemoteProjectPicker 本机打开接线测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   添加本机项目后必须立刻进入共享项目列表；选择器若直接调用
 *   workbenchApi.projects.add，侧栏只能等用户点刷新才能看到新项目。
 *
 * Code Logic（这个测试做什么）:
 *   mock 本机 fs roots/info，渲染 source=local 选择器；确认打开走注入的
 *   openLocalProject，而不是绕过共享上下文的 projects.add。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { WorkbenchProject } from '@/lib/types';

const fsRoots = vi.fn();
const fsListDir = vi.fn();
const fsInfo = vi.fn();
const projectsAdd = vi.fn();

vi.mock('@/api/workbench', async () => {
  const actual = await vi.importActual<typeof import('@/api/workbench')>('@/api/workbench');
  return {
    ...actual,
    workbenchApi: {
      ...actual.workbenchApi,
      fs: {
        ...actual.workbenchApi.fs,
        roots: (...args: unknown[]) => fsRoots(...args),
        listDir: (...args: unknown[]) => fsListDir(...args),
        info: (...args: unknown[]) => fsInfo(...args),
      },
      projects: {
        ...actual.workbenchApi.projects,
        add: (...args: unknown[]) => projectsAdd(...args),
      },
    },
  };
});

import { WorkbenchRemoteProjectPicker } from './WorkbenchRemoteProjectPicker';

const openedProject: WorkbenchProject = {
  id: 'proj-local-1',
  name: 'demo-app',
  kind: 'local',
  deviceId: 'local',
  deviceName: '本机',
  path: '/Users/demo/demo-app',
  lastOpenedAt: '2026-09-02T00:00:00.000Z',
  createdAt: '2026-09-02T00:00:00.000Z',
  updatedAt: '2026-09-02T00:00:00.000Z',
};

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   选择器打开前要先有可读目录，否则「打开项目」保持 disabled。
 *
 * Code Logic（这个函数做什么）:
 *   填入单一 home root 与可读 pathInfo。
 */
function mockReadableLocalHome(): void {
  fsRoots.mockResolvedValue([{ label: 'Home', path: '/Users/demo', kind: 'home' }]);
  fsListDir.mockResolvedValue([]);
  fsInfo.mockResolvedValue({
    name: 'demo',
    path: '/Users/demo',
    kind: 'dir',
    readable: true,
    isGitRepo: true,
    suggestedProjectName: 'demo',
  });
}

describe('WorkbenchRemoteProjectPicker local open', () => {
  test('opens local project via injected openLocalProject instead of projects.add', async () => {
    mockReadableLocalHome();
    const openLocalProject = vi.fn(async () => openedProject);
    const onProjectOpened = vi.fn();
    const user = userEvent.setup();

    render(
      <I18nextProvider i18n={i18n}>
        <WorkbenchRemoteProjectPicker
          source="local"
          openLocalProject={openLocalProject}
          onCancel={() => undefined}
          onProjectOpened={onProjectOpened}
        />
      </I18nextProvider>,
    );

    const openButton = await screen.findByRole('button', { name: '打开项目' });
    await waitFor(() => {
      expect(openButton.hasAttribute('disabled')).toBe(false);
    });

    await user.click(openButton);

    await waitFor(() => {
      expect(openLocalProject).toHaveBeenCalledWith('/Users/demo');
    });
    expect(projectsAdd).not.toHaveBeenCalled();
    expect(onProjectOpened).toHaveBeenCalledWith(openedProject);
  });
});
