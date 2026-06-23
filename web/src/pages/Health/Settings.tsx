/**
 * Health 配置表单 - 完整受控表单,覆盖 HealthConfig 全部字段
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在「健康提醒」页需要调整久坐监测的全部参数:工作窗口/休息判定时长、
 *   通知/全屏遮罩/记录窗口标题/喝水提醒开关、喝水间隔、免打扰时段、明细保留天数、总开关。
 *   后端 `update_health_config` 是整体覆盖式回写,故表单每次变更必须提交完整对象
 *   (当前完整 config + 本次改动),否则未传字段(waterEnabled/reminderFullscreen 等)
 *   会被清零——这是「当前 cfg + patch」模式的核心目的。
 *
 * Code Logic（这个组件做什么）:
 *   - mount 时 getConfig 拉取当前完整配置初始化受控表单
 *   - update(patch):setCfg({...cfg, ...patch}) + updateConfig(完整对象),乐观更新
 *   - 分钟输入:workWindowSeconds/breakSeconds/waterIntervalSeconds ↔ 分钟整数双向换算(×60)
 *   - dndStart/dndEnd 用 HH:MM 文本草稿输入,失焦/回车时校验提交,空串 ↔ null
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { ChangeEvent } from 'react';
import { Card, Input } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { HealthConfig } from '@/lib/types';
import styles from './Settings.module.css';

interface ToggleFieldProps {
  /** 字段标题 */
  label: string;
  /** 字段说明 */
  description: string;
  /** 当前开关状态 */
  checked: boolean;
  /** 状态变更回调 */
  onChange: (checked: boolean) => void;
}

interface NumberFieldProps {
  /** 字段标题 */
  label: string;
  /** 字段说明 */
  description: string;
  /** 当前数字值 */
  value: number;
  /** 最小值 */
  min: number;
  /** 最大值 */
  max?: number;
  /** 数字变更回调 */
  onChange: (value: number) => void;
}

interface TimeFieldProps {
  /** 字段标题 */
  label: string;
  /** 当前时间值，null 表示未设置 */
  value: string | null;
  /** 时间变更回调 */
  onChange: (value: string | null) => void;
}

/** HH:MM 时间格式校验,24 小时制 */
const TIME_PATTERN = /^([01]\d|2[0-3]):[0-5]\d$/;

/**
 * 归一化免打扰时间草稿
 *
 * Business Logic（为什么需要这个函数）:
 *   原生 `input type="time"` 在空值时逐字符输入不会形成有效 value,用户会感觉免打扰时间无法调整。
 *   这里支持用户输入 `09:30`、`9:30`、`0930`、`930`,并在提交时统一成 `HH:MM`。
 *
 * Code Logic（这个函数做什么）:
 *   空串返回 null;合法 HH:MM 原样/补零返回;纯数字 3/4 位按小时分钟解析;
 *   不合法返回 undefined,由调用方决定回滚显示值。
 */
const normalizeTimeDraft = (draft: string): string | null | undefined => {
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
};

/**
 * 渲染设置页开关行
 *
 * Business Logic（为什么需要这个函数）:
 *   健康配置中大部分布尔项都是“标题 + 说明 + 开关”的同构结构,复用行组件可让配置表单
 *   与项目设置页风格统一。
 *
 * Code Logic（这个函数做什么）:
 *   接收 label/description/checked/onChange,渲染可点击 label 和自定义 switch。
 */
function ToggleField(props: ToggleFieldProps) {
  const { label, description, checked, onChange } = props;

  return (
    <label className={styles.toggleRow}>
      <span className={styles.fieldCopy}>
        <span className={styles.labelText}>{label}</span>
        <span className={styles.description}>{description}</span>
      </span>
      <span className={styles.switch} data-checked={checked || undefined}>
        <input
          type="checkbox"
          checked={checked}
          onChange={(event: ChangeEvent<HTMLInputElement>) => onChange(event.target.checked)}
        />
        <span className={styles.switchTrack} aria-hidden="true">
          <span className={styles.switchThumb} />
        </span>
      </span>
    </label>
  );
}

/**
 * 渲染分钟/天数数字配置行
 *
 * Business Logic（为什么需要这个函数）:
 *   工作窗口、休息判定、喝水间隔和保留天数都是数字配置,需要保持一致输入宽度与说明层级。
 *
 * Code Logic（这个函数做什么）:
 *   使用 primitives/Input 渲染 number 输入,把原生 change 事件转换为 number 回调。
 */
function NumberField(props: NumberFieldProps) {
  const { label, description, value, min, max, onChange } = props;

  return (
    <label className={styles.compactRow}>
      <span className={styles.fieldCopy}>
        <span className={styles.labelText}>{label}</span>
        <span className={styles.description}>{description}</span>
      </span>
      <Input
        type="number"
        size="sm"
        mono
        min={min}
        max={max}
        value={value}
        className={styles.compactInput}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </label>
  );
}

/**
 * 渲染免打扰时间配置行
 *
 * Business Logic（为什么需要这个函数）:
 *   免打扰起止时间可以为空,用户需要能直接清空或选择本地时间。
 *
 * Code Logic（这个函数做什么）:
 *   渲染 HH:MM 文本草稿输入,失焦/回车时提交;空串转换为 null,保持后端 HealthConfig 语义。
 */
