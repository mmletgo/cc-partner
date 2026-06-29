import { useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';
import styles from './MobileWorkbench.module.css';

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
  const { t } = useTranslation(['workbench']);
  const activeProject: string | null = null;
  const activeWorktree: string | null = null;
  const activeSession: string | null = null;

  const panelPlaceholders: Record<MobileWorkbenchPanel, { title: string; label: string }> = {
    projects: {
      title: t('workbench:mobile.placeholders.projects.title'),
      label: t('workbench:mobile.placeholders.projects.label'),
    },
    terminal: {
      title: t('workbench:mobile.placeholders.terminal.title'),
      label: t('workbench:mobile.placeholders.terminal.label'),
    },
    files: {
      title: t('workbench:mobile.placeholders.files.title'),
      label: t('workbench:mobile.placeholders.files.label'),
    },
    git: {
      title: t('workbench:mobile.placeholders.git.title'),
      label: t('workbench:mobile.placeholders.git.label'),
    },
    worktrees: {
      title: t('workbench:mobile.placeholders.worktrees.title'),
      label: t('workbench:mobile.placeholders.worktrees.label'),
    },
    prompt: {
      title: t('workbench:mobile.placeholders.prompt.title'),
      label: t('workbench:mobile.placeholders.prompt.label'),
    },
    settings: {
      title: t('workbench:mobile.placeholders.settings.title'),
      label: t('workbench:mobile.placeholders.settings.label'),
    },
  };
  const placeholder = panelPlaceholders[panel];

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
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id="mobile-panel-title">{placeholder.title}</h1>
        </div>
        <div className={styles.placeholder}>{placeholder.label}</div>
      </section>
    </MobileWorkbenchShell>
  );
}
