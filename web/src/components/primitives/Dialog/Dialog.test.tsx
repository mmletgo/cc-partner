// @vitest-environment jsdom
/**
 * Dialog 原语行为测试
 *
 * Business Logic（为什么需要这些测试）:
 *   Dialog 是无业务语义的可访问模态合同，业务页迁移后不得再各自实现 focus trap；
 *   必须验证 portal、ARIA、焦点、Escape、backdrop 策略与关闭恢复。
 *
 * Code Logic（这些测试做什么）:
 *   用 Testing Library + user-event 渲染 Dialog，断言 DOM 位置、角色属性与键盘/点击行为。
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { useRef, useState, type ReactElement, type RefObject } from 'react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Dialog, type DialogProps } from './Dialog';

/**
 * 断言元素拥有文档焦点
 */
function expectFocused(el: Element): void {
  expect(document.activeElement).toBe(el);
}

/**
 * 受控 Dialog 测试壳：带触发按钮与标题。
 */
function DialogHost(
  props: Partial<DialogProps> & { title?: string } = {},
): ReactElement {
  const {
    open: openProp,
    onClose,
    title = 'Dialog title',
    titleId = 'test-dialog-title',
    closeOnEscape,
    closeOnBackdrop,
    className,
    backdropVariant,
    initialFocusRef,
    children,
  } = props;
  const [open, setOpen] = useState(openProp ?? false);
  const isControlled = openProp !== undefined;
  const visible = isControlled ? openProp : open;

  return (
    <>
      <div data-testid="app-content">
        <button
          type="button"
          data-testid="open-dialog"
          onClick={() => setOpen(true)}
        >
          Open
        </button>
      </div>
      <Dialog
        open={visible}
        titleId={titleId}
        closeOnEscape={closeOnEscape}
        closeOnBackdrop={closeOnBackdrop}
        className={className}
        backdropVariant={backdropVariant}
        initialFocusRef={initialFocusRef}
        onClose={() => {
          onClose?.();
          if (!isControlled) setOpen(false);
        }}
      >
        <h2 id={titleId}>{title}</h2>
        {children ?? (
          <>
            <button type="button" data-testid="dialog-action-a">
              Action A
            </button>
            <button type="button" data-testid="dialog-action-b">
              Action B
            </button>
          </>
        )}
      </Dialog>
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

describe('Dialog', () => {
  test('does not render portal content when closed', () => {
    render(<DialogHost open={false} />);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  test('portals surface into document.body', () => {
    render(<DialogHost open />);
    const dialog = screen.getByRole('dialog');
    expect(dialog).toBeTruthy();
    // 从 surface 向上应能到达 body 直接子节点
    let node: HTMLElement | null = dialog;
    while (node && node.parentElement !== document.body) {
      node = node.parentElement;
    }
    expect(node?.parentElement).toBe(document.body);
    expect(document.body.contains(dialog)).toBe(true);
  });

  test('surface has role=dialog, aria-modal and aria-labelledby', () => {
    render(<DialogHost open titleId="dlg-title" title="Hello" />);
    const dialog = screen.getByRole('dialog');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(dialog.getAttribute('aria-labelledby')).toBe('dlg-title');
    expect(screen.getByText('Hello').id).toBe('dlg-title');
  });

  test('backdrop does not carry dialog role', () => {
    render(<DialogHost open />);
    const dialogs = screen.getAllByRole('dialog');
    expect(dialogs).toHaveLength(1);
  });

  test('focuses first focusable on open and traps Tab', async () => {
    const user = userEvent.setup();
    render(<DialogHost />);
    await user.click(screen.getByTestId('open-dialog'));
    const a = screen.getByTestId('dialog-action-a');
    const b = screen.getByTestId('dialog-action-b');
    await waitFor(() => expectFocused(a));

    await user.tab();
    expectFocused(b);
    await user.tab();
    expectFocused(a);
    await user.tab({ shift: true });
    expectFocused(b);
  });

  test('honors initialFocusRef', async () => {
    function WithInitial(): ReactElement {
      const ref = useRef<HTMLButtonElement | null>(null);
      return (
        <DialogHost open initialFocusRef={ref as RefObject<HTMLElement | null>}>
          <button type="button" data-testid="first">
            First
          </button>
          <button type="button" ref={ref} data-testid="preferred">
            Preferred
          </button>
        </DialogHost>
      );
    }
    render(<WithInitial />);
    await waitFor(() => {
      expectFocused(screen.getByTestId('preferred'));
    });
  });

  test('Escape closes when closeOnEscape is true (default)', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DialogHost open onClose={onClose} />);
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('Escape does not close when closeOnEscape is false', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DialogHost open closeOnEscape={false} onClose={onClose} />);
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
  });

  test('default backdrop keeps frost blur; scrim variant is translucent without blur', () => {
    const { rerender } = render(<DialogHost open />);
    const defaultBackdrop = screen
      .getByRole('dialog')
      .parentElement!.querySelector('[data-dialog-backdrop]') as HTMLElement;
    expect(defaultBackdrop.getAttribute('data-backdrop-variant')).toBe('frost');

    rerender(<DialogHost open backdropVariant="scrim" />);
    const scrimBackdrop = screen
      .getByRole('dialog')
      .parentElement!.querySelector('[data-dialog-backdrop]') as HTMLElement;
    expect(scrimBackdrop.getAttribute('data-backdrop-variant')).toBe('scrim');

    const css = readFileSync(
      join(process.cwd(), 'src/components/primitives/Dialog/Dialog.module.css'),
      'utf8',
    );
    expect(css).toMatch(/\.backdropScrim\s*\{[^}]*backdrop-filter:\s*none/);
    expect(css).toMatch(/\.backdropScrim\s*\{[^}]*var\(--overlay-scrim\)/);
    expect(css).not.toMatch(/\.backdropScrim\s*\{[^}]*blur\(/);
  });

  test('backdrop click closes when closeOnBackdrop is true (default)', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DialogHost open onClose={onClose} />);
    const dialog = screen.getByRole('dialog');
    const root = dialog.parentElement;
    expect(root).toBeTruthy();
    // backdrop 是 root 内、surface 外的兄弟节点
    const backdrop = root!.querySelector('[data-dialog-backdrop]') as HTMLElement | null;
    expect(backdrop).toBeTruthy();
    await user.click(backdrop!);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('backdrop click ignored when closeOnBackdrop is false', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DialogHost open closeOnBackdrop={false} onClose={onClose} />);
    const dialog = screen.getByRole('dialog');
    const backdrop = dialog.parentElement!.querySelector(
      '[data-dialog-backdrop]',
    ) as HTMLElement;
    await user.click(backdrop);
    expect(onClose).not.toHaveBeenCalled();
  });

  test('clicking surface does not close', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DialogHost open onClose={onClose} />);
    await user.click(screen.getByRole('dialog'));
    expect(onClose).not.toHaveBeenCalled();
  });

  test('ghost click after pointer-open does not close (same-gesture synthetic click)', async () => {
    // 复现移动端 FAB：pointerdown 打开 sheet 后，同一次手势的合成 click 落到刚挂载的 backdrop。
    const onClose = vi.fn();
    function PointerOpenHost(): ReactElement {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button
            type="button"
            data-testid="pointer-open"
            onPointerDown={() => setOpen(true)}
          >
            Open
          </button>
          <Dialog
            open={open}
            titleId="ghost-dialog-title"
            onClose={() => {
              onClose();
              setOpen(false);
            }}
          >
            <h2 id="ghost-dialog-title">Ghost dialog</h2>
            <button type="button">Inside</button>
          </Dialog>
        </>
      );
    }

    render(<PointerOpenHost />);
    const trigger = screen.getByTestId('pointer-open');
    trigger.dispatchEvent(
      new PointerEvent('pointerdown', { bubbles: true, button: 0, pointerId: 1 }),
    );
    await waitFor(() => expect(screen.getByRole('dialog')).toBeTruthy());
    const backdrop = screen
      .getByRole('dialog')
      .parentElement!.querySelector('[data-dialog-backdrop]') as HTMLElement;
    // 仅 click、无先于本手势的 backdrop pointerdown → 不得关闭
    backdrop.dispatchEvent(new MouseEvent('click', { bubbles: true, button: 0 }));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  test('restores trigger focus after close', async () => {
    const user = userEvent.setup();
    render(<DialogHost />);
    const trigger = screen.getByTestId('open-dialog');
    await user.click(trigger);
    await waitFor(() => expect(screen.getByRole('dialog')).toBeTruthy());
    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
    expectFocused(trigger);
  });

  test('background content is inert/aria-hidden while open', async () => {
    render(<DialogHost open />);
    const dialog = screen.getByRole('dialog');
    let portalRoot: HTMLElement = dialog;
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

  test('applies className to surface', () => {
    render(<DialogHost open className="custom-dialog" />);
    expect(screen.getByRole('dialog').className).toContain('custom-dialog');
  });

  test('locks body scroll while open', () => {
    const { rerender } = render(<DialogHost open />);
    expect(document.body.style.overflow).toBe('hidden');
    rerender(<DialogHost open={false} />);
    expect(document.body.style.overflow).toBe('');
  });
});
