// @vitest-environment jsdom
/**
 * AI 辅助改写 Dialog 测试。
 *
 * Business Logic: 空方向不能提交；busy 时确认按钮 loading 且取消禁用。
 * Code Logic: jsdom 渲染纯 props。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import { AiReviseInstructionDialog } from './AiReviseInstructionDialog';

afterEach(() => {
  cleanup();
});

describe('AiReviseInstructionDialog', () => {
  test('disables confirm when direction is blank', () => {
    const onConfirm = vi.fn();
    render(
      <AiReviseInstructionDialog
        open
        title="AI revise"
        description="Will revise the common slot."
        directionLabel="Direction"
        directionPlaceholder="Type here"
        confirmLabel="Revise and save"
        cancelLabel="Cancel"
        direction="   "
        error={null}
        busy={false}
        onDirectionChange={vi.fn()}
        onCancel={vi.fn()}
        onConfirm={onConfirm}
      />,
    );
    const confirm = screen.getByTestId('instruction-ai-revise-confirm') as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.click(confirm);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  test('calls onConfirm when direction is present', () => {
    const onConfirm = vi.fn();
    render(
      <AiReviseInstructionDialog
        open
        title="AI revise"
        description="Will revise the common slot."
        directionLabel="Direction"
        directionPlaceholder="Type here"
        confirmLabel="Revise and save"
        cancelLabel="Cancel"
        direction="make it shorter"
        error={null}
        busy={false}
        onDirectionChange={vi.fn()}
        onCancel={vi.fn()}
        onConfirm={onConfirm}
      />,
    );
    fireEvent.click(screen.getByTestId('instruction-ai-revise-confirm'));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  test('busy disables cancel and keeps the dialog mounted', () => {
    render(
      <AiReviseInstructionDialog
        open
        title="AI revise"
        description="Will revise the common slot."
        directionLabel="Direction"
        directionPlaceholder="Type here"
        confirmLabel="Revise and save"
        cancelLabel="Cancel"
        direction="make it shorter"
        error={null}
        busy
        onDirectionChange={vi.fn()}
        onCancel={vi.fn()}
        onConfirm={vi.fn()}
      />,
    );
    expect(
      (screen.getByTestId('instruction-ai-revise-cancel') as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(screen.getByTestId('instruction-ai-revise-dialog')).toBeTruthy();
  });
});
