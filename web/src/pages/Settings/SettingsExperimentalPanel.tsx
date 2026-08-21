/**
 * Settings 内测功能面板
 *
 * Business Logic（为什么需要这个组件）:
 *   充电模式、游戏大厅、网页浏览、项目自动化与云端同步默认关闭；用户须在本页显式打开后
 *   才看到对应入口。各项设置集中在底部独立 tab，避免一开启就把表单堆在开关下方。
 *
 * Code Logic（这个组件做什么）:
 *   五个总开关 + 已开启且带设置的功能的底部 tablist/tabpanel；禁止 @/api transport。
 */
import type { ChangeEvent, KeyboardEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { CheckIcon, FolderIcon, XIcon } from '@/lib/icons';
import type { ExperimentalFeaturesConfig } from '@/lib/types/settings';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import { AutomationSettingsPanel } from './AutomationSettingsPanel';
import { SettingsBatteryPanel } from './SettingsBatteryPanel';
import {
  SettingsCloudSyncPanel,
  type SettingsCloudSyncPanelProps,
} from './SettingsCloudSyncPanel';
import type { AutomationSettingsForm } from './automationSettingsState';
import {
  EXPERIMENTAL_FEATURE_IDS,
  EXPERIMENTAL_SETTINGS_TAB_IDS,
  resolveExperimentalSettingsTab,
  type ExperimentalFeatureId,
} from './settingsState';
import styles from './Settings.module.css';

/**
 * 内测功能面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   壳层把总开关、底部设置 tab 选中态与各嵌套设置的受控值一次性交给纯视图。
 *
 * Code Logic（这个接口做什么）:
 *   features/onToggle 管 opt-in；onSelectFeatureTab 写 URL feature=；其余字段仅在对应 tab 渲染。
 */
export interface SettingsExperimentalPanelProps {
  features: ExperimentalFeaturesConfig;
  highlightedFeature: ExperimentalFeatureId | null;
  featureError: string | null;
  onToggleFeature: (id: ExperimentalFeatureId, enabled: boolean) => void;
  onSelectFeatureTab: (id: ExperimentalFeatureId) => void;
  gamePluginDir: string;
  choosingGamePluginDir: boolean;
  isDirty: boolean;
  saving: boolean;
  saveError: string | null;
  savedAt: Date | null;
  canResetCoreDefaults: boolean;
  onGamePluginDirChange: (e: ChangeEvent<HTMLInputElement>) => void;
  onChooseGamePluginDir: () => void;
  onResetDefaults: () => void;
  onSave: () => void;
  automationLoadError: Error | null;
  retryingAutomation: boolean;
  onRetryAutomation: () => void;
  automationForm: AutomationSettingsForm;
  defaultAutomationForm: AutomationSettingsForm;
  automationDirty: boolean;
  savingAutomation: boolean;
  automationError: string | null;
  automationSaved: boolean;
  onAutomationChange: (nextForm: AutomationSettingsForm) => void;
  onResetAutomationDefaults: () => void;
  onSaveAutomation: () => void;
  canResetAutomationDefaults: boolean;
  agentAdapters: OrchestratorAgentAdapterCatalogItem[] | undefined;
  onOpenOpenCodeBridgePreview: () => void;
  cloudSync: SettingsCloudSyncPanelProps;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   保存成功后游戏目录区块需展示本地时间。
 *
 * Code Logic（这个函数做什么）:
 *   从 Date 取本地时分秒并零填充。
 */
function formatTime(d: Date): string {
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   内测功能必须集中在一个 Settings tab；已开启功能的设置走底部 tab 切换，而不是纵向堆叠。
 *
 * Code Logic（这个组件做什么）:
 *   渲染五个 switch；按已开启且带设置的列表画 tablist，只挂载当前选中设置。充电/自动化保留原 panel id 供深链与 E2E。
 */
export function SettingsExperimentalPanel(
  props: SettingsExperimentalPanelProps,
): ReactElement {
  const { t } = useTranslation(['settings', 'common']);
  const {
    features,
    highlightedFeature,
    featureError,
    onToggleFeature,
    onSelectFeatureTab,
    gamePluginDir,
    choosingGamePluginDir,
    isDirty,
    saving,
    saveError,
    savedAt,
    canResetCoreDefaults,
    onGamePluginDirChange,
    onChooseGamePluginDir,
    onResetDefaults,
    onSave,
    automationLoadError,
    retryingAutomation,
    onRetryAutomation,
    automationForm,
    defaultAutomationForm,
    automationDirty,
    savingAutomation,
    automationError,
    automationSaved,
    onAutomationChange,
    onResetAutomationDefaults,
    onSaveAutomation,
    canResetAutomationDefaults,
    agentAdapters,
    onOpenOpenCodeBridgePreview,
    cloudSync,
  } = props;

  const enabledFeatures = EXPERIMENTAL_SETTINGS_TAB_IDS.filter((id) => features[id]);
  const activeFeature = resolveExperimentalSettingsTab(features, highlightedFeature);

  /**
   * Business Logic（为什么需要这个函数）:
   *   底部设置 tab 与顶层 Settings tab 一样需要方向键循环，避免鼠标才能切换。
   *
   * Code Logic（这个函数做什么）:
   *   Arrow/Home/End 在已开启列表上取下一 id，写 URL 并聚焦对应 tab。
   */
  const handleFeatureTabKeyDown = (
    e: KeyboardEvent<HTMLButtonElement>,
    currentIndex: number,
  ): void => {
    if (enabledFeatures.length === 0) return;
    let nextIndex: number | null = null;
    if (e.key === 'ArrowRight') {
      nextIndex = (currentIndex + 1) % enabledFeatures.length;
    } else if (e.key === 'ArrowLeft') {
      nextIndex = (currentIndex - 1 + enabledFeatures.length) % enabledFeatures.length;
    } else if (e.key === 'Home') {
      nextIndex = 0;
    } else if (e.key === 'End') {
      nextIndex = enabledFeatures.length - 1;
    }
    if (nextIndex === null) return;
    const nextId = enabledFeatures[nextIndex];
    if (!nextId) return;
    e.preventDefault();
    onSelectFeatureTab(nextId);
    window.requestAnimationFrame(() => {
      document.getElementById(`settings-experimental-tab-${nextId}`)?.focus();
    });
  };

  return (
    <>
      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings:experimental.title')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          <p className={styles.helper}>{t('settings:experimental.subtitle')}</p>
          <div className={styles.toggleList}>
            {EXPERIMENTAL_FEATURE_IDS.map((id) => {
              const checked = features[id];
              return (
                <button
                  key={id}
                  type="button"
                  className={styles.toggleRow}
                  onClick={() => onToggleFeature(id, !checked)}
                  role="switch"
                  aria-checked={checked}
                  aria-label={t(`settings:experimental.${id}.label`)}
                  data-testid={`settings-experimental-toggle-${id}`}
                >
                  <div className={styles.toggleText}>
                    <span className={styles.toggleLabel}>
                      {t(`settings:experimental.${id}.label`)}
                    </span>
                    <span className={styles.toggleHelper}>
                      {t(`settings:experimental.${id}.helper`)}
                    </span>
                  </div>
                  <span className={styles.toggleState}>
                    {checked ? (
                      <Pill tone="success" dot>
                        <CheckIcon size={12} />
                        {t('settings:sync.enabled')}
                      </Pill>
                    ) : (
                      <Pill tone="neutral" dot>
                        <XIcon size={12} />
                        {t('settings:sync.disabled')}
                      </Pill>
                    )}
                  </span>
                </button>
              );
            })}
          </div>
          {featureError ? (
            <span className={styles.updateError} role="alert">
              {featureError}
            </span>
          ) : null}
        </Card.Body>
      </Card>

      {enabledFeatures.length > 0 && activeFeature ? (
        <>
          <div
            className={styles.tabs}
            role="tablist"
            aria-label={t('settings:experimental.settingsTabsLabel')}
            data-testid="settings-experimental-feature-tablist"
          >
            {enabledFeatures.map((id, index) => (
              <button
                key={id}
                id={`settings-experimental-tab-${id}`}
                type="button"
                role="tab"
                aria-selected={activeFeature === id}
                aria-controls={`settings-panel-${id === 'cloudSync' ? 'cloud-sync' : id}`}
                tabIndex={activeFeature === id ? 0 : -1}
                className={activeFeature === id ? styles.tabActive : styles.tab}
                data-testid={`settings-experimental-tab-${id}`}
                onClick={() => onSelectFeatureTab(id)}
                onKeyDown={(e) => handleFeatureTabKeyDown(e, index)}
              >
                {t(`settings:experimental.${id}.label`)}
              </button>
            ))}
          </div>

          {activeFeature === 'battery' ? (
            <div
              id="settings-panel-battery"
              role="tabpanel"
              aria-labelledby="settings-experimental-tab-battery"
              data-testid="settings-panel-battery"
            >
              <SettingsBatteryPanel />
            </div>
          ) : null}

          {activeFeature === 'game' ? (
            <div
              id="settings-panel-game"
              role="tabpanel"
              aria-labelledby="settings-experimental-tab-game"
              data-testid="settings-panel-game"
            >
              <Card variant="flat" padding="md">
                <Card.Header>
                  <h2 className={styles.sectionTitle}>{t('settings:basic.gamePluginDir')}</h2>
                </Card.Header>
                <Card.Body padding="md">
                  <div className={styles.field}>
                    <label className={styles.label} htmlFor="settings-game-plugin-dir">
                      {t('settings:basic.gamePluginDir')}
                    </label>
                    <div className={styles.inputRow}>
                      <Input
                        id="settings-game-plugin-dir"
                        type="text"
                        value={gamePluginDir}
                        onChange={onGamePluginDirChange}
                        icon={<FolderIcon />}
                      />
                      <Button
                        variant="secondary"
                        size="md"
                        onClick={onChooseGamePluginDir}
                        disabled={choosingGamePluginDir}
                      >
                        {choosingGamePluginDir
                          ? t('settings:basic.selecting')
                          : t('settings:basic.selectFolder')}
                      </Button>
                    </div>
                    <p className={styles.helper}>{t('settings:basic.gamePluginDirHelper')}</p>
                  </div>
                  <div className={styles.footer}>
                    {saveError ? (
                      <span className={styles.updateError} role="alert">
                        {saveError}
                      </span>
                    ) : savedAt && !isDirty ? (
                      <span className={styles.aboutHint}>
                        {t('settings:status.savedAt', { time: formatTime(savedAt) })}
                      </span>
                    ) : isDirty ? (
                      <span className={styles.aboutHint}>{t('settings:status.dirtyHint')}</span>
                    ) : null}
                    <Button
                      variant="ghost"
                      size="md"
                      onClick={onResetDefaults}
                      disabled={!canResetCoreDefaults}
                    >
                      {t('settings:action.resetDefault')}
                    </Button>
                    <Button variant="primary" size="md" onClick={onSave} disabled={saving}>
                      {saving ? t('settings:action.saving') : t('settings:action.apply')}
                    </Button>
                  </div>
                </Card.Body>
              </Card>
            </div>
          ) : null}

          {activeFeature === 'automation' ? (
            <div
              id="settings-panel-automation"
              role="tabpanel"
              aria-labelledby="settings-experimental-tab-automation"
              data-testid="settings-panel-automation"
            >
              {automationLoadError ? (
                <div className={styles.resourceError} role="alert">
                  <span className={styles.updateError}>
                    {t('settings:resource.loadFailed', {
                      error: automationLoadError.message,
                    })}
                  </span>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void onRetryAutomation()}
                    disabled={retryingAutomation}
                  >
                    {retryingAutomation
                      ? t('settings:resource.retrying')
                      : t('settings:resource.retry')}
                  </Button>
                </div>
              ) : (
                <AutomationSettingsPanel
                  form={automationForm}
                  defaults={defaultAutomationForm}
                  dirty={automationDirty}
                  saving={savingAutomation}
                  error={automationError}
                  saved={automationSaved}
                  onChange={onAutomationChange}
                  onResetDefaults={onResetAutomationDefaults}
                  onSave={onSaveAutomation}
                  canResetDefaults={canResetAutomationDefaults}
                  agentAdapters={agentAdapters}
                  onOpenOpenCodeBridgePreview={onOpenOpenCodeBridgePreview}
                />
              )}
            </div>
          ) : null}

          {activeFeature === 'cloudSync' ? (
            <div
              id="settings-panel-cloud-sync"
              role="tabpanel"
              aria-labelledby="settings-experimental-tab-cloudSync"
              data-testid="settings-panel-cloud-sync"
            >
              <SettingsCloudSyncPanel {...cloudSync} />
            </div>
          ) : null}
        </>
      ) : null}
    </>
  );
}
