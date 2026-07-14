/**
 * Settings 常规设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在常规 tab 调整设备名、接收目录与截图快捷键；编排与持久化由 controller 负责，
 *   本组件只负责受控表单展示与交互触发。
 *
 * Code Logic（这个组件做什么）:
 *   渲染基本设置 Card、快捷键 Card 与 footer 保存/恢复默认；无 API 调用，无业务副作用状态。
 */
import type { ChangeEvent, KeyboardEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input } from '@/components/primitives';
import { DevicesIcon, FolderIcon, KeyboardIcon } from '@/lib/icons';
import { formatShortcutForDisplay } from './shortcutRecorder';
import type { SettingsState } from './settingsState';
import styles from './Settings.module.css';

/**
 * 常规面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层把 controller 状态透传给 pure panel，避免 panel 直接读 hook。
 *
 * Code Logic（这个接口做什么）:
 *   声明设备/目录/快捷键受控值与 save/reset/chooseDir 回调。
 */
export interface SettingsGeneralPanelProps {
  state: SettingsState;
  isDirty: boolean;
  savedAt: Date | null;
  saving: boolean;
  /** 最近一次保存失败文案；有值时在 footer 展示且不卸表单 */
  saveError: string | null;
  choosingDir: boolean;
  canResetCoreDefaults: boolean;
  recordingShortcutId: string | null;
  onDeviceNameChange: (e: ChangeEvent<HTMLInputElement>) => void;
  onReceiveDirChange: (e: ChangeEvent<HTMLInputElement>) => void;
  onChooseDir: () => void;
  onShortcutFocus: (id: string) => void;
  onShortcutBlur: (id: string) => void;
  onShortcutKeyDown: (e: KeyboardEvent<HTMLInputElement>, id: string) => void;
  onResetDefaults: () => void;
  onSave: () => void;
}

/**
 * 常规设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   常规偏好是 Settings 默认 tab，需要独立 pure 视图以便控制器拆分与静态 ownership 守卫。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 置顶；原样渲染基本设置、快捷键与 footer 按钮组。
 *
 * @param props 受控表单与动作
 * @returns 常规 tab 内容
 */

/**
 * 把 Date 格式化为 "HH:MM:SS" 字符串
 *
 * Business Logic（为什么需要这个函数）:
 *   常规 tab 保存成功后需在页脚展示本地保存时间。
 *
 * Code Logic（这个函数做什么）:
 *   从 Date 取本地时分秒并零填充。
 *
 * @param d Date 实例
 * @returns 时间字符串
 */
function formatTime(d: Date): string {
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function SettingsGeneralPanel({
  state,
  isDirty,
  savedAt,
  saving,
  saveError,
  choosingDir,
  canResetCoreDefaults,
  recordingShortcutId,
  onDeviceNameChange,
  onReceiveDirChange,
  onChooseDir,
  onShortcutFocus,
  onShortcutBlur,
  onShortcutKeyDown,
  onResetDefaults,
  onSave,
}: SettingsGeneralPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);

  return (
    <>
{/* Card 1: 基本设置 */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:basic.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    <div className={styles.field}>
      <label className={styles.label} htmlFor="settings-device-name">
        {t('settings:basic.deviceName')}
      </label>
      <div className={styles.inputRow}>
        <Input
          id="settings-device-name"
          type="text"
          value={state.deviceName}
          onChange={onDeviceNameChange}
          icon={<DevicesIcon />}
        />
      </div>
      <p className={styles.helper}>{t('settings:basic.deviceNameHelper')}</p>
    </div>

    <div className={styles.field}>
      <label className={styles.label} htmlFor="settings-receive-dir">
        {t('settings:basic.receiveDir')}
      </label>
      <div className={styles.inputRow}>
        <Input
          id="settings-receive-dir"
          type="text"
          value={state.receiveDir}
          onChange={onReceiveDirChange}
          icon={<FolderIcon />}
        />
        <Button variant="secondary" size="md" onClick={onChooseDir} disabled={choosingDir}>
          {choosingDir ? t('settings:basic.selecting') : t('settings:basic.selectFolder')}
        </Button>
      </div>
      <p className={styles.helper}>{t('settings:basic.receiveDirHelper')}</p>
    </div>
  </Card.Body>
</Card>

{/* Card 2: 快捷键 */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:shortcut.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    <div className={styles.shortcutList}>
      {state.shortcuts.map((s) => {
        const isRecording = recordingShortcutId === s.id;
        const label = t(`settings:shortcut.${s.labelKey}.label`);
        return (
          <div key={s.id} className={styles.shortcutRow}>
            <div className={styles.shortcutText}>
              <span className={styles.shortcutLabel}>{label}</span>
              <span className={styles.shortcutHelper}>
                {isRecording
                  ? t('settings:shortcut.recordingHelper')
                  : t(`settings:shortcut.${s.labelKey}.helper`)}
              </span>
            </div>
            <div className={styles.shortcutInput}>
              <Input
                id={`settings-shortcut-${s.id}`}
                type="text"
                value={isRecording ? t('settings:shortcut.recording') : formatShortcutForDisplay(s.value)}
                placeholder={t('settings:shortcut.placeholder')}
                onChange={() => undefined}
                onFocus={() => onShortcutFocus(s.id)}
                onClick={() => onShortcutFocus(s.id)}
                onBlur={() => onShortcutBlur(s.id)}
                onKeyDown={(e) => onShortcutKeyDown(e, s.id)}
                icon={<KeyboardIcon />}
                className={isRecording ? styles.shortcutRecorderActive : undefined}
                aria-label={label}
                readOnly
                mono
              />
            </div>
          </div>
        );
      })}
    </div>
  </Card.Body>
</Card>

{/* 底部按钮组：只保存常规 tab 的基础配置 */}
<div className={styles.footer}>
  <div className={styles.footerLeft}>
    {saveError ? (
      <span className={styles.updateError} role="alert">
        {t('settings:status.saveFailed')}: {saveError}
      </span>
    ) : null}
    {isDirty ? (
      <span className={styles.dirtyHint}>{t('settings:status.dirtyHint')}</span>
    ) : savedAt ? (
      <span className={styles.savedHint}>
        {t('settings:status.savedAt', { time: formatTime(savedAt) })}
      </span>
    ) : null}
  </div>
  <div className={styles.footerActions}>
    <Button
      variant="ghost"
      onClick={onResetDefaults}
      disabled={!canResetCoreDefaults}
      title={
        canResetCoreDefaults ? undefined : t('settings:resource.defaultsUnavailable')
      }
    >
      {t('settings:action.resetDefault')}
    </Button>
    <Button variant="primary" onClick={onSave} disabled={!isDirty || saving}>
      {saving ? t('settings:action.applying') : t('settings:action.apply')}
    </Button>
  </div>
</div>
    </>
  );
}

SettingsGeneralPanel.displayName = 'SettingsGeneralPanel';
