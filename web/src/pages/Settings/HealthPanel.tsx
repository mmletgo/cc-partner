/**
 * 健康提醒设置面板 - 设置页「健康提醒」tab 的纯渲染组件
 *
 * Business Logic（为什么需要这个组件）:
 *   健康提醒配置从 Health 监控页迁移到设置页;用户在此表单编辑久坐监测的全部参数
 *   (工作/休息阈值、提醒方式、喝水、免打扰、隐私),通过「恢复默认」「应用配置」提交,
 *   与同步/AI tab 的表单编辑 + 手动应用模式一致。本组件只负责渲染,状态由 Settings.tsx 顶层持有。
 *
 * Code Logic（这个组件做什么）:
 *   - 复用设置页通用样式(field/label/helper/toggleList/toggleRow/Pill/Input)保证视觉统一
 *   - 每个健康配置栏目独立渲染为 section + Card,与设置页常规 tab 的多 Card 节奏一致
 *   - ToggleRow/NumberRow/TimeRow 为私有受控小组件,onChange 只回传 patch,不落盘
 *   - 免打扰时间用 24 小时制小时/分钟下拉框选择,空小时回传 null
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import type { ChangeEvent, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon } from '@/lib/icons';
import type { HealthForm } from './settingsState';
import type { HealthConfig } from '@/lib/types';
import styles from './Settings.module.css';

/** 24 小时制小时选项 */
const HOUR_OPTIONS = Array.from({ length: 24 }, (_, i) => String(i).padStart(2, '0'));

/** 分钟选项 */
const MINUTE_OPTIONS = Array.from({ length: 60 }, (_, i) => String(i).padStart(2, '0'));

interface TimeParts {
  hour: string;
  minute: string;
}

/**
 * 拆分配置时间为小时和分钟
 *
 * Business Logic（为什么需要这个函数）:
 *   免打扰时间在配置中仍保存为 HH:MM 或 null,但 UI 需要分别渲染 24 小时制小时/分钟选择器。
 *
 * Code Logic（这个函数做什么）:
 *   null 或格式不完整时返回空小时/分钟;合法 HH:MM 按冒号拆成 hour/minute。
 *
 * @param value 配置中的 HH:MM 字符串或 null
 * @returns 拆分后的小时和分钟
 */
// eslint-disable-next-line react-refresh/only-export-components -- splitTimeValue 是与 HealthPanel 同文件的纯工具函数,测试需直接 import;HMR 偶发失效可接受(参照 ScreenshotToolbar/Card 先例)
export function splitTimeValue(value: string | null): TimeParts {
  if (!value) return { hour: '', minute: '' };
  const [hour, minute] = value.split(':');
  if (!hour || !minute) return { hour: '', minute: '' };
  return { hour, minute };
}

/**
 * 合成小时/分钟选择为配置时间
 *
 * Business Logic（为什么需要这个函数）:
 *   用户通过两个下拉框选择免打扰时间,清空小时表示关闭该端点;后端仍需要 HH:MM 或 null。
 *
 * Code Logic（这个函数做什么）:
 *   hour 为空时返回 null;hour 有值时用所选 minute 或默认 00 拼成 HH:MM。
 *
 * @param hour 00-23 小时字符串或空
 * @param minute 00-59 分钟字符串或空
 * @returns HH:MM 字符串或 null
 */
// eslint-disable-next-line react-refresh/only-export-components -- timePartsToConfig 是与 HealthPanel 同文件的纯工具函数,测试需直接 import;HMR 偶发失效可接受(参照 ScreenshotToolbar/Card 先例)
export function timePartsToConfig(hour: string, minute: string): string | null {
  if (hour === '') return null;
  return `${hour}:${minute || '00'}`;
}

interface HealthPanelProps {
  /** 当前表单值 */
  form: HealthForm;
  /** 最近已应用配置快照(显示用) */
  applied: HealthConfig | null;
  /** 字段变更(浅合并,只改本地表单) */
  onPatch: (partial: Partial<HealthForm>) => void;
  /** 恢复默认 */
  onResetDefaults: () => void;
  /** 应用配置(整体提交) */
  onApply: () => void;
  /** 应用中 */
  applying: boolean;
  /** 错误提示 */
  error: string | null;
}

interface ToggleRowProps {
  label: string;
  helper: string;
  checked: boolean;
  onToggle: (next: boolean) => void;
}

interface NumberRowProps {
  label: string;
  helper: string;
  value: number;
  min: number;
  max?: number;
  onChange: (next: number) => void;
}

interface TimeRowProps {
  label: string;
  value: string | null;
  onChange: (next: string | null) => void;
}

interface HealthSectionProps {
  id: string;
  title: string;
  lead?: string;
  children: ReactNode;
}

