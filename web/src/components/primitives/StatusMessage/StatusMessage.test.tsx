// @vitest-environment jsdom
/**
 * StatusMessage 原语可访问性与 live-region 合同测试
 *
 * Business Logic（为什么需要这些测试）:
 *   异步成功/失败反馈必须有正确 live region，阻断失败只播报一次 role=alert，
 *   成功用 role=status，避免各页自建不一致导致读屏漏报或重复播报。
 *
 * Code Logic（这些测试做什么）:
 *   用 Testing Library 渲染 StatusMessage，断言 tone→role/live 映射、action 插槽与 className。
 */

import { afterEach, describe, expect, test } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { StatusMessage } from './StatusMessage';

afterEach(() => {
  cleanup();
});

describe('StatusMessage', () => {
  test('success tone 使用 role=status 与 polite live region', () => {
    render(<StatusMessage tone="success">Saved</StatusMessage>);
    const node = screen.getByRole('status');
    expect(node.textContent).toContain('Saved');
    expect(node.getAttribute('aria-live')).toBe('polite');
    expect(node.getAttribute('data-tone')).toBe('success');
  });

  test('danger tone 使用 role=alert 且恰好一个 alert', () => {
    render(<StatusMessage tone="danger">Commit failed</StatusMessage>);
    const alerts = screen.getAllByRole('alert');
    expect(alerts).toHaveLength(1);
    expect(alerts[0].textContent).toContain('Commit failed');
    expect(alerts[0].getAttribute('aria-live')).toBe('assertive');
    expect(alerts[0].getAttribute('data-tone')).toBe('danger');
  });

  test('info/warn tone 使用 role=status', () => {
    const { rerender } = render(<StatusMessage tone="info">Reconciling</StatusMessage>);
    expect(screen.getByRole('status').getAttribute('data-tone')).toBe('info');

    rerender(<StatusMessage tone="warn">Stale cache</StatusMessage>);
    expect(screen.getByRole('status').getAttribute('data-tone')).toBe('warn');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  test('live=off 时关闭自动播报', () => {
    render(
      <StatusMessage tone="danger" live="off">
        Silent danger
      </StatusMessage>,
    );
    const node = screen.getByRole('alert');
    expect(node.getAttribute('aria-live')).toBe('off');
  });

  test('action 插槽渲染在消息旁', () => {
    render(
      <StatusMessage
        tone="danger"
        action={<button type="button">Retry</button>}
      >
        Failed
      </StatusMessage>,
    );
    expect(screen.getByRole('alert').textContent).toContain('Failed');
    expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
  });

  test('className 透传到根节点', () => {
    render(
      <StatusMessage tone="success" className="extra-class">
        Ok
      </StatusMessage>,
    );
    expect(screen.getByRole('status').className).toContain('extra-class');
  });
});
