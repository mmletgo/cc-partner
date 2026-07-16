// @vitest-environment jsdom
/**
 * WorkspaceSnapshotDialog 行为测试。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { WorkspaceLayout } from '../workspaceLayout';

vi.mock('@/components/primitives/Dialog', () => ({
  Dialog: ({
    open,
    children,
  }: {
    open: boolean;
    children: React.ReactNode;
  }) => (open ? <div data-testid="dialog-mock">{children}</div> : null),
}));

vi.mock('@/components/primitives/Input', () => ({
  Input: (props: {
    value: string;
    onChange: (e: { target: { value: string } }) => void;
    'aria-label'?: string;
    placeholder?: string;
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
    children: React.ReactNode;
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

afterEach(() => {
  cleanup();
});

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
    render(
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
    render(
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
});
