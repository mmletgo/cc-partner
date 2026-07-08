import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { WorkbenchBrowserWorkspace } from '@/components/domain';
import type { WorkbenchTransport } from '@/api/workbenchTransport';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import styles from './MobileBrowserPanel.module.css';

export interface MobileBrowserPanelProps {
  transport: WorkbenchTransport;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
}

/**
 * MobileBrowserPanel（移动端浏览器预览面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 `/mobile` 需要查看本机或远端项目 dev server 效果，且必须使用手机可访问的同源代理路径。
 *
 * Code Logic（这个组件做什么）:
 *   包装 WorkbenchBrowserWorkspace，固定 surface 为 mobile，并提供移动端外层布局。
 */
export function MobileBrowserPanel({
  transport,
  project,
  worktree,
}: MobileBrowserPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);

  return (
    <section className={styles.panel} aria-label={t('workbench:mobile.browser.title')}>
      <WorkbenchBrowserWorkspace
        surface="mobile"
        transport={transport}
        project={project}
        worktree={worktree}
      />
    </section>
  );
}
