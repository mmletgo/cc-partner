/**
 * SettingsBatteryPanel — Settings「充电模式」tab（自挂 hook）。
 *
 * Business Logic（为什么需要这个组件）:
 *   额度数字不进 Settings 11 端点合同；模式开关与 footer 共用权威快照。
 *
 * Code Logic（这个组件做什么）:
 *   自挂 useBattery + 读/写 battery 配置与流水；safe-save 用 editVersion + requestSeq。
 */

import { useCallback, useEffect, useRef, useState, type ChangeEvent, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';

import { batteryApi } from '@/api/battery';
import { Button, Card, Input, StatusMessage } from '@/components/primitives';
import { useBattery } from '@/hooks/useBattery';
import { formatBatteryTime } from '@/lib/batteryTime';
import {
  createSaveAttempt,
  resolveSaveFailure,
  resolveSaveSuccess,
} from '@/lib/asyncState/saveAttempt';
import {
  DEFAULT_BATTERY_CONFIG,
  type BatteryConfig,
  type BatteryLedgerItem,
  type BatteryLedgerKind,
  type BatteryMode,
} from '@/lib/types/battery';
import styles from './Settings.module.css';

const LEDGER_KIND_KEYS = {
  credit_health: 'settings.kinds.credit_health',
  credit_wordgame: 'settings.kinds.credit_wordgame',
  credit_game_plugin: 'settings.kinds.credit_game_plugin',
  daily_reset: 'settings.kinds.daily_reset',
  debit_tick: 'settings.kinds.debit_tick',
  mode_change: 'settings.kinds.mode_change',
} as const satisfies Record<BatteryLedgerKind, string>;

function cloneConfig(config: BatteryConfig): BatteryConfig {
  return {
    flashcardMinutes: config.flashcardMinutes,
    flashcardCap: config.flashcardCap,
    maxBalanceMinutes: config.maxBalanceMinutes,
  };
}

function configsEqual(a: BatteryConfig, b: BatteryConfig): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function clampInt(raw: number, min: number, max: number): number {
  if (!Number.isFinite(raw)) return min;
  return Math.min(max, Math.max(min, Math.round(raw)));
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户要在设置里改模式、看剩余、改闪卡额度与余额上限、看流水。健康额度在提醒模板上。
 *
 * Code Logic（这个组件做什么）:
 *   见文件头。hooks 全部在 early return 之前。
 */
export function SettingsBatteryPanel(): ReactElement {
  const { t } = useTranslation('battery');
  const { snapshot, setMode } = useBattery();
  const [draft, setDraft] = useState<BatteryConfig>(DEFAULT_BATTERY_CONFIG);
  const [baseline, setBaseline] = useState<BatteryConfig>(DEFAULT_BATTERY_CONFIG);
  const [configLoading, setConfigLoading] = useState(true);
  const [configError, setConfigError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [ledger, setLedger] = useState<BatteryLedgerItem[]>([]);
  const [ledgerError, setLedgerError] = useState<string | null>(null);
  const editVersion = useRef(0);
  const requestSeq = useRef(0);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [config, rows] = await Promise.all([
          batteryApi.getConfig(),
          batteryApi.listLedger(40),
        ]);
        if (cancelled) return;
        setDraft(cloneConfig(config));
        setBaseline(cloneConfig(config));
        setLedger(rows);
        setConfigError(null);
        setLedgerError(null);
      } catch (reason) {
        if (cancelled) return;
        setConfigError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        if (!cancelled) setConfigLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const dirty = !configsEqual(draft, baseline);

  const patchDraft = useCallback((next: BatteryConfig): void => {
    editVersion.current += 1;
    setDraft(next);
    setSaved(false);
    setSaveError(null);
  }, []);

  const handleMode = useCallback(
    (mode: BatteryMode): void => {
      void setMode(mode);
    },
    [setMode],
  );

  const handleSave = useCallback(async (): Promise<void> => {
    const next: BatteryConfig = {
      flashcardMinutes: clampInt(draft.flashcardMinutes, 0, 180),
      flashcardCap: clampInt(draft.flashcardCap, 0, 99),
      maxBalanceMinutes: clampInt(draft.maxBalanceMinutes, 30, 720),
    };
    requestSeq.current += 1;
    const attempt = createSaveAttempt(requestSeq.current, next, editVersion.current);
    setSaving(true);
    setSaveError(null);
    try {
      const savedConfig = await batteryApi.updateConfig(next);
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: requestSeq.current,
        currentDraft: draft,
        currentEditVersion: editVersion.current,
        serverValue: savedConfig,
        currentBaseline: baseline,
      });
      if (!resolution.applied) return;
      setBaseline(cloneConfig(resolution.baseline));
      setDraft(cloneConfig(resolution.draft));
      if (!resolution.dirty) setSaved(true);
    } catch (reason) {
      const resolution = resolveSaveFailure({
        attempt,
        currentRequestSeq: requestSeq.current,
        currentDraft: draft,
        currentBaseline: baseline,
      });
      if (!resolution.applied) return;
      setSaveError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSaving(false);
    }
  }, [baseline, draft]);

  const handleResetDefaults = useCallback(async (): Promise<void> => {
    try {
      const defaults = await batteryApi.getDefaultConfig();
      patchDraft(cloneConfig(defaults));
    } catch (reason) {
      setSaveError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [patchDraft]);

  const remainingLabel = formatBatteryTime(snapshot?.remainingMs ?? 0, t);
  const earnedLabel = formatBatteryTime(snapshot?.todayEarnedMs ?? 0, t);
  const spentLabel = formatBatteryTime(snapshot?.todaySpentMs ?? 0, t);

  return (
    <div className={styles.tabPanel} data-testid="settings-panel-battery-inner">
      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings.modeLabel')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          <div className={styles.toggleList}>
            <button
              type="button"
              className={styles.toggleRow}
              role="switch"
              aria-checked={snapshot?.mode === 'charging'}
              data-testid="settings-battery-mode"
              onClick={() =>
                handleMode(snapshot?.mode === 'charging' ? 'unlimited' : 'charging')
              }
            >
              <span>
                <span className={styles.label}>
                  {snapshot?.mode === 'unlimited'
                    ? t('settings.modeUnlimited')
                    : t('settings.modeCharging')}
                </span>
                <p className={styles.helper}>
                  {t('settings.remaining')} · {remainingLabel}
                </p>
              </span>
            </button>
          </div>
          <p className={styles.helper}>
            {t('settings.todayEarned')}: {earnedLabel}
            {' · '}
            {t('settings.todaySpent')}: {spentLabel}
          </p>
        </Card.Body>
      </Card>

      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings.rewardsTitle')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          {configLoading ? (
            <p className={styles.helper}>{t('loading')}</p>
          ) : null}
          {configError ? (
            <StatusMessage tone="danger">{configError}</StatusMessage>
          ) : (
            <div className={styles.healthFieldGrid}>
              <NumberField
                label={t('settings.flashcardMinutes')}
                value={draft.flashcardMinutes}
                min={0}
                max={180}
                onChange={(flashcardMinutes) =>
                  patchDraft({ ...draft, flashcardMinutes })
                }
              />
              <NumberField
                label={t('settings.flashcardCap')}
                value={draft.flashcardCap}
                min={0}
                max={99}
                onChange={(flashcardCap) =>
                  patchDraft({ ...draft, flashcardCap })
                }
              />
              <NumberField
                label={t('settings.maxBalance')}
                value={draft.maxBalanceMinutes}
                min={30}
                max={720}
                onChange={(maxBalanceMinutes) =>
                  patchDraft({ ...draft, maxBalanceMinutes })
                }
              />
            </div>
          )}
          <div className={styles.inputRow} style={{ marginTop: 'var(--space-5)' }}>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void handleResetDefaults()}
              disabled={saving || configLoading}
            >
              {t('settings.resetDefaults')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={() => void handleSave()}
              loading={saving}
              disabled={!dirty || saving || configLoading}
            >
              {saving ? t('settings.saving') : t('settings.save')}
            </Button>
          </div>
          {saved ? (
            <StatusMessage tone="success">{t('settings.saved')}</StatusMessage>
          ) : null}
          {saveError ? (
            <StatusMessage tone="danger">
              {t('settings.saveError', { error: saveError })}
            </StatusMessage>
          ) : null}
        </Card.Body>
      </Card>

      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings.ledgerTitle')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          {ledgerError ? (
            <StatusMessage tone="danger">{t('settings.ledgerLoadError')}</StatusMessage>
          ) : null}
          {ledger.length === 0 && !ledgerError ? (
            <p className={styles.helper}>{t('settings.ledgerEmpty')}</p>
          ) : (
            <ul className={styles.reminderList}>
              {ledger.map((row) => (
                <li key={row.id} className={styles.helper}>
                  {t(LEDGER_KIND_KEYS[row.kind])}
                  {' · '}
                  {row.deltaMs >= 0 ? '+' : ''}
                  {Math.round(row.deltaMs / 60_000)}
                </li>
              ))}
            </ul>
          )}
        </Card.Body>
      </Card>
    </div>
  );
}

SettingsBatteryPanel.displayName = 'SettingsBatteryPanel';

interface NumberFieldProps {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (next: number) => void;
}

function NumberField({ label, value, min, max, onChange }: NumberFieldProps): ReactElement {
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
    </div>
  );
}
