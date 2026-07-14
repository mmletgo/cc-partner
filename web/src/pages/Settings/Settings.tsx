/**
 * Settings 页面 - 偏好设置中心（壳层组合）
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要集中调整设备名、接收目录、截图快捷键、云端同步等运行时偏好，
 *   改变会通过表单即时反映在 UI 状态中；"保存"按钮在用户主动提交时
 *   把整张配置表发送到后端持久化，区分"未保存修改"和"已保存配置"。
 *
 * Code Logic（这个页面做什么）:
 *   - 调用 useSettingsController 获取全部编排状态/handler
 *   - 子 tab：常规 / 依赖环境 / 健康提醒 / 同步 / AI / 自动化 / 关于
 *   - 组合 pure panels（General/Dependencies/Sync/Health/Automation/AI/About）
 *   - loading / core loadError early return 保留在壳层（hooks 已在 controller 无条件执行）
 *   - 所有用户可见文案经 i18next 翻译（settings ns + common ns）
 */
import type { ReactElement } from 'react';
import { Button } from '@/components/primitives';
import { AutomationSettingsPanel } from './AutomationSettingsPanel';
import { HealthPanel } from './HealthPanel';
import { SettingsGeneralPanel } from './SettingsGeneralPanel';
import { SettingsSyncPanel } from './SettingsSyncPanel';
import { SettingsDependenciesPanel } from './SettingsDependenciesPanel';
import { SettingsAiPanel } from './SettingsAiPanel';
import { SettingsAboutPanel } from './SettingsAboutPanel';
import { useSettingsController } from './useSettingsController';
import styles from './Settings.module.css';

/**
 * Settings 页面组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Settings 路由需要一个可导航的偏好中心壳层，把 tab 切换与 panel 组合留给 UI，
 *   资源/表单编排下沉到 controller。
 *
 * Code Logic（这个组件做什么）:
 *   无条件调用 useSettingsController；loading/loadError 后 early return；
 *   再渲染 tablist 与按 activeTab 组合的 pure panels。
 *
 * @returns Settings 路由的根容器
 */
