/**
 * Workbench 检查器外壳 —— 协调 files / history 两个 tab，并把对应叶子视图挂载到右侧 inspectorPane。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspector tab 切换逻辑（inspectorTabs + 条件渲染 files/history）
 *   抽到独立协调组件。该组件只负责根据当前 tab 渲染对应叶子视图（WorkbenchFileInspector / WorkbenchGitInspector），
 *   不持有业务状态、不导入 workbenchApi、不订阅 Tauri 事件。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染顶部 tab 切换按钮（aria tablist + roving tabindex + aria-controls）；
 *   - 选中 tab 对应的 panel 使用 role=tabpanel 与稳定 id 关联；
 *   - inspectorTab === 'files' 时挂载 WorkbenchFileInspector，传入文件域 controller 派生 props；
 *   - inspectorTab === 'history' 时挂载 WorkbenchGitInspector，传入 Git 域 controller 派生 props。
 *
 * 边界：本组件不持有 inspectorTab 状态本身——它仍由 Workbench.tsx 持有（loadGitHistory effect 依赖它），
 * 因此 inspectorTab / setInspectorTab 由页面透传，确保 worktree 切换 / merge 完成等流程仍能触发 Git 历史刷新。
 */
import { useCallback } from 'react';
import type { KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type {
  WorkbenchGitCommit,
  WorkbenchMergeStage,
  WorkbenchWorktree,
} from '@/lib/types';
import { getRovingTabIndex, type RovingTabKey } from '@/lib/rovingTablist';
import styles from './Workbench.module.css';
import { WorkbenchFileInspector } from './WorkbenchFileInspector';
import type { WorkbenchFileInspectorProps } from './WorkbenchFileInspector';
import { WorkbenchGitInspector } from './WorkbenchGitInspector';
import type { WorkbenchGitInspectorProps } from './WorkbenchGitInspector';
import type { WorktreeBusyKind } from './controllers/useWorkbenchWorktreeGitController';

export type WorkbenchInspectorTab = 'files' | 'history';

const INSPECTOR_TABS: readonly WorkbenchInspectorTab[] = ['files', 'history'] as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   inspector tab 与 panel 需要稳定 DOM id 建立 aria-controls / aria-labelledby 关系。
 *
 * Code Logic（这个函数做什么）:
 *   按 tab 名返回 button id 与 panel id 字符串。
 */
function inspectorAriaIds(tab: WorkbenchInspectorTab): {
  tabButtonId: string;
  tabPanelId: string;
} {
  return {
    tabButtonId: `workbench-inspector-tab-${tab}`,
    tabPanelId: `workbench-inspector-panel-${tab}`,
  };
}

/**
 * WorkbenchInspector 输入 props。
 *
 * Business Logic: inspectorTab + setInspectorTab 由 Workbench.tsx 持有并透传；
 * fileInspector / gitInspector 是已绑定 controller 派生 props 的对象，本组件把它们原样下发给叶子视图。
 */
export interface WorkbenchInspectorProps {
  inspectorTab: WorkbenchInspectorTab;
  setInspectorTab: (tab: WorkbenchInspectorTab) => void;
  fileInspector: WorkbenchFileInspectorProps;
  gitInspector: WorkbenchGitInspectorProps;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 右侧检查器是用户切换文件树/Git 历史的入口；tab 切换是纯 UI 行为，但 tab 的 active 态会
 *   触发 Workbench.tsx 的 loadGitHistory effect，所以 tab 状态本身保留在页面。本组件只负责组合 tab UI 与叶子视图。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 tab 按钮（roving focus + aria-controls）+ 对应 tabpanel 叶子组件；不持有状态、不调用任何 API。
 */
export function WorkbenchInspector(props: WorkbenchInspectorProps) {
  const { t } = useTranslation(['workbench']);
  const { inspectorTab, setInspectorTab, fileInspector, gitInspector } = props;
  const activeIds = inspectorAriaIds(inspectorTab);

  /**
   * Business Logic（为什么需要这个函数）:
   *   键盘用户在 inspector tablist 内用方向键/Home/End 切换时，焦点与选中态必须同步。
   *
   * Code Logic（这个函数做什么）:
   *   识别 RovingTabKey；用 getRovingTabIndex 求下一索引；setInspectorTab 并 focus 目标按钮。
   */
  const handleTabKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>) => {
      const key = event.key;
      if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
        return;
      }
      event.preventDefault();
      const currentIndex = INSPECTOR_TABS.indexOf(inspectorTab);
      const nextIndex = getRovingTabIndex(currentIndex, key as RovingTabKey, INSPECTOR_TABS.length);
      const nextTab = INSPECTOR_TABS[nextIndex];
      setInspectorTab(nextTab);
      const { tabButtonId } = inspectorAriaIds(nextTab);
      if (typeof window !== 'undefined') {
        window.requestAnimationFrame(() => {
          document.getElementById(tabButtonId)?.focus();
        });
      }
    },
    [inspectorTab, setInspectorTab],
  );

  return (
    <>
      <div className={styles.inspectorTabs} role="tablist" aria-label={t('workbench:inspectorTabs')}>
        <button
          id={inspectorAriaIds('files').tabButtonId}
          type="button"
          className={styles.inspectorTab}
          data-active={inspectorTab === 'files' || undefined}
          role="tab"
          aria-selected={inspectorTab === 'files'}
          aria-controls={inspectorAriaIds('files').tabPanelId}
          tabIndex={inspectorTab === 'files' ? 0 : -1}
          onClick={() => setInspectorTab('files')}
          onKeyDown={handleTabKeyDown}
        >
          {t('workbench:filesTitle')}
        </button>
        <button
          id={inspectorAriaIds('history').tabButtonId}
          type="button"
          className={styles.inspectorTab}
          data-active={inspectorTab === 'history' || undefined}
          role="tab"
          aria-selected={inspectorTab === 'history'}
          aria-controls={inspectorAriaIds('history').tabPanelId}
          tabIndex={inspectorTab === 'history' ? 0 : -1}
          onClick={() => setInspectorTab('history')}
          onKeyDown={handleTabKeyDown}
        >
          {t('workbench:gitHistoryTitle')}
        </button>
      </div>

      <div
        id={activeIds.tabPanelId}
        role="tabpanel"
        aria-labelledby={activeIds.tabButtonId}
        className={styles.inspectorTabPanel}
      >
        {inspectorTab === 'files' ? (
          <WorkbenchFileInspector {...fileInspector} />
        ) : (
          <WorkbenchGitInspector {...gitInspector} />
        )}
      </div>
    </>
  );
}

// 类型重新导出，便于 Workbench.tsx 单点 import；避免页面同时引用叶子组件类型与协调组件类型。
export type {
  WorkbenchFileInspectorProps,
  WorkbenchGitInspectorProps,
  WorktreeBusyKind,
  WorkbenchGitCommit,
  WorkbenchMergeStage,
  WorkbenchWorktree,
};
