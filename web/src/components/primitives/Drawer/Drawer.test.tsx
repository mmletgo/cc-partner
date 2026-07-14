// @vitest-environment jsdom
/**
 * Drawer 原语行为测试
 *
 * Business Logic（为什么需要这些测试）:
 *   Drawer 复用 Dialog 的 modal 层合同，但侧滑方向与布局语义不同；
 *   必须确认共享可访问性合同与 side 变体不会退化。
 *
 * Code Logic（这些测试做什么）:
 *   渲染 Drawer，断言 portal/ARIA/焦点/Escape/backdrop 与 data-side 标记。
 */

import { useState, type ReactElement } from 'react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Drawer, type DrawerProps } from './Drawer';

/**
 * 断言元素拥有文档焦点
 */
function expectFocused(el: Element): void {
  expect(document.activeElement).toBe(el);
}

/**
 * 受控 Drawer 测试壳。
 */
function DrawerHost(
  props: Partial<DrawerProps> & { title?: string } = {},
): ReactElement {
  const {
    open: openProp,
    onClose,
    title = 'Drawer title',
    titleId = 'test-drawer-title',
    side,
    closeOnEscape,
    closeOnBackdrop,
    className,
    children,
  } = props;
  const [open, setOpen] = useState(openProp ?? false);
  const isControlled = openProp !== undefined;
  const visible = isControlled ? openProp : open;

  return (
    <>
      <div data-testid="app-content">
        <button type="button" data-testid="open-drawer" onClick={() => setOpen(true)}>
          Open
        </button>
      </div>
      <Drawer
        open={visible}
        titleId={titleId}
        side={side}
        closeOnEscape={closeOnEscape}
        closeOnBackdrop={closeOnBackdrop}
        className={className}
        onClose={() => {
          onClose?.();
          if (!isControlled) setOpen(false);
        }}
      >
        <h2 id={titleId}>{title}</h2>
        {children ?? (
          <>
            <button type="button" data-testid="drawer-close">
              Close
            </button>
            <button type="button" data-testid="drawer-nav">
              Nav
            </button>
          </>
        )}
      </Drawer>
    </>
  );
}

afterEach(() => {
  cleanup();
  document.body.style.overflow = '';
  for (const child of Array.from(document.body.children)) {
    child.removeAttribute('aria-hidden');
    if ('inert' in child) {
      (child as HTMLElement & { inert: boolean }).inert = false;
    }
  }
});

describe('Drawer', () => {
  test('portals to document.body with dialog semantics', () => {
    render(<DrawerHost open />);
    const drawer = screen.getByRole('dialog');
    expect(drawer.getAttribute('aria-modal')).toBe('true');
    expect(drawer.getAttribute('aria-labelledby')).toBe('test-drawer-title');
    let node: HTMLElement | null = drawer;
    while (node && node.parentElement !== document.body) {
      node = node.parentElement;
    }
    expect(node?.parentElement).toBe(document.body);
  });

  test('defaults side to right and marks data-side', () => {
    render(<DrawerHost open />);
    expect(screen.getByRole('dialog').getAttribute('data-side')).toBe('right');
  });

  test('supports side=left', () => {
    render(<DrawerHost open side="left" />);
    expect(screen.getByRole('dialog').getAttribute('data-side')).toBe('left');
  });

  test('focuses first focusable and traps Tab', async () => {
    const user = userEvent.setup();
    render(<DrawerHost />);
    await user.click(screen.getByTestId('open-drawer'));
    const closeBtn = screen.getByTestId('drawer-close');
    const navBtn = screen.getByTestId('drawer-nav');
    await waitFor(() => expectFocused(closeBtn));
    await user.tab();
    expectFocused(navBtn);
    await user.tab();
    expectFocused(closeBtn);
  });

  test('Escape closes by default', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DrawerHost open onClose={onClose} />);
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('backdrop click closes by default', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DrawerHost open onClose={onClose} />);
    const drawer = screen.getByRole('dialog');
    const backdrop = drawer.parentElement!.querySelector(
      '[data-drawer-backdrop]',
    ) as HTMLElement;
    expect(backdrop).toBeTruthy();
    await user.click(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('restores trigger focus on close', async () => {
    const user = userEvent.setup();
    render(<DrawerHost />);
    const trigger = screen.getByTestId('open-drawer');
    await user.click(trigger);
    await waitFor(() => expect(screen.getByRole('dialog')).toBeTruthy());
    await user.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expectFocused(trigger);
  });

  test('marks background inert while open', async () => {
    render(<DrawerHost open />);
    const drawer = screen.getByRole('dialog');
    let portalRoot: HTMLElement = drawer;
    while (portalRoot.parentElement && portalRoot.parentElement !== document.body) {
      portalRoot = portalRoot.parentElement;
    }
    await waitFor(() => {
      for (const child of Array.from(document.body.children)) {
        if (child === portalRoot) {
          expect(child.getAttribute('aria-hidden')).not.toBe('true');
        } else {
          expect(child.getAttribute('aria-hidden')).toBe('true');
        }
      }
    });
  });
});
