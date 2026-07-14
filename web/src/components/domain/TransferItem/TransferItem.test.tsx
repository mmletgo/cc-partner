/**
 * TransferItem 动作可见性契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   每个 phase/status 只允许渲染合法动作；无回调不得占位；对账中不得重复动作。
 *
 * Code Logic（这个测试做什么）:
 *   用 I18nextProvider 渲染 TransferItem，按 fixture 与回调有无断言动作按钮存在性。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { TransferItem, type TransferItemProps, type TransferItemTask } from './TransferItem';

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
  props: Partial<TransferItemProps> & {
    task?: TransferItemTask;
  } = {},
) {
  return render(
    <I18nextProvider i18n={i18n}>
      <TransferItem task={props.task ?? buildTask()} {...props} />
    </I18nextProvider>,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   action matrix 用例需要按 fixture 名称组装 task + 合法回调。
 *
 * Code Logic（这个函数做什么）:
 *   返回对应 fixture 的 task 与 callback props。
 */
function buildFixture(
  fixture: string,
): {
  task: TransferItemTask;
  props: Partial<TransferItemProps>;
} {
  switch (fixture) {
    case 'transferring':
      return {
        task: buildTask({ status: 'transferring', progress: 0.4 }),
        props: { onCancel: vi.fn() },
      };
    case 'failed-resumable':
      return {
        task: buildTask({
          status: 'failed',
          progress: 0.5,
          errorMessage: 'network drop',
          phase: 'failed',
        }),
        props: { onResume: vi.fn() },
      };
    case 'failed-retryable':
      return {
        task: buildTask({
          status: 'failed',
          progress: 0,
          errorMessage: 'connect failed',
          phase: 'failed',
        }),
        props: { onRetry: vi.fn() },
      };
    case 'completed-received':
      return {
        task: buildTask({
          status: 'completed',
          direction: 'receive',
          progress: 1,
          speed: undefined,
          phase: 'completed',
        }),
        props: { onOpen: vi.fn(), onReveal: vi.fn() },
      };
    default:
      throw new Error(`unknown fixture: ${fixture}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   断言当前文档中出现的动作按钮集合与期望一致。
 *
 * Code Logic（这个函数做什么）:
 *   对期望动作 getByRole；并确认其它 recovery 动作不存在。
 */
function expectVisibleActions(actions: string[]): void {
  const allKnown = ['取消', '继续传输', '重新传输', '打开', '在文件夹中显示', '暂停', '重试'];
  for (const name of allKnown) {
    const nodes = screen.queryAllByRole('button', { name });
    if (actions.includes(name)) {
      expect(nodes.length).toBeGreaterThan(0);
    } else {
      expect(nodes.length).toBe(0);
    }
  }
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
    expect(screen.queryByRole('button', { name: '重新传输' })).toBeNull();
  });

  test('failed without onRetry renders no retry button', () => {
    renderItem({
      task: buildTask({ status: 'failed', progress: 0.2, errorMessage: 'boom' }),
    });

    expect(screen.queryByRole('button', { name: '重新传输' })).toBeNull();
    expect(screen.queryByRole('button', { name: '继续传输' })).toBeNull();
  });

  test('failed with onRetry renders and invokes retry', () => {
    const onRetry = vi.fn();
    renderItem({
      task: buildTask({ status: 'failed', progress: 0.2 }),
      onRetry,
    });

    fireEvent.click(screen.getByRole('button', { name: '重新传输' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  test('failed with onResume renders continue transfer', () => {
    const onResume = vi.fn();
    renderItem({
      task: buildTask({ status: 'failed', progress: 0.6 }),
      onResume,
    });

    fireEvent.click(screen.getByRole('button', { name: '继续传输' }));
    expect(onResume).toHaveBeenCalledTimes(1);
  });

  test('completed without onOpen renders no open button', () => {
    renderItem({
      task: buildTask({ status: 'completed', progress: 1, speed: undefined }),
    });

    expect(screen.queryByRole('button', { name: '打开' })).toBeNull();
    expect(screen.queryByRole('button', { name: '在文件夹中显示' })).toBeNull();
  });

  test('completed with onOpen and onReveal renders both', () => {
    const onOpen = vi.fn();
    const onReveal = vi.fn();
    renderItem({
      task: buildTask({
        status: 'completed',
        direction: 'receive',
        progress: 1,
        speed: undefined,
      }),
      onOpen,
      onReveal,
    });

    fireEvent.click(screen.getByRole('button', { name: '打开' }));
    fireEvent.click(screen.getByRole('button', { name: '在文件夹中显示' }));
    expect(onOpen).toHaveBeenCalledTimes(1);
    expect(onReveal).toHaveBeenCalledTimes(1);
  });

  test('reconciling suppresses recovery actions and shows confirming copy', () => {
    renderItem({
      task: buildTask({
        status: 'failed',
        progress: 0.3,
        reconciling: true,
        errorMessage: 'timeout',
      }),
      onRetry: vi.fn(),
      onResume: vi.fn(),
      onCancel: vi.fn(),
    });

    expect(screen.getAllByText('正在确认结果').length).toBeGreaterThan(0);
    expect(screen.queryByRole('button', { name: '重新传输' })).toBeNull();
    expect(screen.queryByRole('button', { name: '继续传输' })).toBeNull();
    expect(screen.queryByRole('button', { name: '取消' })).toBeNull();
  });

  test.each([
    ['transferring', ['取消']],
    ['failed-resumable', ['继续传输']],
    ['failed-retryable', ['重新传输']],
    ['completed-received', ['打开', '在文件夹中显示']],
  ] as const)('%s renders only legal actions', (fixture, actions) => {
    const { task, props } = buildFixture(fixture);
    renderItem({ task, ...props });
    expectVisibleActions([...actions]);
  });
});
