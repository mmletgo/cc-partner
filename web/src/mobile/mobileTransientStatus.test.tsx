// @vitest-environment jsdom
/**
 * 移动端短暂成功提示 hook。
 *
 * Business Logic（为什么需要这个测试）:
 *   提交成功等确认必须到时消失；value 变化要重置 timer，不能提前清掉新提示。
 *
 * Code Logic（这个测试做什么）:
 *   用最小 probe 组件驱动 useAutoDismissedStatus，fake timer 断言到期才清空。
 */

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import { useState, type ReactElement } from 'react';
import {
  MOBILE_TRANSIENT_STATUS_MS,
  useAutoDismissedStatus,
} from './mobileTransientStatus';

/**
 * Business Logic（为什么需要这个函数）:
 *   hook 测试需要一个最小宿主，避免把 timer 合同绑死在终端/Git 面板上。
 *
 * Code Logic（这个函数做什么）:
 *   挂载 useAutoDismissedStatus，把当前文案渲染出来。
 */
function Probe({ initial }: { initial: string | null }): ReactElement | null {
  const [value, setValue] = useState<string | null>(initial);
  useAutoDismissedStatus(value, setValue);
  return value ? <div>{value}</div> : null;
}

describe('useAutoDismissedStatus', () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  test('clears the value after the transient delay', () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    render(<Probe initial="提交成功" />);
    expect(screen.getByText('提交成功')).toBeTruthy();
    act(() => {
      vi.advanceTimersByTime(MOBILE_TRANSIENT_STATUS_MS - 1);
    });
    expect(screen.getByText('提交成功')).toBeTruthy();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText('提交成功')).toBeNull();
  });
});
