// @vitest-environment jsdom
/**
 * useModalLayer 行为测试
 *
 * Business Logic（为什么需要这些测试）:
 *   模态层栈是 Dialog/Drawer 的键盘可达性与焦点安全合同；
 *   必须覆盖 Escape/Tab 顶层独占、inert/scroll 引用计数、焦点恢复与卸载清理。
 *
 * Code Logic（这些测试做什么）:
 *   用 jsdom + user-event 驱动最小 harness 组件，断言层栈副作用与键盘行为。
 */

import { useRef, useState, type ReactElement, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useModalLayer, type ModalLayerOptions } from './useModalLayer';

/**
 * 断言元素拥有文档焦点（项目未装 jest-dom，不用 toHaveFocus）
 */
function expectFocused(el: Element): void {
  expect(document.activeElement).toBe(el);
}

/**
 * 断言除 exclude 外的 body 直接子节点均被 aria-hidden/inert
 */
function expectBodySiblingsHidden(exclude: Element | Element[]): void {
  const excluded = new Set(Array.isArray(exclude) ? exclude : [exclude]);
  for (const child of Array.from(document.body.children)) {
    if (excluded.has(child)) {
      expect(child.getAttribute('aria-hidden')).not.toBe('true');
      continue;
    }
    expect(child.getAttribute('aria-hidden')).toBe('true');
  }
}

interface HarnessProps {
  open: boolean;
  closeOnEscape?: boolean;
  onClose?: () => void;
  initialFocus?: boolean;
  label?: string;
  extraFocusable?: boolean;
}

/**
 * 最小模态层 harness：portal 到 body 的 surface，驱动 useModalLayer。
 */
