// @vitest-environment jsdom
/**
 * WorkbenchInspector 键盘与 ARIA 契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   右侧 inspector tabs 必须与终端/文件 tabs 共享 roving focus，并用 aria-controls/tabpanel 关联面板。
 *
 * Code Logic（这个测试做什么）:
 *   渲染最小 stub 叶子 props 的 WorkbenchInspector，断言 tabIndex、Arrow 切换、tabpanel 关联。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { WorkbenchInspector } from './WorkbenchInspector';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';

vi.mock('./WorkbenchFileInspector', () => ({
  WorkbenchFileInspector: () => <div data-testid="files-panel-body">files body</div>,
}));

vi.mock('./WorkbenchGitInspector', () => ({
  WorkbenchGitInspector: () => <div data-testid="history-panel-body">history body</div>,
}));

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试只需验证 tab 外壳语义，叶子 props 用空 stub 即可。
 *
 * Code Logic（这个函数做什么）:
 *   受控渲染 WorkbenchInspector，返回 setInspectorTab spy 与 rerender 辅助。
 */
function renderInspector(initial: WorkbenchInspectorTab = 'files') {
  let tab: WorkbenchInspectorTab = initial;
  const setInspectorTab = vi.fn((next: WorkbenchInspectorTab) => {
    tab = next;
  });
  const fileInspector = {} as never;
  const gitInspector = {} as never;

  const view = render(
    <I18nextProvider i18n={i18n}>
      <WorkbenchInspector
        inspectorTab={tab}
        setInspectorTab={setInspectorTab}
        fileInspector={fileInspector}
        gitInspector={gitInspector}
      />
    </I18nextProvider>,
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   受控组件需要在 setState 后以最新 tab 重渲染，才能观察 aria-selected 变化。
   *
   * Code Logic（这个函数做什么）:
   *   用当前 tab 闭包值 rerender。
   */
  const rerenderCurrent = () => {
    view.rerender(
      <I18nextProvider i18n={i18n}>
        <WorkbenchInspector
          inspectorTab={tab}
          setInspectorTab={setInspectorTab}
          fileInspector={fileInspector}
          gitInspector={gitInspector}
        />
      </I18nextProvider>,
    );
  };

  return { setInspectorTab, rerenderCurrent, getTab: () => tab };
}

describe('WorkbenchInspector keyboard semantics', () => {
  test('selected tab is the only tab stop and owns aria-controls/tabpanel', () => {
    renderInspector('files');
    const filesTab = screen.getByRole('tab', { name: '项目文件夹' });
    const historyTab = screen.getByRole('tab', { name: 'Git 历史' });
    expect(filesTab.getAttribute('tabindex')).toBe('0');
    expect(historyTab.getAttribute('tabindex')).toBe('-1');
    expect(filesTab.getAttribute('aria-controls')).toBe('workbench-inspector-panel-files');
    expect(historyTab.getAttribute('aria-controls')).toBe('workbench-inspector-panel-history');
    const panel = screen.getByRole('tabpanel');
    expect(panel.id).toBe('workbench-inspector-panel-files');
    expect(panel.getAttribute('aria-labelledby')).toBe('workbench-inspector-tab-files');
    expect(screen.getByTestId('files-panel-body')).toBeTruthy();
  });

  test('ArrowRight and ArrowLeft activate adjacent inspector tabs', () => {
    const { setInspectorTab, rerenderCurrent } = renderInspector('files');
    const filesTab = screen.getByRole('tab', { name: '项目文件夹' });
    fireEvent.keyDown(filesTab, { key: 'ArrowRight' });
    expect(setInspectorTab).toHaveBeenCalledWith('history');
    rerenderCurrent();
    expect(screen.getByRole('tab', { name: 'Git 历史' }).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByTestId('history-panel-body')).toBeTruthy();

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Git 历史' }), { key: 'ArrowLeft' });
    expect(setInspectorTab).toHaveBeenLastCalledWith('files');
  });

  test('Home and End jump to first and last inspector tabs', () => {
    const { setInspectorTab } = renderInspector('history');
    fireEvent.keyDown(screen.getByRole('tab', { name: 'Git 历史' }), { key: 'Home' });
    expect(setInspectorTab).toHaveBeenCalledWith('files');
    fireEvent.keyDown(screen.getByRole('tab', { name: 'Git 历史' }), { key: 'End' });
    expect(setInspectorTab).toHaveBeenCalledWith('history');
  });
});