export function Settings(): ReactElement {
  const ctrl = useSettingsController();
  const { t } = ctrl;

  // 加载状态
  if (ctrl.loading) {
    return (
      <div className={styles.page}>
        <div className={styles.container}>
          <header className={styles.header}>
            <span className={styles.eyebrow}>{t('settings:eyebrow')}</span>
            <h1 className={styles.title}>{t('settings:title')}</h1>
            <p className={styles.lead}>{t('settings:loading')}</p>
          </header>
        </div>
      </div>
    );
  }

  // core 配置加载失败：整页错误 + 重试（save 失败也复用此分支）
  if (ctrl.loadError) {
    const isCoreLoadFailure = ctrl.resourceResults?.core.status === 'error';
    return (
      <div className={styles.page}>
        <div className={styles.container}>
          <header className={styles.header}>
            <span className={styles.eyebrow}>{t('settings:eyebrow')}</span>
            <h1 className={styles.title}>{t('settings:title')}</h1>
            <p className={`${styles.lead} ${styles.dangerText}`}>
              {t('settings:loadFailed', { error: ctrl.loadError })}
            </p>
            {isCoreLoadFailure ? (
              <div className={styles.resourceErrorActions}>
                <Button
                  variant="secondary"
                  size="md"
                  onClick={() => void ctrl.handleRetryResourceGroup('core')}
                  disabled={ctrl.retryingGroup === 'core'}
                >
                  {ctrl.retryingGroup === 'core'
                    ? t('settings:resource.retrying')
                    : t('settings:resource.retry')}
                </Button>
              </div>
            ) : null}
          </header>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        {/* 页面头部 */}
        <header className={styles.header}>
          <span className={styles.eyebrow}>{t('settings:eyebrow')}</span>
          <h1 className={styles.title}>{t('settings:title')}</h1>
          <p className={styles.lead}>{t('settings:subtitle')}</p>
        </header>

        <div className={styles.tabs} role="tablist" aria-label={t('settings:tabsLabel')}>
          {ctrl.tabs.map((tab, index) => (
            <button
              key={tab.id}
              id={`settings-tab-${tab.id}`}
              type="button"
              role="tab"
              aria-selected={ctrl.activeTab === tab.id}
              aria-controls={`settings-panel-${tab.id}`}
              tabIndex={ctrl.activeTab === tab.id ? 0 : -1}
              className={ctrl.activeTab === tab.id ? styles.tabActive : styles.tab}
              onClick={() => ctrl.setActiveTab(tab.id)}
              onKeyDown={(e) => ctrl.handleTabKeyDown(e, index)}
            >
              {t(`settings:tabs.${tab.labelKey}`)}
            </button>
          ))}
        </div>

        {ctrl.activeTab === 'general' ? (
          <div
            id="settings-panel-general"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-general"
          >
            <SettingsGeneralPanel
              state={ctrl.state}
              isDirty={ctrl.isDirty}
              savedAt={ctrl.savedAt}
              saving={ctrl.saving}
              choosingDir={ctrl.choosingDir}
              canResetCoreDefaults={ctrl.canResetCoreDefaults}
              recordingShortcutId={ctrl.recordingShortcutId}
              onDeviceNameChange={ctrl.handleDeviceNameChange}
              onReceiveDirChange={ctrl.handleReceiveDirChange}
              onChooseDir={() => void ctrl.handleChooseDir()}
              onShortcutFocus={ctrl.handleShortcutFocus}
              onShortcutBlur={ctrl.handleShortcutBlur}
              onShortcutKeyDown={ctrl.handleShortcutKeyDown}
              onResetDefaults={ctrl.handleResetDefaults}
              onSave={() => void ctrl.handleSave()}
            />
          </div>
        ) : null}

        {ctrl.activeTab === 'health' ? (
          <div
            id="settings-panel-health"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-health"
          >
            {ctrl.healthLoadError ? (
              <div className={styles.resourceError} role="alert">
                <span className={styles.updateError}>
                  {t('settings:resource.loadFailed', { error: ctrl.healthLoadError.message })}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void ctrl.handleRetryResourceGroup('health')}
                  disabled={ctrl.retryingGroup === 'health'}
                >
                  {ctrl.retryingGroup === 'health'
                    ? t('settings:resource.retrying')
                    : t('settings:resource.retry')}
                </Button>
              </div>
            ) : (
              <HealthPanel
                form={ctrl.healthForm}
                applied={ctrl.healthConfig}
                onPatch={ctrl.patchHealthForm}
                onResetDefaults={ctrl.handleResetHealthDefaults}
                onApply={() => void ctrl.handleApplyHealth()}
                applying={ctrl.applyingHealth}
                error={ctrl.healthError}
                canResetDefaults={ctrl.canResetHealthDefaults}
              />
            )}
          </div>
        ) : null}

        {ctrl.activeTab === 'dependencies' ? (
          <div
            id="settings-panel-dependencies"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-dependencies"
          >
            <SettingsDependenciesPanel
              permStatus={ctrl.permStatus}
              permLoading={ctrl.permLoading}
              permRefreshing={ctrl.permRefreshing}
              permError={ctrl.permError}
              permRequesting={ctrl.permRequesting}
              onRequestAccess={ctrl.handleRequestAccess}
              onRefreshPermissions={() => void ctrl.refreshPermissions()}
            />
          </div>
        ) : null}

        {ctrl.activeTab === 'sync' ? (
          <div
            id="settings-panel-sync"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-sync"
          >
            <SettingsSyncPanel
              form={ctrl.cloudSyncForm}
              cloudSync={ctrl.cloudSync}
              syncResult={ctrl.syncResult}
              testResult={ctrl.testResult}
              cloudSyncError={ctrl.cloudSyncError}
              testing={ctrl.testing}
              applying={ctrl.applying}
              syncing={ctrl.syncing}
              loadError={ctrl.cloudSyncLoadError}
              retrying={ctrl.retryingGroup === 'cloudSync'}
              canResetDefaults={ctrl.canResetCloudSyncDefaults}
              onPatch={ctrl.patchCloudSyncForm}
              onResetDefaults={ctrl.handleResetCloudSyncDefaults}
              onTest={() => void ctrl.handleTestCloudSync()}
              onApply={() => void ctrl.handleApplyCloudSync()}
              onSyncNow={() => void ctrl.handleSyncNow()}
              onRetryLoad={() => void ctrl.handleRetryResourceGroup('cloudSync')}
            />
          </div>
        ) : null}

        {ctrl.activeTab === 'ai' ? (
          <div
            id="settings-panel-ai"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-ai"
          >
            <SettingsAiPanel
              githubTrendingForm={ctrl.githubTrendingForm}
              githubTrendingConfig={ctrl.githubTrendingConfig}
              claudeCliTest={ctrl.claudeCliTest}
              githubTrendingError={ctrl.githubTrendingError}
              testingClaudeCli={ctrl.testingClaudeCli}
              applyingGithubTrending={ctrl.applyingGithubTrending}
              githubTrendingLoadError={ctrl.githubTrendingLoadError}
              canResetGithubTrendingDefaults={ctrl.canResetGithubTrendingDefaults}
              onPatchGithubTrending={ctrl.patchGithubTrendingForm}
              onResetGithubTrendingDefaults={ctrl.handleResetGithubTrendingDefaults}
              onApplyGithubTrending={() => void ctrl.handleApplyGithubTrending()}
              onTestClaudeCli={() => void ctrl.handleTestClaudeCli()}
              onRetryGithubTrendingLoad={() =>
                void ctrl.handleRetryResourceGroup('githubTrending')
              }
              retryingGithubTrending={ctrl.retryingGroup === 'githubTrending'}
              promptOptimizerForm={ctrl.promptOptimizerForm}
              promptOptimizerConfig={ctrl.promptOptimizerConfig}
              applyingPromptOptimizer={ctrl.applyingPromptOptimizer}
              promptOptimizerSettingsError={ctrl.promptOptimizerSettingsError}
              canResetPromptOptimizerDefaults={ctrl.canResetPromptOptimizerDefaults}
              onPatchPromptOptimizer={ctrl.patchPromptOptimizerForm}
              onResetPromptOptimizerDefaults={ctrl.handleResetPromptOptimizerSettingsDefaults}
              onApplyPromptOptimizer={() => void ctrl.handleApplyPromptOptimizerSettings()}
              recordingShortcutId={ctrl.recordingShortcutId}
              onShortcutFocus={ctrl.handleShortcutFocus}
              onShortcutBlur={ctrl.handleShortcutBlur}
              onShortcutKeyDown={ctrl.handleShortcutKeyDown}
            />
          </div>
        ) : null}

        {ctrl.activeTab === 'automation' ? (
          <div
            id="settings-panel-automation"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-automation"
          >
            {ctrl.automationLoadError ? (
              <div className={styles.resourceError} role="alert">
                <span className={styles.updateError}>
                  {t('settings:resource.loadFailed', {
                    error: ctrl.automationLoadError.message,
                  })}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void ctrl.handleRetryResourceGroup('automation')}
                  disabled={ctrl.retryingGroup === 'automation'}
                >
                  {ctrl.retryingGroup === 'automation'
                    ? t('settings:resource.retrying')
                    : t('settings:resource.retry')}
                </Button>
              </div>
            ) : (
              <AutomationSettingsPanel
                form={ctrl.automationForm}
                defaults={ctrl.defaultAutomationForm}
                dirty={ctrl.automationDirty}
                saving={ctrl.savingAutomation}
                error={ctrl.automationError}
                saved={ctrl.automationSaved}
                onChange={ctrl.handleAutomationFormChange}
                onResetDefaults={ctrl.handleResetAutomationDefaults}
                onSave={() => void ctrl.handleSaveAutomation()}
                canResetDefaults={ctrl.canResetAutomationDefaults}
              />
            )}
          </div>
        ) : null}

        {ctrl.activeTab === 'about' ? (
          <div
            id="settings-panel-about"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-about"
          >
            <SettingsAboutPanel
              versionInfo={ctrl.versionInfo}
              versionLoadError={ctrl.versionLoadError}
              updateResult={ctrl.updateResult}
              updateHint={ctrl.updateHint}
              updateCheckDisabled={ctrl.updateCheckDisabled}
              updateDownloadDisabled={ctrl.updateDownloadDisabled}
              updateInstallRetry={ctrl.updateInstallRetry}
              updateInstallMode={ctrl.updateInstallMode}
              updateIsInstalling={ctrl.updateIsInstalling}
              updateIsChecking={ctrl.updateIsChecking}
              downloadStatus={ctrl.downloadStatus}
              formatSize={ctrl.formatSize}
              onCheckUpdate={() => void ctrl.handleCheckUpdate()}
              onDownload={() => void ctrl.handleDownload()}
              onCancelDownload={() => void ctrl.handleCancelDownload()}
              onInstall={() => void ctrl.handleInstall()}
              onRetryVersionLoad={() => void ctrl.handleRetryResourceGroup('version')}
              retryingVersion={ctrl.retryingGroup === 'version'}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}

Settings.displayName = 'Settings';
