/**
 * 活动统计设置面板 - 设置页「活动统计」tab 的纯渲染组件
 *
 * Business Logic（为什么需要这个组件）:
 *   记录窗口标题与明细保留天数服务活动统计页，不应再挤在健康提醒 tab。
 *
 * Code Logic（这个组件做什么）:
 *   复用 HealthPanel 的 section / toggle / number 行，只渲染统计相关两项并整体提交切片。
 */
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import type { HealthConfig } from '@/lib/types';
import type { HealthForm } from './settingsState';
import { HEALTH_RANGE, HealthSection, NumberRow, ToggleRow } from './HealthPanel';
import styles from './Settings.module.css';

export interface ActivityStatsPanelProps {
  /** 当前表单值 */
  form: HealthForm;
  /** 最近已应用配置快照(显示用) */
  applied: HealthConfig | null;
  /** 字段变更(浅合并,只改本地表单) */
  onPatch: (partial: Partial<HealthForm>) => void;
  /** 恢复活动统计默认 */
  onResetDefaults: () => void;
  /** 应用活动统计切片 */
  onApply: () => void;
  /** 应用中 */
  applying: boolean;
  /** 错误提示 */
  error: string | null;
  /** 默认配置是否可用 */
  canResetDefaults?: boolean;
}

/**
 * 活动统计设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在独立 tab 调整窗口标题记录与保留天数，避免和提醒节奏混在一起。
 *
 * Code Logic（这个组件做什么）:
 *   渲染一个统计栏目 Card + 恢复默认 / 应用配置。
 */
export function ActivityStatsPanel({
  form,
  applied,
  onPatch,
  onResetDefaults,
  onApply,
  applying,
  error,
  canResetDefaults = true,
}: ActivityStatsPanelProps) {
  const { t } = useTranslation(['settings', 'health', 'common']);

  return (
    <HealthSection
      id="settings-activity-privacy"
      title={t('settings:activity.title')}
      lead={t('settings:activity.subtitle')}
    >
      <div className={styles.toggleList}>
        <ToggleRow
          label={t('health:recordWindowTitle')}
          helper={t('health:recordWindowTitleDescription')}
          checked={form.recordWindowTitle}
          onToggle={(v) => onPatch({ recordWindowTitle: v })}
        />
      </div>
      <NumberRow
        label={t('health:retainDays')}
        helper={t('health:retainDaysDescription')}
        min={HEALTH_RANGE.retainDays.min}
        max={HEALTH_RANGE.retainDays.max}
        value={form.retainDays}
        onChange={(v) => onPatch({ retainDays: v })}
      />

      {applied ? (
        <div className={styles.metaRow}>
          <span className={styles.metaKey}>{t('settings:activity.appliedConfig')}</span>
          <span className={styles.metaValue}>
            {applied.recordWindowTitle
              ? t('health:recordWindowTitle')
              : t('settings:sync.disabled')}
            {` · ${applied.retainDays}d`}
          </span>
        </div>
      ) : null}

      <div className={styles.aboutActions}>
        <Button
          variant="ghost"
          size="md"
          onClick={onResetDefaults}
          disabled={applying || !canResetDefaults}
          title={canResetDefaults ? undefined : t('settings:resource.defaultsUnavailable')}
        >
          {t('settings:action.resetDefault')}
        </Button>
        <Button variant="primary" size="md" onClick={onApply} disabled={applying}>
          {applying ? t('settings:action.applying') : t('settings:action.apply')}
        </Button>
      </div>

      {error ? <span className={styles.updateError}>{error}</span> : null}
    </HealthSection>
  );
}