/**
 * 渲染开关行
 *
 * Business Logic（为什么需要这个组件）:
 *   健康配置的布尔项(监测开关/通知/全屏/喝水/记录窗口标题)需要统一的开关交互,
 *   复用设置页 toggleRow + Pill 视觉,与同步/AI tab 一致。
 *
 * Code Logic（这个组件做什么）:
 *   受控 button(role=switch),点击 onToggle 取反;checked 用 success/neutral Pill + 图标表达状态。
 */
function ToggleRow({ label, helper, checked, onToggle }: ToggleRowProps) {
  return (
    <button
      type="button"
      className={styles.toggleRow}
      onClick={() => onToggle(!checked)}
      role="switch"
      aria-checked={checked}
      aria-label={label}
    >
      <div className={styles.toggleText}>
        <span className={styles.toggleLabel}>{label}</span>
        <span className={styles.toggleHelper}>{helper}</span>
      </div>
      <span className={styles.toggleState}>
        {checked ? (
          <Pill tone="success" dot>
            <CheckIcon size={12} />
          </Pill>
        ) : (
          <Pill tone="neutral" dot>
            <XIcon size={12} />
          </Pill>
        )}
      </span>
    </button>
  );
}

/**
 * 渲染数字配置行
 *
 * Business Logic（为什么需要这个组件）:
 *   工作窗口/休息/喝水间隔/保留天数等数字阈值需统一表单布局,复用设置页 field + label + Input + helper。
 *
 * Code Logic（这个组件做什么）:
 *   受控 number Input,onChange 把字符串转 Number 回传;min/max 约束输入范围。
 */
function NumberRow({ label, helper, value, min, max, onChange }: NumberRowProps) {
  return (
    <div className={styles.field}>
      <label className={styles.label}>{label}</label>
      <Input
        type="number"
        mono
        min={min}
        max={max}
        value={value}
        onChange={(e: ChangeEvent<HTMLInputElement>) => onChange(Number(e.target.value))}
      />
      <p className={styles.helper}>{helper}</p>
    </div>
  );
}

/**
 * 渲染免打扰时间行
 *
 * Business Logic（为什么需要这个组件）:
 *   免打扰起止时间应由用户从 24 小时制下拉选择器中选择,减少手动输入格式错误,
 *   且不受系统 12/24 小时显示偏好影响。
 *
 * Code Logic（这个组件做什么）:
 *   将 value 拆成 hour/minute;小时选择空值时 onChange(null),分钟在未选择小时时禁用。
 */
function TimeRow({ label, value, onChange }: TimeRowProps) {
  const { hour, minute } = splitTimeValue(value);

  return (
    <div className={styles.field}>
      <label className={styles.label}>{label}</label>
      <div className={styles.timePicker}>
        <select
          data-part="hour"
          className={styles.timeSelect}
          value={hour}
          onChange={(e: ChangeEvent<HTMLSelectElement>) => onChange(timePartsToConfig(e.target.value, minute))}
          aria-label={`${label} hour`}
        >
          <option value="">--</option>
          {HOUR_OPTIONS.map((option) => (
            <option key={option} value={option}>{option}</option>
          ))}
        </select>
        <span className={styles.timeSeparator}>:</span>
        <select
          data-part="minute"
          className={styles.timeSelect}
          value={minute}
          onChange={(e: ChangeEvent<HTMLSelectElement>) => onChange(timePartsToConfig(hour || '00', e.target.value))}
          aria-label={`${label} minute`}
          disabled={!hour}
        >
          <option value="">--</option>
          {MINUTE_OPTIONS.map((option) => (
            <option key={option} value={option}>{option}</option>
          ))}
        </select>
      </div>
    </div>
  );
}

/**
 * 健康设置栏目
 *
 * Business Logic（为什么需要这个组件）:
 *   健康提醒 tab 的字段较多,单一卡片连续堆叠会显得拥挤;按常规设置页的方式拆成
 *   「健康提醒 / 提醒方向 / 免打扰 / 通知与隐私」四个栏目,便于扫描和定位。
 *
 * Code Logic（这个组件做什么）:
 *   渲染语义化 section,内部复用 Card.Header/Card.Body;标题 id 与 aria-labelledby 关联。
 */
function HealthSection({ id, title, lead, children }: HealthSectionProps) {
  const titleId = `${id}-title`;

  return (
    <section className={styles.healthSection} aria-labelledby={titleId}>
      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 id={titleId} className={styles.sectionTitle}>{title}</h2>
        </Card.Header>
        <Card.Body padding="md">
          {lead ? <p className={styles.helper}>{lead}</p> : null}
          {children}
        </Card.Body>
      </Card>
    </section>
  );
}

