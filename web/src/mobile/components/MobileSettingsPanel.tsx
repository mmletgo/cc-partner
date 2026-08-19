/**
 * MobileSettingsPanel（移动端设置：外观）
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端工作台需要浅色/深色主题切换入口，放在 Settings 避免挤占 shell 常驻 chrome。
 *   tmux 依赖检测/安装属于桌面主机职责，手机浏览器不能可靠探测或安装，不在此展示。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 Settings 说明与外观主题行（复用 ThemeToggle + useTheme）。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { ThemeToggle } from '@/components/layout';
import { useTheme } from '@/hooks/useTheme';
import styles from '../MobileWorkbench.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   主题偏好按本浏览器 localStorage（cp-theme）持久化，与桌面共用 useTheme 合同。
 *
 * Code Logic（这个组件做什么）:
 *   展示标题与外观切换行。
 */
export function MobileSettingsPanel(): ReactElement {
  const { t } = useTranslation(['workbench', 'settings']);
  const { theme } = useTheme();
  const titleId = 'mobile-settings-title';
  const themeTitleId = 'mobile-settings-theme-title';
  const themeCurrentLabel =
    theme === 'dark'
      ? t('workbench:mobile.theme.currentDark')
      : t('workbench:mobile.theme.currentLight');

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeader}>
        <h1 id={titleId}>{t('workbench:mobile.placeholders.settings.title')}</h1>
      </div>
      <p className={styles.panelState}>
        {t('workbench:mobile.placeholders.settings.label')}
      </p>

      <section className={styles.settingsSection} aria-labelledby={themeTitleId}>
        <h2 id={themeTitleId} className={styles.settingsSectionTitle}>
          {t('workbench:mobile.theme.title')}
        </h2>
        <div className={styles.themeRow} data-testid="mobile-theme-row">
          <div className={styles.themeRowText}>
            <p className={styles.themeRowTitle}>{t('workbench:mobile.theme.appearance')}</p>
            <p className={styles.themeRowMeta} data-testid="mobile-theme-current">
              {themeCurrentLabel}
            </p>
          </div>
          <ThemeToggle className={styles.themeToggle} />
        </div>
      </section>
    </section>
  );
}
