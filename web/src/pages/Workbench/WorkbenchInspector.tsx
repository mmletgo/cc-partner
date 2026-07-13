/**
 * Workbench 检查器外壳 —— 协调 files / history 两个 tab，并把对应叶子视图挂载到右侧 inspectorPane。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspector tab 切换逻辑（inspectorTabs + 条件渲染 files/history）
 *   抽到独立协调组件。该组件只负责根据当前 tab 渲染对应叶子视图（WorkbenchFileInspector / WorkbenchGitInspector），
 *   不持有业务状态、不导入 workbenchApi、不订阅 Tauri 事件。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染顶部 tab 切换按钮（aria tablist）；
 *   - inspectorTab === 'files' 时挂载 WorkbenchFileInspector，传入文件域 controller 派生 props；
 *   - inspectorTab === 'history' 时挂载 WorkbenchGitInspector，传入 Git 域 controller 派生 props。
 *
 * 边界：本组件不持有 inspectorTab 状态本身——它仍由 Workbench.tsx 持有（loadGitHistory effect 依赖它），
 * 因此 inspectorTab / setInspectorTab 由页面透传，确保 worktree 切换 / merge 完成等流程仍能触发 Git 历史刷新。
 */
import { useTranslation } from 'react-i18next';
import type {
  WorkbenchGitCommit,
  WorkbenchMergeStage,
  WorkbenchWorktree,
} from '@/lib/types';
import styles from './Workbench.module.css';
import { WorkbenchFileInspector } from './WorkbenchFileInspector';
import type { WorkbenchFileInspectorProps } from './WorkbenchFileInspector';
import { WorkbenchGitInspector } from './WorkbenchGitInspector';
import type { WorkbenchGitInspectorProps } from './WorkbenchGitInspector';
import type { WorktreeBusyKind } from './controllers/useWorkbenchWorktreeGitController';

export type WorkbenchInspectorTab = 'files' | 'history';

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
 *   渲染 tab 按钮 + 条件渲染对应叶子组件；不持有状态、不调用任何 API。
 */
export function WorkbenchInspector(props: WorkbenchInspectorProps) {
  const { t } = useTranslation(['workbench']);
  const { inspectorTab, setInspectorTab, fileInspector, gitInspector } = props;

  return (
    <>
      <div className={styles.inspectorTabs} role="tablist" aria-label={t('workbench:inspectorTabs')}>
        <button
          type="button"
          className={styles.inspectorTab}
          data-active={inspectorTab === 'files' || undefined}
          role="tab"
          aria-selected={inspectorTab === 'files'}
          onClick={() => setInspectorTab('files')}
        >
          {t('workbench:filesTitle')}
        </button>
        <button
          type="button"
          className={styles.inspectorTab}
          data-active={inspectorTab === 'history' || undefined}
          role="tab"
          aria-selected={inspectorTab === 'history'}
          onClick={() => setInspectorTab('history')}
        >
          {t('workbench:gitHistoryTitle')}
        </button>
      </div>

      {inspectorTab === 'files' ? (
        <WorkbenchFileInspector {...fileInspector} />
      ) : (
        <WorkbenchGitInspector {...gitInspector} />
      )}
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
