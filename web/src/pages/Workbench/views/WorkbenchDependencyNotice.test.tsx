// @vitest-environment jsdom
/**
 * WorkbenchDependencyNotice：tmux 就绪不得占用终端工作台。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import type { WorkbenchDependencyStatus, WorkbenchProject } from '@/lib/types';

const checkRemote = vi.fn();

vi.mock('@/api/workbenchDependency', () => ({
  workbenchDependencyApi: {
    check: (...args: unknown[]) => checkRemote(...args),
    install: vi.fn(),
    cancel: vi.fn(),
    status: vi.fn(),
  },
}));

vi.mock('@/components/domain/WorkbenchDependencyCard', () => ({
  WorkbenchDependencyCard: () => <div data-testid="workbench-dependency-card">dependency</div>,
}));

import { WorkbenchDependencyNotice } from './WorkbenchDependencyNotice';

/**
 * Business Logic（为什么需要这个函数）:
 *   本机项目是就绪卡误入终端区的主路径。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小 local WorkbenchProject。
 */
function localProject(): WorkbenchProject {
  return {
    id: 'local-1',
    name: 'demo',
    kind: 'local',
    deviceId: 'self',
    deviceName: 'Mac',
    path: '/Users/demo/project',
    lastOpenedAt: '2026-08-17T00:00:00.000Z',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-08-17T00:00:00.000Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目必须用对端 tmux 状态，不能误用本机 ready。
 *
 * Code Logic（这个函数做什么）:
 *   在 local fixture 上改 kind/deviceId。
 */
function remoteProject(): WorkbenchProject {
  return {
    ...localProject(),
    id: 'remote:device-a:abc',
    kind: 'remote',
    deviceId: 'device-a',
    deviceName: 'Studio Mac',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端 check 返回需要对齐 DTO，避免测试里拼不完整状态。
 *
 * Code Logic（这个函数做什么）:
 *   默认 ready，再用 patch 覆盖。
 */
function remoteStatus(patch: Partial<WorkbenchDependencyStatus> = {}): WorkbenchDependencyStatus {
  return {
    status: 'ready',
    available: true,
    version: '3.6a',
    backend: 'native',
    path: '/opt/homebrew/bin/tmux',
    installable: false,
    installCommandPreview: [],
    error: null,
    output: [],
    statusChangedAt: '2026-08-17T00:00:00.000Z',
    ...patch,
  };
}

afterEach(() => {
  cleanup();
  checkRemote.mockReset();
});

describe('WorkbenchDependencyNotice', () => {
  it('hides the local tmux card when the dependency is already ready', () => {
    const { container } = render(
      <WorkbenchDependencyNotice project={localProject()} localStatus="ready" />,
    );
    expect(screen.queryByTestId('workbench-dependency-card')).toBeNull();
    expect(container.firstChild).toBeNull();
  });

  it('shows the local tmux card when the dependency is missing', () => {
    render(<WorkbenchDependencyNotice project={localProject()} localStatus="missing" />);
    expect(screen.getByTestId('workbench-dependency-card')).toBeTruthy();
  });

  it('hides the remote tmux card after the peer reports ready', async () => {
    checkRemote.mockResolvedValue(remoteStatus());
    const { container } = render(
      <WorkbenchDependencyNotice project={remoteProject()} localStatus="ready" />,
    );
    expect(screen.queryByTestId('workbench-dependency-card')).toBeNull();
    await waitFor(() => {
      expect(checkRemote).toHaveBeenCalledWith('device-a');
    });
    expect(screen.queryByTestId('workbench-dependency-card')).toBeNull();
    expect(container.firstChild).toBeNull();
  });

  it('shows the remote tmux card when the peer is missing tmux', async () => {
    checkRemote.mockResolvedValue(
      remoteStatus({ status: 'missing', available: false, version: null, path: null }),
    );
    render(<WorkbenchDependencyNotice project={remoteProject()} localStatus="ready" />);
    expect(await screen.findByTestId('workbench-dependency-card')).toBeTruthy();
  });
});
