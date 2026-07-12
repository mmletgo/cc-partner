/**
 * MobileSettingsPanel（移动端设置：依赖区域）
 *
 * Business Logic（为什么需要这个组件）:
 *   Attention 的 settings/dependencies target 必须进入现有依赖设置语义，不能新建第二套依赖管理。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 Settings 依赖说明，并复用 WorkbenchDependencyCard 展示/安装/重检 tmux 依赖。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { WorkbenchDependencyCard } from '@/components/domain/WorkbenchDependencyCard';
import styles from '../MobileWorkbench.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   移动导航 Settings 与 Inbox 环境受阻条目需要落到同一依赖权威界面。
 *
 * Code Logic（这个组件做什么）:
 *   展示标题与 WorkbenchDependencyCard（compact）。
 */
export function MobileSettingsPanel(): ReactElement {
  const { t } = useTranslation(['workbench', 'settings']);
  const titleId = 'mobile-settings-title';

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeader}>
        <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
        <h1 id={titleId}>{t('workbench:mobile.placeholders.settings.title')}</h1>
      </div>
      <p className={styles.panelState}>
        {t('workbench:mobile.placeholders.settings.label')}
      </p>
      <div id="mobile-settings-dependencies">
        <WorkbenchDependencyCard compact />
      </div>
    </section>
  );
}
