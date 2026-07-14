/**
 * TransferItem 动作可见性契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   后端当前只支持 cancel；pause/resume/retry/open 不得以空回调占位渲染，
 *   否则用户会点击 no-op 按钮以为动作已执行。
 *
 * Code Logic（这个测试做什么）:
 *   用 I18nextProvider 渲染 TransferItem，按 status 与回调有无断言动作按钮存在性。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { TransferItem, type TransferItemTask } from './TransferItem';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法任务数据，避免样板重复。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 TransferItemTask。
 */
function buildTask(overrides: Partial<TransferItemTask> = {}): TransferItemTask {
  return {
    id: 'task-1',
    fileName: 'report.txt',
    fileSize: 1024,
    direction: 'send',
    status: 'transferring',
    progress: 0.4,
    peerDevice: 'MacBook',
    speed: 1024,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需挂载 i18n，避免 aria-label 以 key 原样泄漏。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 TransferItem 并透传 props。
 */
function renderItem(
  props: Partial<React.ComponentProps<typeof TransferItem>> & {
    task?: TransferItemTask;
  } = {},
) {
  return render(
    <I18nextProvider i18n={i18n}>
      <TransferItem task={props.task ?? buildTask()} {...props} />
    </I18nextProvider>,
  );
}

describe('TransferItem action guards', () => {
  test('transferring without callbacks renders no pause/cancel buttons', () => {
    renderItem({ task: buildTask({ status: 'transferring' }) });

    expect(screen.queryByRole('button', { name: '暂停' })).toBeNull();
    expect(screen.queryByRole('button', { name: '取消' })).toBeNull();
  });

  test('transferring with only onCancel renders cancel and not pause', () => {
    const onCancel = vi.fn();
    renderItem({
      task: buildTask({ status: 'transferring' }),
      onCancel,
    });

    expect(screen.queryByRole('button', { name: '暂停' })).toBeNull();
    const cancel = screen.getByRole('button', { name: '取消' });
    fireEvent.click(cancel);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  test('pending with onCancel renders cancel only', () => {
    renderItem({
      task: buildTask({ status: 'pending', progress: 0, speed: undefined }),
      onCancel: vi.fn(),
    });

    expect(screen.getByRole('button', { name: '取消' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: '暂停' })).toBeNull();
    expect(screen.queryByRole('button', { name: '重试' })).toBeNull();
  });

  test('failed without onRetry renders no retry button', () => {
    renderItem({
      task: buildTask({ status: 'failed', progress: 0.2, errorMessage: 'boom' }),
    });

    expect(screen.queryByRole('button', { name: '重试' })).toBeNull();
  });

  test('failed with onRetry renders and invokes retry', () => {
    const onRetry = vi.fn();
    renderItem({
      task: buildTask({ status: 'failed', progress: 0.2 }),
      onRetry,
    });

    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  test('completed without onOpen renders no open button', () => {
    renderItem({
      task: buildTask({ status: 'completed', progress: 1, speed: undefined }),
    });

    expect(screen.queryByRole('button', { name: '打开' })).toBeNull();
  });

  test('completed with onOpen renders open button', () => {
    const onOpen = vi.fn();
    renderItem({
      task: buildTask({ status: 'completed', progress: 1, speed: undefined }),
      onOpen,
    });

    fireEvent.click(screen.getByRole('button', { name: '打开' }));
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  test('cancelled without onResume renders no continue button', () => {
    renderItem({
      task: buildTask({ status: 'cancelled', progress: 0.1, speed: undefined }),
    });

    expect(screen.queryByRole('button', { name: '继续' })).toBeNull();
  });
});
