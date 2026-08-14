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
 *   - 子 tab：常规 / 依赖环境 / 健康提醒 / 充电模式 / 活动统计 / 同步 / AI / 自动化 / Fleet / 关于
 *   - 组合 pure panels（General/Dependencies/Sync/Health/Battery/Activity/Automation/Fleet/AI/About）
 *   - loading / core loadError early return 保留在壳层（hooks 已在 controller 无条件执行）
 *   - 所有用户可见文案经 i18next 翻译（settings ns + common ns）
 */
import { useCallback, useEffect, useRef, type ReactElement } from 'react';
import { useNavigate } from 'react-router-dom';
import { Button } from '@/components/primitives';
import { openCodeBridgePreviewHref } from '@/lib/agentAdapterPresentation';
import { AutomationSettingsPanel } from './AutomationSettingsPanel';
import { HealthPanel } from './HealthPanel';
import { ActivityStatsPanel } from './ActivityStatsPanel';
import { SettingsGeneralPanel } from './SettingsGeneralPanel';
import { SettingsSyncPanel } from './SettingsSyncPanel';
import { SettingsDependenciesPanel } from './SettingsDependenciesPanel';
import { SettingsAiPanel } from './SettingsAiPanel';
import { SettingsAboutPanel } from './SettingsAboutPanel';
import { SettingsFleetPanel } from './SettingsFleetPanel';
import { SettingsBatteryPanel } from './SettingsBatteryPanel';
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
  const navigate = useNavigate();
  const tablistRef = useRef<HTMLDivElement | null>(null);

  /**
   * Business Logic: OpenCode previewRequired 打开既有 Agent Hub 项目预览，禁止静默 enable。
   * Code Logic: navigate(`/agent-hub?preview=1&bridge=...`)。
   */
  const handleOpenOpenCodeBridgePreview = useCallback(() => {
    navigate(openCodeBridgePreviewHref());
  }, [navigate]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   ≤680px 深链选中的 tab 可能落在横向滚动区外，必须把选中 tab 滚进 tablist 视口，且不移动页面主滚动。
   *
   * Code Logic（这个 effect 做什么）:
   *   activeTab 变化后双 rAF 等布局稳定，用 getBoundingClientRect 相对 tablist 计算 scrollLeft
   *   （禁止用 offsetLeft：offsetParent 未必是 tablist，会算成过大 scroll 把选中 tab 左侧裁切），
   *   对 tablist 自身 scrollTo/scrollLeft 居中并二次校正到完全可见。
   */
  useEffect(() => {
    if (ctrl.loading || ctrl.loadError) return;
    const tablist = tablistRef.current;
    if (!tablist) return;
    const selected = tablist.querySelector<HTMLElement>(
      `[role="tab"][aria-selected="true"], #settings-tab-${ctrl.activeTab}`,
    );
    if (!selected) return;

    /**
     * Business Logic（为什么需要这个函数）:
     *   深链/键盘切换后用户必须看到完整选中 tab，否则窄屏 a11y 意图失效。
     *
     * Code Logic（这个函数做什么）:
     *   有布局时用 tab/tablist 的 getBoundingClientRect 差值换算 scrollLeft（居中 + 贴边校正）；
     *   jsdom/布局未就绪时仍调用 scrollTo（offsetLeft 回退）以保持可观测性，真实浏览器下一帧再校正。
     */
    const scrollSelectedIntoTablist = (): void => {
      const listWidth = tablist.clientWidth;
      const maxScroll = Math.max(0, tablist.scrollWidth - Math.max(listWidth, 0));
      const tabRect = selected.getBoundingClientRect();
      const listRect = tablist.getBoundingClientRect();
      const hasLayout =
        listWidth > 0 && listRect.width > 0 && tabRect.width > 0;

      let targetLeft: number;
      if (hasLayout) {
        const centerDelta =
          tabRect.left -
          listRect.left -
          (listRect.width - tabRect.width) / 2;
        targetLeft = Math.max(
          0,
          Math.min(tablist.scrollLeft + centerDelta, maxScroll),
        );
      } else {
        targetLeft = Math.max(
          0,
          selected.offsetLeft -
            Math.max(listWidth - selected.offsetWidth, 0) / 2,
        );
      }

      // 浏览器用 scrollTo；jsdom 等无该方法时回退 scrollLeft；auto 避免 smooth 未完成时 flaky 断言
      if (typeof tablist.scrollTo === 'function') {
        tablist.scrollTo({ left: targetLeft, behavior: 'auto' });
      }
      tablist.scrollLeft = targetLeft;

      if (!hasLayout) return;

      // 居中后若 tab 仍宽于视口或亚像素裁切，再贴边校正到完全可见
      const tabRect2 = selected.getBoundingClientRect();
      const listRect2 = tablist.getBoundingClientRect();
      let edgeAdjust = 0;
      if (tabRect2.left < listRect2.left - 0.5) {
        edgeAdjust = tabRect2.left - listRect2.left;
      } else if (tabRect2.right > listRect2.right + 0.5) {
        edgeAdjust = tabRect2.right - listRect2.right;
      }
      if (edgeAdjust !== 0) {
        targetLeft = Math.max(
          0,
          Math.min(tablist.scrollLeft + edgeAdjust, maxScroll),
        );
        if (typeof tablist.scrollTo === 'function') {
          tablist.scrollTo({ left: targetLeft, behavior: 'auto' });
        }
        tablist.scrollLeft = targetLeft;
      }
    };

    let frame2 = 0;
    const frame1 = window.requestAnimationFrame(() => {
      frame2 = window.requestAnimationFrame(scrollSelectedIntoTablist);
    });
    return () => {
      window.cancelAnimationFrame(frame1);
      window.cancelAnimationFrame(frame2);
    };
  }, [ctrl.activeTab, ctrl.loading, ctrl.loadError]);

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

  // core 配置加载失败：整页错误 + 重试（常规 tab 保存失败走 saveError，不进此分支）
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

        <div
          ref={tablistRef}
          className={styles.tabs}
          role="tablist"
          aria-label={t('settings:tabsLabel')}
          data-testid="settings-tablist"
        >
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
              data-testid={`settings-tab-${tab.id}`}
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
              saveError={ctrl.saveError}
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
              agentLedgerClearDialogOpen={ctrl.agentLedgerClearDialogOpen}
              agentLedgerClearing={ctrl.agentLedgerClearing}
              agentLedgerClearMessage={ctrl.agentLedgerClearMessage}
              agentLedgerClearError={ctrl.agentLedgerClearError}
              onOpenAgentLedgerClearDialog={ctrl.openAgentLedgerClearDialog}
              onCloseAgentLedgerClearDialog={ctrl.closeAgentLedgerClearDialog}
              onConfirmClearAgentLedger={() => void ctrl.confirmClearAgentLedger()}
              onboardingResetDialogOpen={ctrl.onboardingResetDialogOpen}
              onboardingResetting={ctrl.onboardingResetting}
              onboardingResetError={ctrl.onboardingResetError}
              onOpenOnboardingResetDialog={ctrl.openOnboardingResetDialog}
              onCloseOnboardingResetDialog={ctrl.closeOnboardingResetDialog}
              onConfirmOnboardingReset={() => void ctrl.confirmOnboardingReset()}
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

        {ctrl.activeTab === 'battery' ? (
          <div
            id="settings-panel-battery"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-battery"
            data-testid="settings-panel-battery"
          >
            <SettingsBatteryPanel />
          </div>
        ) : null}

        {ctrl.activeTab === 'activity' ? (
          <div
            id="settings-panel-activity"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-activity"
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
              <ActivityStatsPanel
                form={ctrl.healthForm}
                applied={ctrl.healthConfig}
                onPatch={ctrl.patchHealthForm}
                onResetDefaults={ctrl.handleResetActivityDefaults}
                onApply={() => void ctrl.handleApplyActivity()}
                applying={ctrl.applyingActivity}
                error={ctrl.activityError}
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
              lanSyncResult={ctrl.lanSyncResult}
              lanSyncing={ctrl.lanSyncing}
              lanSyncError={ctrl.lanSyncError}
              backupExporting={ctrl.backupExporting}
              backupExportPath={ctrl.backupExportPath}
              backupExportError={ctrl.backupExportError}
              backupRestoring={ctrl.backupRestoring}
              backupInspect={ctrl.backupInspect}
              backupArchivePath={ctrl.backupArchivePath}
              backupSelectedDomains={ctrl.backupSelectedDomains}
              backupMode={ctrl.backupMode}
              backupRestoreDialogOpen={ctrl.backupRestoreDialogOpen}
              backupRestoreResult={ctrl.backupRestoreResult}
              backupRestoreError={ctrl.backupRestoreError}
              backupJobs={ctrl.backupJobs}
              backupJobsLoading={ctrl.backupJobsLoading}
              backupJobsError={ctrl.backupJobsError}
              backupRollbackJobId={ctrl.backupRollbackJobId}
              backupRollbackDialogOpen={ctrl.backupRollbackDialogOpen}
              backupRollingBack={ctrl.backupRollingBack}
              onPatch={ctrl.patchCloudSyncForm}
              onResetDefaults={ctrl.handleResetCloudSyncDefaults}
              onTest={() => void ctrl.handleTestCloudSync()}
              onApply={() => void ctrl.handleApplyCloudSync()}
              onSyncNow={() => void ctrl.handleSyncNow()}
              onLanSyncNow={() => void ctrl.handleLanSyncNow()}
              onRetryLoad={() => void ctrl.handleRetryResourceGroup('cloudSync')}
              onBackupExport={() => void ctrl.handleBackupExport()}
              onBackupPickRestore={() => void ctrl.handleBackupPickRestore()}
              onBackupToggleDomain={ctrl.handleBackupToggleDomain}
              onBackupSetMode={ctrl.handleBackupSetMode}
              onBackupOpenRestoreDialog={ctrl.handleBackupOpenRestoreDialog}
              onBackupRestoreConfirm={() => void ctrl.handleBackupRestoreConfirm()}
              onCloseRestoreDialog={ctrl.handleCloseRestoreDialog}
              onRefreshRecoveryJobs={() => void ctrl.handleRefreshRecoveryJobs()}
              onOpenRollback={ctrl.handleOpenRollback}
              onConfirmRollback={() => void ctrl.handleConfirmRollback()}
              onCloseRollbackDialog={ctrl.handleCloseRollbackDialog}
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
                agentAdapters={ctrl.agentAdapters}
                onOpenOpenCodeBridgePreview={handleOpenOpenCodeBridgePreview}
              />
            )}
          </div>
        ) : null}

        {ctrl.activeTab === 'fleet' ? (
          <div
            id="settings-panel-fleet"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-fleet"
            data-testid="settings-panel-fleet"
          >
            <SettingsFleetPanel />
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
