import { useState } from 'react';
import type { ReactElement } from 'react';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';
import styles from './MobileWorkbench.module.css';

const PANEL_PLACEHOLDERS: Record<MobileWorkbenchPanel, { title: string; label: string }> = {
  projects: { title: '项目', label: '选择项目' },
  terminal: { title: '终端', label: '等待会话' },
  files: { title: '文件', label: '等待项目' },
  git: { title: 'Git', label: '等待仓库' },
  worktrees: { title: 'Worktrees', label: '等待项目' },
  prompt: { title: 'Prompt', label: '等待输入' },
  settings: { title: '设置', label: '移动端' },
};

/**
 * MobileWorkbench（移动端工作台占位页面）
 *
 * Business Logic（为什么需要这个组件）:
 *   Task 5 需要先搭出 `/mobile` 的 Workbench shell，让后续项目、终端、文件和 Git 面板能逐步接入。
 *
 * Code Logic（这个组件做什么）:
 *   管理当前面板与项目/worktree/session 占位状态，渲染响应式 MobileWorkbenchShell，并在内容区显示当前面板占位。
 */
export function MobileWorkbench(): ReactElement {
  const [panel, setPanel] = useState<MobileWorkbenchPanel>('projects');
  const [activeProject] = useState<string | null>(null);
  const [activeWorktree] = useState<string | null>(null);
  const [activeSession] = useState<string | null>(null);

  const placeholder = PANEL_PLACEHOLDERS[panel];

  return (
    <MobileWorkbenchShell
      panel={panel}
      project={activeProject}
      worktree={activeWorktree}
      session={activeSession}
      onPanelChange={setPanel}
    >
      <section className={styles.panel} aria-labelledby="mobile-panel-title">
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>Mobile Workbench</p>
          <h1 id="mobile-panel-title">{placeholder.title}</h1>
        </div>
        <div className={styles.placeholder}>{placeholder.label}</div>
      </section>
    </MobileWorkbenchShell>
  );
}
