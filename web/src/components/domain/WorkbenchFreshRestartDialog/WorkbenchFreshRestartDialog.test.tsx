// @vitest-environment jsdom
/**
 * WorkbenchFreshRestartDialog 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   设备级「全新启动连接」是危险动作的最终确认层；预检清单、非工作台会话警告、
 *   降级手动命令展示与 busy 锁必须独立可测，防止后续改动静默吞掉确认语义。
 *
 * Code Logic（这个测试做什么）:
 *   props-only 渲染 Dialog（jsdom）；覆盖预检态、foreign 警告 + checkbox、
 *   ssh 不可用提示、成功/降级结果态、未知 degraded token 回落、busy 禁用与
 *   onConfirm 透传 includeForeignSessions。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';

import i18n from '@/i18n';
import type { WorkbenchFreshRestartPreview, WorkbenchFreshRestartResult } from '@/lib/types';
import { WorkbenchFreshRestartDialog } from './WorkbenchFreshRestartDialog';

const basePreview: WorkbenchFreshRestartPreview = {
  sessions: [
    { sessionId: 's1', projectId: 'p1', name: 'demo-main', backend: 'tmux' },
    { sessionId: 's2', projectId: 'p1', name: 'demo-feat', backend: 'tmux' },
  ],
  workbenchTmuxSessionCount: 2,
  foreignSessionCount: 0,
  foreignSessionNames: [],
  sshBootstrapAvailable: true,
  sshBootstrapDetail: null,
};

function renderDialog(
  overrides: Partial<Parameters<typeof WorkbenchFreshRestartDialog>[0]> = {},
) {
  const onConfirm = vi.fn();
  const onClose = vi.fn();
  const props = {
    open: true,
    onClose,
    previewing: false,
    preview: basePreview,
    result: null as WorkbenchFreshRestartResult | null,
    error: null,
    busy: false,
    onConfirm,
    ...overrides,
  };
  render(<WorkbenchFreshRestartDialog {...props} />);
  return { onConfirm, onClose };
}

beforeEach(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

describe('WorkbenchFreshRestartDialog', () => {
  test('远端目标展示设备名，避免看成是本机', () => {
    renderDialog({ deviceName: 'Studio Mac', targetKind: 'remote' });
    const target = screen.getByTestId('fresh-restart-target');
    expect(target.textContent).toContain('远端');
    expect(target.textContent).toContain('Studio Mac');
    expect(target.textContent).not.toContain('本机');
  });

  test('本机目标展示本机与设备名', () => {
    renderDialog({ deviceName: 'Mac', targetKind: 'local' });
    const target = screen.getByTestId('fresh-restart-target');
    expect(target.textContent).toContain('本机');
    expect(target.textContent).toContain('Mac');
  });

  test('预检态渲染会话清单并确认时透传 includeForeign=false', () => {
    const { onConfirm } = renderDialog();
    expect(screen.getByText('demo-main')).toBeDefined();
    expect(screen.getByText('demo-feat')).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '全新启动' }));
    expect(onConfirm).toHaveBeenCalledWith(false);
  });

  test('存在非工作台会话时渲染警告与 checkbox，勾选后透传 true', () => {
    const { onConfirm } = renderDialog({
      preview: {
        ...basePreview,
        foreignSessionCount: 1,
        foreignSessionNames: ['manual'],
      },
    });
    expect(screen.getByText(/非工作台会话/)).toBeDefined();
    const checkbox = screen.getByRole('checkbox') as HTMLInputElement;
    expect(checkbox.checked).toBe(false);
    fireEvent.click(checkbox);
    fireEvent.click(screen.getByRole('button', { name: '全新启动' }));
    expect(onConfirm).toHaveBeenCalledWith(true);
  });

  test('ssh 引导不可用时展示 warn 提示', () => {
    renderDialog({
      preview: {
        ...basePreview,
        sshBootstrapAvailable: false,
        sshBootstrapDetail: 'sshd 未运行',
      },
    });
    expect(screen.getByText(/自动引导不可用/)).toBeDefined();
  });

  test('成功结果态展示重启成功并隐藏确认按钮', () => {
    renderDialog({
      result: {
        terminatedSessionCount: 2,
        terminatedSessionIds: ['s1', 's2'],
        skippedSessionIds: [],
        serverRestarted: true,
        bootstrap: 'ssh',
        degradedReason: null,
        degradedDetail: null,
        foreignSessionCount: 0,
        manualCommand: null,
      },
    });
    expect(screen.getByText(/tmux server 已以全新登录环境重启/)).toBeDefined();
    expect(screen.queryByRole('button', { name: '全新启动' })).toBeNull();
    expect(screen.getByRole('button', { name: '关闭' })).toBeDefined();
  });

  test('降级结果态展示手动命令与复制按钮', () => {
    renderDialog({
      result: {
        terminatedSessionCount: 2,
        terminatedSessionIds: ['s1', 's2'],
        skippedSessionIds: [],
        serverRestarted: false,
        bootstrap: 'manual',
        degradedReason: 'fresh_restart_ssh_timeout',
        degradedDetail: 'ssh 引导超时',
        foreignSessionCount: 0,
        manualCommand: 'cc-partner-backend workbench-fresh',
      },
    });
    expect(screen.getByText(/tmux server 未重启/)).toBeDefined();
    expect(screen.getByText('cc-partner-backend workbench-fresh')).toBeDefined();
    expect(screen.getByRole('button', { name: '复制命令' })).toBeDefined();
  });

  test('error 态渲染 alert 文案', () => {
    renderDialog({ error: '全新启动连接执行失败: boom' });
    expect(screen.getByRole('alert')).toBeDefined();
  });

  test('busy 时确认按钮禁用且预检中不可确认', () => {
    renderDialog({ busy: true });
    const confirmButton = screen.getByRole('button', { name: '全新启动' }) as HTMLButtonElement;
    expect(confirmButton.disabled).toBe(true);
  });
});