/**
 * 健康提醒设置面板组件
 *
 * Business Logic（为什么需要这个组件）:
 *   设置页健康 tab 的纯渲染入口,聚合健康提醒/提醒方向/免打扰/通知与隐私四组受控字段,
 *   底部提供「恢复默认」「应用配置」按钮 + 已应用配置快照 + 错误提示。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 在顶部(无 early return,项目规则 20);
 *   渲染四个 section Card,字段 onChange 经 onPatch 浅合并回传父组件。
 *
 * @returns 健康提醒/提醒方向/免打扰/通知与隐私 四组受控字段 + 恢复默认/应用配置按钮
 */
export function HealthPanel({
  form,
  applied,
  onPatch,
  onResetDefaults,
  onApply,
  applying,
  error,
}: HealthPanelProps) {
  const { t } = useTranslation(['settings', 'health', 'common']);

  return (
    <>
      <HealthSection
        id="settings-health-reminder"
        title={t('health:monitoringGroup')}
        lead={t('settings:health.subtitle')}
      >
        <div className={styles.toggleList}>
          <ToggleRow
            label={t('health:enabled')}
            helper={t('health:enabledDescription')}
            checked={form.enabled}
            onToggle={(v) => onPatch({ enabled: v })}
          />
        </div>
        <div className={styles.healthFieldGrid}>
          <NumberRow
            label={t('health:workWindowMinutes')}
            helper={t('health:workWindowDescription')}
            min={1}
            max={120}
            value={Math.round(form.workWindowSeconds / 60)}
            onChange={(v) => onPatch({ workWindowSeconds: v * 60 })}
          />
          <NumberRow
            label={t('health:breakMinutes')}
            helper={t('health:breakDescription')}
            min={1}
            value={Math.round(form.breakSeconds / 60)}
            onChange={(v) => onPatch({ breakSeconds: v * 60 })}
          />
        </div>
      </HealthSection>

      <HealthSection id="settings-health-reminder-style" title={t('health:reminderGroup')}>
        <div className={styles.toggleList}>
          <ToggleRow
            label={t('health:reminderFullscreen')}
            helper={t('health:fullscreenDescription')}
            checked={form.reminderFullscreen}
            onToggle={(v) => onPatch({ reminderFullscreen: v })}
          />
          <ToggleRow
            label={t('health:waterEnabled')}
            helper={t('health:waterDescription')}
            checked={form.waterEnabled}
            onToggle={(v) => onPatch({ waterEnabled: v })}
          />
        </div>
        <NumberRow
          label={t('health:waterIntervalMinutes')}
          helper={t('health:waterIntervalDescription')}
          min={1}
          value={Math.round(form.waterIntervalSeconds / 60)}
          onChange={(v) => onPatch({ waterIntervalSeconds: v * 60 })}
        />
      </HealthSection>

      <HealthSection id="settings-health-quiet-hours" title={t('health:quietHoursGroup')}>
        <div className={styles.healthFieldGrid}>
          <TimeRow
            label={t('health:dndStart')}
            value={form.dndStart}
            onChange={(v) => onPatch({ dndStart: v })}
          />
          <TimeRow
            label={t('health:dndEnd')}
            value={form.dndEnd}
            onChange={(v) => onPatch({ dndEnd: v })}
          />
        </div>
      </HealthSection>

      <HealthSection id="settings-health-notification-privacy" title={t('health:privacyGroup')}>
        <div className={styles.toggleList}>
          <ToggleRow
            label={t('health:notifyEnabled')}
            helper={t('health:notifyDescription')}
            checked={form.notifyEnabled}
            onToggle={(v) => onPatch({ notifyEnabled: v })}
          />
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
          min={1}
          value={form.retainDays}
          onChange={(v) => onPatch({ retainDays: v })}
        />

        {/* 已应用配置快照 */}
        {applied ? (
          <div className={styles.metaRow}>
            <span className={styles.metaKey}>{t('settings:health.appliedConfig')}</span>
            <span className={styles.metaValue}>
              {applied.enabled ? t('settings:sync.enabled') : t('settings:sync.disabled')}
              {` · ${Math.round(applied.workWindowSeconds / 60)}m / ${Math.round(applied.breakSeconds / 60)}m`}
            </span>
          </div>
        ) : null}

        {/* 按钮组 */}
        <div className={styles.aboutActions}>
          <Button variant="ghost" size="md" onClick={onResetDefaults} disabled={applying}>
            {t('settings:action.resetDefault')}
          </Button>
          <Button variant="primary" size="md" onClick={onApply} disabled={applying}>
            {applying ? t('settings:action.applying') : t('settings:action.apply')}
          </Button>
        </div>

        {error ? <span className={styles.updateError}>{error}</span> : null}
      </HealthSection>
    </>
  );
}
