// @vitest-environment jsdom
/**
 * WorkbenchPaneTools 组件测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   终端工具栏的四个窗格操作（右分屏/下分屏/切换/关闭）被收纳进「窗格」菜单，
 *   需要锁住触发按钮、菜单四动作的存在性、禁用逻辑与「点击执行并关闭」的交互契约，
 *   保证既有按 aria-label 查询的测试与用户肌肉记忆不被破坏。
 *
 * Code Logic（这个测试做什么）:
 *   用 i18next zh 资源渲染组件；点击触发按钮打开菜单（Dialog portal 到 body）；
 *   断言 4 个动作按钮存在且 disabled 状态正确；点击动作断言回调触发且菜单关闭。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import i18n from 'i18next';
import { I18nextProvider, initReactI18next } from 'react-i18next';
import type { ReactElement } from 'react';

import { WorkbenchPaneTools } from './WorkbenchPaneTools';

const resources = {
  zh: {
    workbench: {
      splitPaneRight: '左右分屏',
      splitPaneDown: '上下分屏',
      switchPane: '切换 pane',
      closePane: '关闭当前 pane',
      paneTools: {
        open: '窗格',
      },
    },
  },
};

void i18n.use(initReactI18next).init({
  lng: 'zh',
  resources,
  interpolation: { escapeValue: false },
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需挂载 i18n 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 包裹 children。
 */
function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   各用例共享默认 props 与回调 spy，减少重复。
 *
 * Code Logic（这个函数做什么）:
 *   返回带 vi.fn 回调的完整 props 组合。
 */
function makeProps(overrides: Partial<Parameters<typeof WorkbenchPaneTools>[0]> = {}) {
  return {
    canUsePanes: true,
    canSwitchPane: true,
    remoteWriteDisabled: false,
    onSplitPane: vi.fn(),
    onSwitchPane: vi.fn(),
    onClosePane: vi.fn(),
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
});

describe('WorkbenchPaneTools', () => {
  test('renders trigger button with paneTools label and closed state', () => {
    render(wrap(<WorkbenchPaneTools {...makeProps()} />));
    const trigger = screen.getByRole('button', { name: '窗格' });
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(trigger.getAttribute('aria-haspopup')).toBe('dialog');
    expect((trigger as HTMLButtonElement).disabled).toBe(false);
    // 菜单未打开：动作按钮不存在。
    expect(screen.queryByRole('button', { name: '左右分屏' })).toBeNull();
  });

  test('opens menu and exposes all four pane actions with correct disabled state', () => {
    render(wrap(<WorkbenchPaneTools {...makeProps()} />));
    fireEvent.click(screen.getByRole('button', { name: '窗格' }));

    const splitRight = screen.getByRole('button', { name: '左右分屏' }) as HTMLButtonElement;
    const splitDown = screen.getByRole('button', { name: '上下分屏' }) as HTMLButtonElement;
    const switchPane = screen.getByRole('button', { name: '切换 pane' }) as HTMLButtonElement;
    const closePane = screen.getByRole('button', { name: '关闭当前 pane' }) as HTMLButtonElement;
    expect(splitRight.disabled).toBe(false);
    expect(splitDown.disabled).toBe(false);
    expect(switchPane.disabled).toBe(false);
    expect(closePane.disabled).toBe(false);
    // 菜单打开时共享 Dialog 会让背景 inert，触发按钮暂时退出可访问树（aria-expanded=false 已在首用例锁定）。
    expect(splitRight.closest('[role="dialog"]')).toBeTruthy();
  });

  test('split/close actions follow canUsePanes while switch follows canSwitchPane', () => {
    render(
      wrap(
        <WorkbenchPaneTools
          {...makeProps({ canUsePanes: false, canSwitchPane: true })}
        />,
      ),
    );
    const trigger = screen.getByRole('button', { name: '窗格' }) as HTMLButtonElement;
    // 切换窗格仍可用，触发按钮不得禁用。
    expect(trigger.disabled).toBe(false);
    fireEvent.click(trigger);

    expect(
      (screen.getByRole('button', { name: '左右分屏' }) as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByRole('button', { name: '上下分屏' }) as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByRole('button', { name: '切换 pane' }) as HTMLButtonElement).disabled,
    ).toBe(false);
    expect(
      (screen.getByRole('button', { name: '关闭当前 pane' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  test('remote write disable disables every action and the trigger', () => {
    render(wrap(<WorkbenchPaneTools {...makeProps({ remoteWriteDisabled: true })} />));
    const trigger = screen.getByRole('button', { name: '窗格' }) as HTMLButtonElement;
    expect(trigger.disabled).toBe(true);
    fireEvent.click(trigger);
    // 触发按钮禁用时菜单不应暴露动作。
    expect(screen.queryByRole('button', { name: '左右分屏' })).toBeNull();
  });

  test('clicking an action fires its callback and closes the menu', () => {
    const props = makeProps();
    render(wrap(<WorkbenchPaneTools {...props} />));
    fireEvent.click(screen.getByRole('button', { name: '窗格' }));

    fireEvent.click(screen.getByRole('button', { name: '左右分屏' }));
    expect(props.onSplitPane).toHaveBeenCalledWith('right');
    // 菜单关闭后动作按钮卸载。
    expect(screen.queryByRole('button', { name: '上下分屏' })).toBeNull();

    // 重新打开，验证其余动作。
    fireEvent.click(screen.getByRole('button', { name: '窗格' }));
    fireEvent.click(screen.getByRole('button', { name: '上下分屏' }));
    expect(props.onSplitPane).toHaveBeenCalledWith('down');

    fireEvent.click(screen.getByRole('button', { name: '窗格' }));
    fireEvent.click(screen.getByRole('button', { name: '切换 pane' }));
    expect(props.onSwitchPane).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: '窗格' }));
    fireEvent.click(screen.getByRole('button', { name: '关闭当前 pane' }));
    expect(props.onClosePane).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('button', { name: '切换 pane' })).toBeNull();
  });
});
