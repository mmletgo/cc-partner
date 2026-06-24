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
 *   - ToggleRow/NumberRow/TimeRow 为私有受控小组件,onChange 只回传 patch,不落盘
 *   - 免打扰时间用本地草稿输入,失焦/回车经 normalizeTimeDraft 归一化提交(空串↔null)
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useState } from 'react';
import type { ChangeEvent, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon } from '@/lib/icons';
import type { HealthForm } from './settingsState';
import type { HealthConfig } from '@/lib/types';
import styles from './Settings.module.css';

/** HH:MM 24 小时制校验 */
const TIME_PATTERN = /^([01]\d|2[0-3]):[0-5]\d$/;

/**
 * 归一化免打扰时间草稿
 *
 * Business Logic（为什么需要这个函数）:
 *   免打扰时间需支持 `09:30`/`9:30`/`0930`/`930` 多种输入,提交时统一为 HH:MM;
 *   返回 null 表示清空(undefined 才是非法回滚信号),区分空与非法。
 *
 * Code Logic（这个函数做什么）:
 *   空串→null;合法 HH:MM 原样;带冒号或纯数字按位解析补零;不合法→undefined(回滚)。
 *
 * @param draft 用户输入的时间草稿字符串
 * @returns 归一化的 HH:MM | null(空) | undefined(非法)
 */
// eslint-disable-next-line react-refresh/only-export-components -- normalizeTimeDraft 是与 HealthPanel 同文件的纯工具函数,测试需直接 import;HMR 偶发失效可接受(参照 ScreenshotToolbar/Card 先例)
export function normalizeTimeDraft(draft: string): string | null | undefined {
  const trimmed = draft.trim();
  if (trimmed === '') return null;
  if (TIME_PATTERN.test(trimmed)) return trimmed;

  const colonMatch = trimmed.match(/^(\d{1,2}):(\d{2})$/);
  if (colonMatch) {
    const hour = Number(colonMatch[1]);
    const minute = Number(colonMatch[2]);
    if (hour <= 23 && minute <= 59) {
      return `${String(hour).padStart(2, '0')}:${String(minute).padStart(2, '0')}`;
    }
    return undefined;
  }

  const digits = trimmed.replace(/\D/g, '');
  if (digits.length === 3 || digits.length === 4) {
    const splitAt = digits.length - 2;
    const hour = Number(digits.slice(0, splitAt));
    const minute = Number(digits.slice(splitAt));
    if (hour <= 23 && minute <= 59) {
      return `${String(hour).padStart(2, '0')}:${String(minute).padStart(2, '0')}`;
    }
  }

  return undefined;
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
 *   免打扰起止时间用 HH:MM 文本输入,需本地草稿(避免每次输入都提交非法中间态),
 *   失焦或回车时归一化为合法 HH:MM 或清空(null),非法则回滚草稿。
 *
 * Code Logic（这个组件做什么）:
 *   useState 维护 draft;commitDraft 调 normalizeTimeDraft,undefined 回滚、null/合法值落库并 onChange。
 *   useState 在函数体顶部,无 early return(项目规则 20)。
 */
function TimeRow({ label, value, onChange }: TimeRowProps) {
  const [draft, setDraft] = useState(value ?? '');

  const commitDraft = () => {
    const next = normalizeTimeDraft(draft);
    if (next === undefined) {
      setDraft(value ?? '');
      return;
    }
    setDraft(next ?? '');
    if (next !== value) onChange(next);
  };

  return (
    <div className={styles.field}>
      <label className={styles.label}>{label}</label>
      <Input
        type="text"
        size="sm"
        mono
        inputMode="numeric"
        placeholder="HH:MM"
        value={draft}
        onChange={(e: ChangeEvent<HTMLInputElement>) => setDraft(e.target.value)}
        onBlur={commitDraft}
        onKeyDown={(e) => {
          if (e.key === 'Enter') e.currentTarget.blur();
        }}
      />
    </div>
  );
}

/**
 * 分组标题
 *
 * Business Logic（为什么需要这个组件）:
 *   健康配置按「监测/提醒/免打扰/隐私」分组,需统一分组标题样式。
 *
 * Code Logic（这个组件做什么）:
 *   复用 sectionTitle 样式的 h3 容器,透传 children。
 */
function GroupTitle({ children }: { children: ReactNode }) {
  return <h3 className={styles.sectionTitle}>{children}</h3>;
}

/**
 * 健康提醒设置面板组件
 *
 * Business Logic（为什么需要这个组件）:
 *   设置页健康 tab 的纯渲染入口,聚合监测/提醒/免打扰/隐私四组受控字段,
 *   底部提供「恢复默认」「应用配置」按钮 + 已应用配置快照 + 错误提示。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 在顶部(无 early return,项目规则 20);
 *   渲染 Card(Header+Body) 内四组字段,字段 onChange 经 onPatch 浅合并回传父组件。
 *
 * @returns 监测/提醒/免打扰/隐私 四组受控字段 + 恢复默认/应用配置按钮
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
    <Card variant="flat" padding="md">
      <Card.Header>
        <h2 className={styles.sectionTitle}>{t('settings:health.title')}</h2>
      </Card.Header>
      <Card.Body padding="md">
        <p className={styles.helper}>{t('settings:health.subtitle')}</p>

        {/* 监测 */}
        <GroupTitle>{t('health:monitoringGroup')}</GroupTitle>
        <div className={styles.toggleList}>
          <ToggleRow
            label={t('health:enabled')}
            helper={t('health:enabledDescription')}
            checked={form.enabled}
            onToggle={(v) => onPatch({ enabled: v })}
          />
        </div>
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

        {/* 提醒 */}
        <GroupTitle>{t('health:reminderGroup')}</GroupTitle>
        <div className={styles.toggleList}>
          <ToggleRow
            label={t('health:notifyEnabled')}
            helper={t('health:notifyDescription')}
            checked={form.notifyEnabled}
            onToggle={(v) => onPatch({ notifyEnabled: v })}
          />
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

        {/* 免打扰时段 */}
        <GroupTitle>{t('health:quietHoursGroup')}</GroupTitle>
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

        {/* 隐私 */}
        <GroupTitle>{t('health:privacyGroup')}</GroupTitle>
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
      </Card.Body>
    </Card>
  );
}

HealthPanel.displayName = 'HealthPanel';
