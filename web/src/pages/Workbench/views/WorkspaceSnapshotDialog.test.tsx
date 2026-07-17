// @vitest-environment jsdom
/**
 * WorkspaceSnapshotDialog 行为测试。
 */
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import type { ReactElement, ReactNode } from 'react';
import i18n from '@/i18n';
import type { WorkspaceLayout } from '../workspaceLayout';

vi.mock('@/components/primitives/Dialog', () => ({
  Dialog: ({
    open,
    children,
  }: {
    open: boolean;
    children: ReactNode;
  }) => (open ? <div data-testid="dialog-mock">{children}</div> : null),
}));

vi.mock('@/components/primitives/Input', () => ({
  Input: (props: {
    value: string;
    onChange: (e: { target: { value: string } }) => void;
    'aria-label'?: string;
    placeholder?: string;
    className?: string;
  }) => (
    <input
      aria-label={props['aria-label']}
      placeholder={props.placeholder}
      value={props.value}
      onChange={(e) => props.onChange({ target: { value: e.target.value } })}
    />
  ),
}));

vi.mock('@/components/primitives/Button', () => ({
  Button: ({
    children,
    onClick,
    type = 'button',
  }: {
    children: ReactNode;
    onClick?: () => void;
    type?: 'button' | 'submit';
    variant?: string;
    size?: string;
    loading?: boolean;
  }) => (
    <button type={type} onClick={onClick}>
      {children}
    </button>
  ),
}));

import { WorkspaceSnapshotDialog } from './WorkspaceSnapshotDialog';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   组件依赖 react-i18next 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   用真实 i18n 包一层 provider。
 */
function renderWithI18n(ui: ReactElement) {
  return render(<I18nextProvider i18n={i18n}>{ui}</I18nextProvider>);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要最小可用的命名 snapshot 数据。
 *
 * Code Logic（这个函数做什么）:
 *   构造 WorkspaceLayout named 项。
 */
function snap(name: string, id = 'id1'): WorkspaceLayout {
  return {
    schemaVersion: 1,
    id,
    slotKey: `named:${id}`,
    kind: 'named',
    name,
    projectId: 'p1',
    activeWorktreeId: null,
    activeSessionId: null,
    workspaceView: 'terminal',
    inspectorTab: 'files',
    browserTargetUrl: null,
    revision: 1,
    createdAt: 't',
    updatedAt: 't',
  };
}

describe('WorkspaceSnapshotDialog', () => {
  it('saves current structure without a command editor', () => {
    const onSaveCurrent = vi.fn();
    renderWithI18n(
      <WorkspaceSnapshotDialog
        open
        onClose={() => undefined}
        snapshots={[snap('Morning')]}
        onSaveCurrent={onSaveCurrent}
        onApply={vi.fn()}
        onDelete={vi.fn()}
      />,
    );
    expect(screen.queryByRole('textbox', { name: /command/i })).toBeNull();
    fireEvent.change(screen.getByLabelText('快照名称'), {
      target: { value: 'Evening' },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存当前结构' }));
    expect(onSaveCurrent).toHaveBeenCalledWith('Evening');
  });

  it('applies and confirms delete', () => {
    const onApply = vi.fn();
    const onDelete = vi.fn();
    renderWithI18n(
      <WorkspaceSnapshotDialog
        open
        onClose={() => undefined}
        snapshots={[snap('Morning', 'n1')]}
        onSaveCurrent={vi.fn()}
        onApply={onApply}
        onDelete={onDelete}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: '应用' }));
    expect(onApply).toHaveBeenCalledWith('n1');
    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }));
    expect(onDelete).toHaveBeenCalledWith('n1');
  });

  it('shows empty state when there are no snapshots', () => {
    renderWithI18n(
      <WorkspaceSnapshotDialog
        open
        onClose={() => undefined}
        snapshots={[]}
        onSaveCurrent={vi.fn()}
        onApply={vi.fn()}
        onDelete={vi.fn()}
      />,
    );
    expect(screen.getByText('暂无命名快照')).toBeTruthy();
  });
});