function ModalHarness(props: HarnessProps): ReactElement | null {
  const {
    open,
    closeOnEscape = true,
    onClose = () => undefined,
    initialFocus = false,
    label = 'layer',
    extraFocusable = true,
  } = props;
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const initialFocusRef = useRef<HTMLButtonElement | null>(null);

  useModalLayer({
    open,
    surfaceRef,
    initialFocusRef: initialFocus ? initialFocusRef : undefined,
    closeOnEscape,
    onClose,
  });

  if (!open) return null;

  return createPortal(
    <div data-testid={`${label}-root`}>
      <div
        ref={surfaceRef}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        data-testid={`${label}-surface`}
      >
        {initialFocus ? (
          <button type="button" ref={initialFocusRef} data-testid={`${label}-initial`}>
            initial
          </button>
        ) : null}
        {extraFocusable ? (
          <>
            <button type="button" data-testid={`${label}-first`}>
              first
            </button>
            <button type="button" data-testid={`${label}-last`}>
              last
            </button>
          </>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}

/**
 * 双层嵌套 harness：可同时打开两层 modal。
 */
function NestedHarness(props: {
  firstOpen: boolean;
  secondOpen: boolean;
  onCloseFirst: () => void;
  onCloseSecond: () => void;
}): ReactElement {
  return (
    <>
      <div data-testid="app-root">
        <button type="button" data-testid="app-trigger">
          app
        </button>
      </div>
      <ModalHarness open={props.firstOpen} label="first" onClose={props.onCloseFirst} />
      <ModalHarness open={props.secondOpen} label="second" onClose={props.onCloseSecond} />
    </>
  );
}

/**
 * 可控开关 harness，便于 open→close 序列。
 */
function ControllableHarness(props: {
  closeOnEscape?: boolean;
  withInitialFocus?: boolean;
}): ReactElement {
  const [open, setOpen] = useState(false);
  return (
    <>
      <div data-testid="page">
        <button type="button" data-testid="trigger" onClick={() => setOpen(true)}>
          open
        </button>
      </div>
      <ModalHarness
        open={open}
        closeOnEscape={props.closeOnEscape}
        initialFocus={props.withInitialFocus}
        onClose={() => setOpen(false)}
      />
    </>
  );
}

afterEach(() => {
  cleanup();
  document.body.style.overflow = '';
  document.body.removeAttribute('aria-hidden');
  // 清理可能残留的 inert/aria-hidden（防止测试间泄漏）
  for (const child of Array.from(document.body.children)) {
    child.removeAttribute('aria-hidden');
    if ('inert' in child) {
      (child as HTMLElement & { inert: boolean }).inert = false;
    }
  }
});

beforeEach(() => {
  document.body.innerHTML = '';
});

describe('useModalLayer', () => {
  test('opens with initial focus on initialFocusRef', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness withInitialFocus />);
    await user.click(screen.getByTestId('trigger'));
    await waitFor(() => {
      expectFocused(screen.getByTestId('layer-initial'));
    });
  });

  test('falls back to surface when no focusable children', async () => {
    function EmptySurface(): ReactElement {
      const [open, setOpen] = useState(true);
      return (
        <>
          <button type="button" data-testid="outside" onClick={() => setOpen(true)}>
            outside
          </button>
          <ModalHarness open={open} extraFocusable={false} onClose={() => setOpen(false)} />
        </>
      );
    }
    render(<EmptySurface />);
    await waitFor(() => {
      expectFocused(screen.getByTestId('layer-surface'));
    });
  });

  test('traps Tab and Shift+Tab within surface', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    await user.click(screen.getByTestId('trigger'));
    const first = screen.getByTestId('layer-first');
    const last = screen.getByTestId('layer-last');
    await waitFor(() => expectFocused(first));

    await user.tab();
    expectFocused(last);
    await user.tab();
    expectFocused(first);
    await user.tab({ shift: true });
    expectFocused(last);
  });

  test('Tab trap wraps when focus is on surface (tabIndex=-1)', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    await user.click(screen.getByTestId('trigger'));
    const first = screen.getByTestId('layer-first');
    const last = screen.getByTestId('layer-last');
    const surface = screen.getByTestId('layer-surface');
    await waitFor(() => expectFocused(first));

    surface.focus();
    expectFocused(surface);

    await user.tab({ shift: true });
    expectFocused(last);

    surface.focus();
    expectFocused(surface);
    await user.tab();
    expectFocused(first);
  });

  test('contenteditable inside surface participates in Tab trap', async () => {
    const user = userEvent.setup();

    function ContentEditableHarness(): ReactElement {
      const [open, setOpen] = useState(true);
      const surfaceRef = useRef<HTMLDivElement | null>(null);
      useModalLayer({
        open,
        surfaceRef,
        closeOnEscape: true,
        onClose: () => setOpen(false),
      });
      return createPortal(
        <div data-testid="layer-root">
          <div
            ref={surfaceRef}
            role="dialog"
            aria-modal="true"
            aria-label="layer"
            tabIndex={-1}
            data-testid="layer-surface"
          >
            <button type="button" data-testid="layer-first">
              first
            </button>
            <div contentEditable data-testid="layer-editable" suppressContentEditableWarning>
              edit
            </div>
            <button type="button" data-testid="layer-last">
              last
            </button>
          </div>
        </div>,
        document.body,
      );
    }

    render(
      <>
        <div data-testid="page">page</div>
        <ContentEditableHarness />
      </>,
    );

    const first = screen.getByTestId('layer-first');
    const editable = screen.getByTestId('layer-editable');
    const last = screen.getByTestId('layer-last');
    await waitFor(() => expectFocused(first));

    await user.tab();
    expectFocused(editable);
    await user.tab();
    expectFocused(last);
    await user.tab();
    expectFocused(first);
  });

  test('late body siblings become inert while modal is open', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    await user.click(screen.getByTestId('trigger'));
    const root = screen.getByTestId('layer-root');
    await waitFor(() => {
      expectBodySiblingsHidden(root);
    });

    const late = document.createElement('div');
    late.setAttribute('data-testid', 'late-toast');
    late.textContent = 'toast';
    document.body.appendChild(late);

    await waitFor(() => {
      expect(late.getAttribute('aria-hidden')).toBe('true');
    });
    const lateHtml = late as HTMLElement & { inert?: boolean };
    expect(lateHtml.inert === true || late.hasAttribute('inert')).toBe(true);

    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByTestId('layer-surface')).toBeNull();
    });
    expect(late.getAttribute('aria-hidden')).not.toBe('true');

    late.remove();
  });

  test('Escape calls onClose when closeOnEscape is true', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    function EscHarness(): ReactElement {
      return (
        <>
          <div data-testid="page">
            <span>page</span>
          </div>
          <ModalHarness open closeOnEscape onClose={onClose} />
        </>
      );
    }
    render(<EscHarness />);
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('Escape is ignored when closeOnEscape is false', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    function EscHarness(): ReactElement {
      return (
        <>
          <div data-testid="page">
            <span>page</span>
          </div>
          <ModalHarness open closeOnEscape={false} onClose={onClose} />
        </>
      );
    }
    render(<EscHarness />);
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
  });

  test('marks background siblings inert and aria-hidden', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    await user.click(screen.getByTestId('trigger'));
    const root = screen.getByTestId('layer-root');
    await waitFor(() => {
      expectBodySiblingsHidden(root);
    });
    const htmlRoot = root as HTMLElement & { inert?: boolean };
    expect(htmlRoot.inert === true || root.hasAttribute('inert')).toBe(false);
  });

  test('locks body scroll while open and restores on close', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    expect(document.body.style.overflow).toBe('');

    await user.click(screen.getByTestId('trigger'));
    expect(document.body.style.overflow).toBe('hidden');

    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByTestId('layer-surface')).toBeNull();
    });
    expect(document.body.style.overflow).toBe('');
  });

  test('nested layers: only top handles Escape; inert uses reference counting', async () => {
    const user = userEvent.setup();
    const onCloseFirst = vi.fn();
    const onCloseSecond = vi.fn();

    function NestedController(): ReactElement {
      const [firstOpen, setFirstOpen] = useState(true);
      const [secondOpen, setSecondOpen] = useState(true);
      return (
        <NestedHarness
          firstOpen={firstOpen}
          secondOpen={secondOpen}
          onCloseFirst={() => {
            onCloseFirst();
            setFirstOpen(false);
          }}
          onCloseSecond={() => {
            onCloseSecond();
            setSecondOpen(false);
          }}
        />
      );
    }

    render(<NestedController />);
    const firstRoot = screen.getByTestId('first-root');
    const secondRoot = screen.getByTestId('second-root');

    // 两层都开时：RTL container + firstRoot 应 inert，secondRoot 不 inert
    await waitFor(() => {
      expect(firstRoot.getAttribute('aria-hidden')).toBe('true');
      expect(secondRoot.getAttribute('aria-hidden')).not.toBe('true');
      expectBodySiblingsHidden(secondRoot);
    });

    await user.keyboard('{Escape}');
    expect(onCloseSecond).toHaveBeenCalledTimes(1);
    expect(onCloseFirst).not.toHaveBeenCalled();

    await waitFor(() => {
      expect(screen.queryByTestId('second-root')).toBeNull();
    });

    // 第二层关闭后，第一层恢复可交互；其它 body 兄弟仍 inert
    expect(firstRoot.getAttribute('aria-hidden')).not.toBe('true');
    expectBodySiblingsHidden(firstRoot);
    expect(document.body.style.overflow).toBe('hidden');

    await user.keyboard('{Escape}');
    expect(onCloseFirst).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(screen.queryByTestId('first-root')).toBeNull();
    });
    for (const child of Array.from(document.body.children)) {
      expect(child.getAttribute('aria-hidden')).not.toBe('true');
    }
    expect(document.body.style.overflow).toBe('');
  });

  test('restores focus to trigger on close', async () => {
    const user = userEvent.setup();
    render(<ControllableHarness />);
    const trigger = screen.getByTestId('trigger');
    await user.click(trigger);
    await waitFor(() => expectFocused(screen.getByTestId('layer-first')));
    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByTestId('layer-surface')).toBeNull();
    });
    expectFocused(trigger);
  });

  test('unmount cleanup removes scroll lock and inert', () => {
    const { unmount } = render(
      <>
        <div data-testid="page">page</div>
        <ModalHarness open onClose={() => undefined} />
      </>,
    );
    const root = screen.getByTestId('layer-root');
    expect(document.body.style.overflow).toBe('hidden');
    expectBodySiblingsHidden(root);

    unmount();
    expect(document.body.style.overflow).toBe('');
    for (const child of Array.from(document.body.children)) {
      expect(child.getAttribute('aria-hidden')).not.toBe('true');
    }
  });

  test('options type contract is exportable', () => {
    // 编译期契约：ModalLayerOptions 形状固定
    const options: ModalLayerOptions = {
      open: false,
      surfaceRef: { current: null } as RefObject<HTMLElement | null>,
      closeOnEscape: true,
      onClose: () => undefined,
    };
    expect(options.open).toBe(false);
  });
});