function TimeField(props: TimeFieldProps) {
  const { label, value, onChange } = props;
  const [draft, setDraft] = useState(value ?? '');

  /**
   * 提交当前草稿值
   *
   * Business Logic（为什么需要这个函数）:
   *   用户输入免打扰时间时需要先完成 `HH:MM` 草稿,不能每个按键都立即整体回写配置。
   *
   * Code Logic（这个函数做什么）:
   *   调 normalizeTimeDraft 校验并归一化;合法则同步显示并触发 onChange,非法则回滚到最近有效值。
   */
  const commitDraft = () => {
    const next = normalizeTimeDraft(draft);
    if (next === undefined) {
      setDraft(value ?? '');
      return;
    }
    setDraft(next ?? '');
    if (next !== value) {
      onChange(next);
    }
  };

  return (
    <label className={styles.timeRow}>
      <span className={styles.labelText}>{label}</span>
      <Input
        type="text"
        size="sm"
        mono
        inputMode="numeric"
        placeholder="HH:MM"
        value={draft}
        className={styles.timeInput}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commitDraft}
        onKeyDown={(event) => {
          if (event.key === 'Enter') {
            event.currentTarget.blur();
          }
        }}
      />
    </label>
  );
}

/**
 * Health 配置表单组件
 *
 * @returns 渲染工作窗口/休息/通知/全屏/记录标题/喝水/免打扰/保留天数 全部字段的受控表单
 */
export function Settings() {
  const { t } = useTranslation(['health', 'common']);
  const [cfg, setCfg] = useState<HealthConfig | null>(null);

  useEffect(() => {
    void healthApi.getConfig().then(setCfg);
  }, []);

  /**
   * 提交一次配置变更:用「当前完整 cfg + 本次 patch」合成新对象,
   * 乐观更新本地状态后再整体回写后端,确保未变更字段不被清零。
   * 后端回写失败时回滚到 prev 并记录错误,避免本地状态与后端不一致。
   */
  const update = useCallback(async (patch: Partial<HealthConfig>) => {
    if (!cfg) return;
    const prev = cfg;
    const next = { ...cfg, ...patch };
    setCfg(next);
    try {
      await healthApi.updateConfig(next);
    } catch (e) {
      console.error('update_health_config failed, rolling back', e);
      setCfg(prev);
    }
  }, [cfg]);

  // hooks 已在 early return 之前调用完毕(规则 20),下方可安全 early return
  if (!cfg) return null;

  return (
    <Card variant="outlined" padding="md" className={styles.settingsCard}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <h2 className={styles.subtitle}>{t('health:settingsTitle')}</h2>
          <p className={styles.lead}>{t('health:settingsLead')}</p>
        </div>
      </Card.Header>

      <Card.Body className={styles.body}>
        <section className={styles.group}>
          <h3 className={styles.groupTitle}>{t('health:monitoringGroup')}</h3>
          <ToggleField
            label={t('health:enabled')}
            description={t('health:enabledDescription')}
            checked={cfg.enabled}
            onChange={(checked) => { void update({ enabled: checked }); }}
          />
          <NumberField
            label={t('health:workWindowMinutes')}
            description={t('health:workWindowDescription')}
            min={1}
            max={120}
            value={Math.round(cfg.workWindowSeconds / 60)}
            onChange={(value) => { void update({ workWindowSeconds: value * 60 }); }}
          />
          <NumberField
            label={t('health:breakMinutes')}
            description={t('health:breakDescription')}
            min={1}
            value={Math.round(cfg.breakSeconds / 60)}
            onChange={(value) => { void update({ breakSeconds: value * 60 }); }}
          />
        </section>

        <section className={styles.group}>
          <h3 className={styles.groupTitle}>{t('health:reminderGroup')}</h3>
          <ToggleField
            label={t('health:notifyEnabled')}
            description={t('health:notifyDescription')}
            checked={cfg.notifyEnabled}
            onChange={(checked) => { void update({ notifyEnabled: checked }); }}
          />
          <ToggleField
            label={t('health:reminderFullscreen')}
            description={t('health:fullscreenDescription')}
            checked={cfg.reminderFullscreen}
            onChange={(checked) => { void update({ reminderFullscreen: checked }); }}
          />
          <ToggleField
            label={t('health:waterEnabled')}
            description={t('health:waterDescription')}
            checked={cfg.waterEnabled}
            onChange={(checked) => { void update({ waterEnabled: checked }); }}
          />
          <NumberField
            label={t('health:waterIntervalMinutes')}
            description={t('health:waterIntervalDescription')}
            min={1}
            value={Math.round(cfg.waterIntervalSeconds / 60)}
            onChange={(value) => { void update({ waterIntervalSeconds: value * 60 }); }}
          />
        </section>

        <section className={styles.group}>
          <h3 className={styles.groupTitle}>{t('health:quietHoursGroup')}</h3>
          <div className={styles.twoColumn}>
            <TimeField
              key={`dnd-start-${cfg.dndStart ?? 'empty'}`}
              label={t('health:dndStart')}
              value={cfg.dndStart}
              onChange={(value) => { void update({ dndStart: value }); }}
            />
            <TimeField
              key={`dnd-end-${cfg.dndEnd ?? 'empty'}`}
              label={t('health:dndEnd')}
              value={cfg.dndEnd}
              onChange={(value) => { void update({ dndEnd: value }); }}
            />
          </div>
        </section>

        <section className={styles.group}>
          <h3 className={styles.groupTitle}>{t('health:privacyGroup')}</h3>
          <ToggleField
            label={t('health:recordWindowTitle')}
            description={t('health:recordWindowTitleDescription')}
            checked={cfg.recordWindowTitle}
            onChange={(checked) => { void update({ recordWindowTitle: checked }); }}
          />
          <NumberField
            label={t('health:retainDays')}
            description={t('health:retainDaysDescription')}
            min={1}
            value={cfg.retainDays}
            onChange={(value) => { void update({ retainDays: value }); }}
          />
        </section>
      </Card.Body>
    </Card>
  );
}

Settings.displayName = 'HealthSettings';
