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
 *   - dndStart/dndEnd 用 `<input type="time">`,空串 ↔ null
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { HealthConfig } from '@/lib/types';
import styles from './Settings.module.css';

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

  // hooks 已在 early return 之前调用完毕(规则 20),下方可安全 early return
  if (!cfg) return null;

  /**
   * 提交一次配置变更:用「当前完整 cfg + 本次 patch」合成新对象,
   * 乐观更新本地状态后再整体回写后端,确保未变更字段不被清零。
   */
  const update = async (patch: Partial<HealthConfig>) => {
    const next = { ...cfg, ...patch };
    setCfg(next);
    await healthApi.updateConfig(next);
  };

  return (
    <Card variant="outlined" padding="md" className={styles.section}>
      <h3 className={styles.subtitle}>{t('health:settingsTitle')}</h3>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:enabled')}</span>
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={cfg.enabled}
          onChange={(e) => update({ enabled: e.target.checked })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:workWindowMinutes')}</span>
        <input
          type="number"
          min={1}
          max={120}
          className={styles.numberInput}
          value={Math.round(cfg.workWindowSeconds / 60)}
          onChange={(e) => update({ workWindowSeconds: Number(e.target.value) * 60 })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:breakMinutes')}</span>
        <input
          type="number"
          min={1}
          className={styles.numberInput}
          value={Math.round(cfg.breakSeconds / 60)}
          onChange={(e) => update({ breakSeconds: Number(e.target.value) * 60 })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:notifyEnabled')}</span>
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={cfg.notifyEnabled}
          onChange={(e) => update({ notifyEnabled: e.target.checked })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:reminderFullscreen')}</span>
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={cfg.reminderFullscreen}
          onChange={(e) => update({ reminderFullscreen: e.target.checked })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:recordWindowTitle')}</span>
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={cfg.recordWindowTitle}
          onChange={(e) => update({ recordWindowTitle: e.target.checked })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:waterEnabled')}</span>
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={cfg.waterEnabled}
          onChange={(e) => update({ waterEnabled: e.target.checked })}
        />
      </label>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:waterIntervalMinutes')}</span>
        <input
          type="number"
          min={1}
          className={styles.numberInput}
          value={Math.round(cfg.waterIntervalSeconds / 60)}
          onChange={(e) => update({ waterIntervalSeconds: Number(e.target.value) * 60 })}
        />
      </label>

      <div className={styles.dndRow}>
        <label className={styles.field}>
          <span className={styles.labelText}>{t('health:dndStart')}</span>
          <input
            type="time"
            className={styles.timeInput}
            value={cfg.dndStart ?? ''}
            onChange={(e) => update({ dndStart: e.target.value || null })}
          />
        </label>
        <label className={styles.field}>
          <span className={styles.labelText}>{t('health:dndEnd')}</span>
          <input
            type="time"
            className={styles.timeInput}
            value={cfg.dndEnd ?? ''}
            onChange={(e) => update({ dndEnd: e.target.value || null })}
          />
        </label>
      </div>

      <label className={styles.field}>
        <span className={styles.labelText}>{t('health:retainDays')}</span>
        <input
          type="number"
          min={1}
          className={styles.numberInput}
          value={cfg.retainDays}
          onChange={(e) => update({ retainDays: Number(e.target.value) })}
        />
      </label>
    </Card>
  );
}

Settings.displayName = 'HealthSettings';
