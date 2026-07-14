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
 *   - 组合 pure panels（General/Dependencies/Sync/Health/Automation）与内联 AI/About 内容
 *   - loading / core loadError early return 保留在壳层（hooks 已在 controller 无条件执行）
 *   - 所有用户可见文案经 i18next 翻译（settings ns + common ns）
 */
import type { ReactElement } from 'react';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon, KeyboardIcon, SyncIcon, InfoIcon, DownloadIcon } from '@/lib/icons';
import { formatShortcutForDisplay } from './shortcutRecorder';
import { AutomationSettingsPanel } from './AutomationSettingsPanel';
import { HealthPanel } from './HealthPanel';
import { SettingsGeneralPanel } from './SettingsGeneralPanel';
import { SettingsSyncPanel } from './SettingsSyncPanel';
import { SettingsDependenciesPanel } from './SettingsDependenciesPanel';
import {
  PROMPT_OPTIMIZER_SHORTCUT_ID,
  useSettingsController,
} from './useSettingsController';
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
 *   再渲染 tablist 与按 activeTab 组合的 panel/内联内容。
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
            {/* Card: Claude CLI / AI 能力 */}
            <Card variant="flat" padding="md">
              <Card.Header>
                <h2 className={styles.sectionTitle}>{t('settings:githubTrending.title')}</h2>
              </Card.Header>
              <Card.Body padding="md">
                <p className={styles.helper}>{t('settings:githubTrending.subtitle')}</p>

                <div className={styles.toggleList}>
                  <button
                    type="button"
                    className={styles.toggleRow}
                    onClick={() =>
                      ctrl.patchGithubTrendingForm({
                        aiEnabled: !ctrl.githubTrendingForm.aiEnabled,
                      })
                    }
                    role="switch"
                    aria-checked={ctrl.githubTrendingForm.aiEnabled}
                    aria-label={t('settings:githubTrending.aiEnabled.label')}
                  >
                    <div className={styles.toggleText}>
                      <span className={styles.toggleLabel}>
                        {t('settings:githubTrending.aiEnabled.label')}
                      </span>
                      <span className={styles.toggleHelper}>
                        {t('settings:githubTrending.aiEnabled.helper')}
                      </span>
                    </div>
                    <span className={styles.toggleState}>
                      {ctrl.githubTrendingForm.aiEnabled ? (
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
                </div>

                <div className={styles.field}>
                  <label className={styles.label} htmlFor="settings-github-claude-path">
                    {t('settings:githubTrending.claudeCliPath.label')}
                  </label>
                  <Input
                    id="settings-github-claude-path"
                    type="text"
                    value={ctrl.githubTrendingForm.claudeCliPath}
                    onChange={(e) =>
                      ctrl.patchGithubTrendingForm({ claudeCliPath: e.target.value })
                    }
                    mono
                  />
                  <p className={styles.helper}>
                    {t('settings:githubTrending.claudeCliPath.helper')}
                  </p>
                </div>

                <div className={styles.field}>
                  <label className={styles.label} htmlFor="settings-github-claude-model">
                    {t('settings:githubTrending.claudeModel.label')}
                  </label>
                  <Input
                    id="settings-github-claude-model"
                    type="text"
                    value={ctrl.githubTrendingForm.claudeModel}
                    onChange={(e) =>
                      ctrl.patchGithubTrendingForm({ claudeModel: e.target.value })
                    }
                    mono
                  />
                  <p className={styles.helper}>
                    {t('settings:githubTrending.claudeModel.helper')}
                  </p>
                </div>

                <div className={styles.field}>
                  <label className={styles.label} htmlFor="settings-github-cache-ttl">
                    {t('settings:githubTrending.cacheTtlHours.label')}
                  </label>
                  <Input
                    id="settings-github-cache-ttl"
                    type="number"
                    value={ctrl.githubTrendingForm.cacheTtlHours}
                    onChange={(e) =>
                      ctrl.patchGithubTrendingForm({
                        cacheTtlHours: Number(e.target.value) || 24,
                      })
                    }
                    min={1}
                    max={168}
                    mono
                  />
                  <p className={styles.helper}>
                    {t('settings:githubTrending.cacheTtlHours.helper')}
                  </p>
                </div>

                {ctrl.githubTrendingConfig ? (
                  <div className={styles.metaRow}>
                    <span className={styles.metaKey}>
                      {t('settings:githubTrending.appliedConfig')}
                    </span>
                    <span className={styles.metaValue}>
                      {ctrl.githubTrendingConfig.aiEnabled
                        ? t('settings:sync.enabled')
                        : t('settings:sync.disabled')}
                      {' · '}
                      {ctrl.githubTrendingConfig.claudeCliPath || 'claude'}
                      {' · '}
                      {ctrl.githubTrendingConfig.claudeModel || 'sonnet'}
                    </span>
                  </div>
                ) : null}

                <div className={styles.aboutActions}>
                  <Button
                    variant="secondary"
                    size="md"
                    icon={<InfoIcon />}
                    onClick={() => void ctrl.handleTestClaudeCli()}
                    disabled={ctrl.testingClaudeCli}
                  >
                    {ctrl.testingClaudeCli
                      ? t('settings:githubTrending.testing')
                      : t('settings:githubTrending.testCli')}
                  </Button>
                  <Button
                    variant="ghost"
                    size="md"
                    onClick={ctrl.handleResetGithubTrendingDefaults}
                    disabled={!ctrl.canResetGithubTrendingDefaults}
                    title={
                      ctrl.canResetGithubTrendingDefaults
                        ? undefined
                        : t('settings:resource.defaultsUnavailable')
                    }
                  >
                    {t('settings:action.resetDefault')}
                  </Button>
                  <Button
                    variant="primary"
                    size="md"
                    onClick={() => void ctrl.handleApplyGithubTrending()}
                    disabled={ctrl.applyingGithubTrending}
                  >
                    {ctrl.applyingGithubTrending
                      ? t('settings:githubTrending.applying')
                      : t('settings:githubTrending.apply')}
                  </Button>
                </div>

                {ctrl.claudeCliTest ? (
                  <span
                    className={`${styles.aboutHint} ${ctrl.claudeCliTest.ok ? '' : styles.dangerText}`}
                  >
                    <InfoIcon size={14} />
                    <span>
                      {ctrl.claudeCliTest.ok
                        ? t('settings:githubTrending.testOk', {
                            version: ctrl.claudeCliTest.version ?? '—',
                          })
                        : t('settings:githubTrending.testFailed', {
                            error: ctrl.claudeCliTest.error ?? '',
                          })}
                    </span>
                  </span>
                ) : null}

                {ctrl.githubTrendingLoadError ? (
                  <div className={styles.resourceError} role="alert">
                    <span className={styles.updateError}>
                      {t('settings:resource.loadFailed', {
                        error: ctrl.githubTrendingLoadError.message,
                      })}
                    </span>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void ctrl.handleRetryResourceGroup('githubTrending')}
                      disabled={ctrl.retryingGroup === 'githubTrending'}
                    >
                      {ctrl.retryingGroup === 'githubTrending'
                        ? t('settings:resource.retrying')
                        : t('settings:resource.retry')}
                    </Button>
                  </div>
                ) : null}

                {ctrl.githubTrendingError ? (
                  <span className={styles.updateError}>{ctrl.githubTrendingError}</span>
                ) : null}
              </Card.Body>
            </Card>

            {/* Card: Workbench Prompt 优化小组件 */}
            <Card variant="flat" padding="md">
              <Card.Header>
                <h2 className={styles.sectionTitle}>
                  {t('settings:promptOptimizerSettings.title')}
                </h2>
              </Card.Header>
              <Card.Body padding="md">
                <p className={styles.helper}>{t('settings:promptOptimizerSettings.subtitle')}</p>

                <div className={styles.shortcutList}>
                  <div className={styles.shortcutRow}>
                    <div className={styles.shortcutText}>
                      <span className={styles.shortcutLabel}>
                        {t('settings:promptOptimizerSettings.hotkey.label')}
                      </span>
                      <span className={styles.shortcutHelper}>
                        {ctrl.recordingShortcutId === PROMPT_OPTIMIZER_SHORTCUT_ID
                          ? t('settings:shortcut.recordingHelper')
                          : t('settings:promptOptimizerSettings.hotkey.helper')}
                      </span>
                    </div>
                    <div className={styles.shortcutInput}>
                      <Input
                        id="settings-prompt-optimizer-hotkey"
                        type="text"
                        value={
                          ctrl.recordingShortcutId === PROMPT_OPTIMIZER_SHORTCUT_ID
                            ? t('settings:shortcut.recording')
                            : formatShortcutForDisplay(ctrl.promptOptimizerForm.hotkey)
                        }
                        placeholder={t('settings:shortcut.placeholder')}
                        onChange={() => undefined}
                        onFocus={() => ctrl.handleShortcutFocus(PROMPT_OPTIMIZER_SHORTCUT_ID)}
                        onClick={() => ctrl.handleShortcutFocus(PROMPT_OPTIMIZER_SHORTCUT_ID)}
                        onBlur={() => ctrl.handleShortcutBlur(PROMPT_OPTIMIZER_SHORTCUT_ID)}
                        onKeyDown={(e) =>
                          ctrl.handleShortcutKeyDown(e, PROMPT_OPTIMIZER_SHORTCUT_ID)
                        }
                        icon={<KeyboardIcon />}
                        className={
                          ctrl.recordingShortcutId === PROMPT_OPTIMIZER_SHORTCUT_ID
                            ? styles.shortcutRecorderActive
                            : undefined
                        }
                        aria-label={t('settings:promptOptimizerSettings.hotkey.label')}
                        readOnly
                        mono
                      />
                    </div>
                  </div>
                </div>

                <div className={styles.toggleList}>
                  <button
                    type="button"
                    className={styles.toggleRow}
                    onClick={() => ctrl.patchPromptOptimizerForm({ fillLanguage: 'zh' })}
                    role="radio"
                    aria-checked={ctrl.promptOptimizerForm.fillLanguage === 'zh'}
                    aria-label={t('settings:promptOptimizerSettings.fillLanguage.zh')}
                  >
                    <div className={styles.toggleText}>
                      <span className={styles.toggleLabel}>
                        {t('settings:promptOptimizerSettings.fillLanguage.zh')}
                      </span>
                      <span className={styles.toggleHelper}>
                        {t('settings:promptOptimizerSettings.fillLanguage.helper')}
                      </span>
                    </div>
                    <span className={styles.toggleState}>
                      {ctrl.promptOptimizerForm.fillLanguage === 'zh' ? (
                        <Pill tone="success" dot>
                          <CheckIcon size={12} />
                          {t('settings:sync.enabled')}
                        </Pill>
                      ) : (
                        <Pill tone="neutral" dot>
                          {t('settings:sync.disabled')}
                        </Pill>
                      )}
                    </span>
                  </button>
                  <button
                    type="button"
                    className={styles.toggleRow}
                    onClick={() => ctrl.patchPromptOptimizerForm({ fillLanguage: 'en' })}
                    role="radio"
                    aria-checked={ctrl.promptOptimizerForm.fillLanguage === 'en'}
                    aria-label={t('settings:promptOptimizerSettings.fillLanguage.en')}
                  >
                    <div className={styles.toggleText}>
                      <span className={styles.toggleLabel}>
                        {t('settings:promptOptimizerSettings.fillLanguage.en')}
                      </span>
                      <span className={styles.toggleHelper}>
                        {t('settings:promptOptimizerSettings.fillLanguage.helper')}
                      </span>
                    </div>
                    <span className={styles.toggleState}>
                      {ctrl.promptOptimizerForm.fillLanguage === 'en' ? (
                        <Pill tone="success" dot>
                          <CheckIcon size={12} />
                          {t('settings:sync.enabled')}
                        </Pill>
                      ) : (
                        <Pill tone="neutral" dot>
                          {t('settings:sync.disabled')}
                        </Pill>
                      )}
                    </span>
                  </button>
                </div>

                {ctrl.promptOptimizerConfig ? (
                  <div className={styles.metaRow}>
                    <span className={styles.metaKey}>
                      {t('settings:promptOptimizerSettings.appliedConfig')}
                    </span>
                    <span className={styles.metaValue}>
                      {formatShortcutForDisplay(ctrl.promptOptimizerConfig.hotkey)}
                      {' · '}
                      {ctrl.promptOptimizerConfig.fillLanguage === 'en'
                        ? t('settings:promptOptimizerSettings.fillLanguage.en')
                        : t('settings:promptOptimizerSettings.fillLanguage.zh')}
                    </span>
                  </div>
                ) : null}

                <div className={styles.aboutActions}>
                  <Button
                    variant="ghost"
                    size="md"
                    onClick={ctrl.handleResetPromptOptimizerSettingsDefaults}
                    disabled={!ctrl.canResetPromptOptimizerDefaults}
                    title={
                      ctrl.canResetPromptOptimizerDefaults
                        ? undefined
                        : t('settings:resource.defaultsUnavailable')
                    }
                  >
                    {t('settings:action.resetDefault')}
                  </Button>
                  <Button
                    variant="primary"
                    size="md"
                    onClick={() => void ctrl.handleApplyPromptOptimizerSettings()}
                    disabled={ctrl.applyingPromptOptimizer}
                  >
                    {ctrl.applyingPromptOptimizer
                      ? t('settings:promptOptimizerSettings.applying')
                      : t('settings:promptOptimizerSettings.apply')}
                  </Button>
                </div>

                {ctrl.promptOptimizerSettingsError ? (
                  <span className={styles.updateError}>{ctrl.promptOptimizerSettingsError}</span>
                ) : null}
              </Card.Body>
            </Card>
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
            {/* Card 4: 关于 */}
            <Card variant="flat" padding="md">
              <Card.Header>
                <h2 className={styles.sectionTitle}>{t('settings:about.title')}</h2>
              </Card.Header>
              <Card.Body padding="md">
                {ctrl.versionLoadError ? (
                  <div className={styles.resourceError} role="alert">
                    <span className={styles.updateError}>
                      {t('settings:resource.versionLoadFailed', {
                        error: ctrl.versionLoadError.message,
                      })}
                    </span>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void ctrl.handleRetryResourceGroup('version')}
                      disabled={ctrl.retryingGroup === 'version'}
                    >
                      {ctrl.retryingGroup === 'version'
                        ? t('settings:resource.retrying')
                        : t('settings:resource.retry')}
                    </Button>
                  </div>
                ) : null}
                <dl className={styles.metaList}>
                  <div className={styles.metaRow}>
                    <dt className={styles.metaKey}>{t('settings:about.versionLabel')}</dt>
                    <dd className={styles.metaValue}>
                      <Pill tone="accent">{`v${ctrl.versionInfo?.version ?? '—'}`}</Pill>
                    </dd>
                  </div>
                  <div className={styles.metaRow}>
                    <dt className={styles.metaKey}>{t('settings:about.buildLabel')}</dt>
                    <dd className={styles.metaValue}>{ctrl.versionInfo?.buildDate ?? '—'}</dd>
                  </div>
                  <div className={styles.metaRow}>
                    <dt className={styles.metaKey}>{t('settings:about.sourceLabel')}</dt>
                    <dd className={styles.metaValue}>{t('settings:about.source')}</dd>
                  </div>
                </dl>
                <div className={styles.aboutActions}>
                  <Button
                    variant="secondary"
                    size="md"
                    icon={<SyncIcon />}
                    onClick={() => void ctrl.handleCheckUpdate()}
                    disabled={ctrl.updateCheckDisabled}
                  >
                    {ctrl.updateIsChecking
                      ? t('settings:about.checking')
                      : t('settings:about.checkUpdate')}
                  </Button>
                  <span className={styles.aboutHint}>
                    <InfoIcon size={14} />
                    <span>{ctrl.updateHint}</span>
                  </span>
                </div>

                {/* 发现新版本时展示：版本说明 + 下载/进度/安装；installing 不伪造进度条 */}
                {ctrl.updateResult?.hasUpdate ? (
                  <div className={styles.updateBlock}>
                    <div className={styles.metaRow}>
                      <span className={styles.metaKey}>{t('settings:about.latestVersion')}</span>
                      <Pill tone="accent">{`v${ctrl.updateResult.version}`}</Pill>
                    </div>
                    {ctrl.updateResult.body ? (
                      <p className={styles.updateBody}>{ctrl.updateResult.body}</p>
                    ) : null}

                    {ctrl.downloadStatus?.status === 'downloading' ? (
                      <div className={styles.progressRow}>
                        <div className={styles.progressBar}>
                          <div
                            className={styles.progressFill}
                            style={{
                              width: `${Math.round(ctrl.downloadStatus.progress * 100)}%`,
                            }}
                          />
                        </div>
                        <span className={styles.progressText}>
                          {Math.round(ctrl.downloadStatus.progress * 100)}%
                        </span>
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={<XIcon size={14} />}
                          onClick={() => void ctrl.handleCancelDownload()}
                        >
                          {t('settings:about.cancel')}
                        </Button>
                      </div>
                    ) : ctrl.downloadStatus?.status === 'installing' ||
                      ctrl.updateIsInstalling ? (
                      <div className={styles.updateActions}>
                        <Button
                          variant="primary"
                          size="sm"
                          icon={<DownloadIcon size={14} />}
                          disabled
                        >
                          {t('settings:about.installing')}
                        </Button>
                        <span className={styles.aboutHint}>{t('settings:about.installing')}</span>
                      </div>
                    ) : ctrl.downloadStatus?.status === 'completed' ? (
                      <div className={styles.updateActions}>
                        {ctrl.updateInstallRetry ? (
                          <span className={styles.updateError}>
                            {ctrl.downloadStatus.error || t('settings:about.installFailed')}
                          </span>
                        ) : (
                          <span className={styles.aboutHint}>
                            {t('settings:about.downloadCompleted')}
                          </span>
                        )}
                        <Button
                          variant="primary"
                          size="sm"
                          icon={<DownloadIcon size={14} />}
                          onClick={() => void ctrl.handleInstall()}
                          disabled={ctrl.updateIsInstalling}
                        >
                          {ctrl.updateInstallMode === 'installing'
                            ? t('settings:about.installing')
                            : ctrl.updateInstallMode === 'retryInstall'
                              ? t('settings:about.retryInstall')
                              : t('settings:about.installAndRestart')}
                        </Button>
                      </div>
                    ) : ctrl.downloadStatus?.status === 'failed' ? (
                      <div className={styles.updateActions}>
                        <span className={styles.updateError}>
                          {ctrl.downloadStatus.error || t('settings:about.downloadFailed')}
                        </span>
                        <Button
                          variant="secondary"
                          size="sm"
                          icon={<DownloadIcon size={14} />}
                          onClick={() => void ctrl.handleDownload()}
                          disabled={ctrl.updateDownloadDisabled}
                        >
                          {t('settings:about.retryDownload')}
                        </Button>
                      </div>
                    ) : ctrl.downloadStatus?.status === 'cancelled' ? (
                      <div className={styles.updateActions}>
                        <span className={styles.aboutHint}>
                          {t('settings:about.downloadCancelled')}
                        </span>
                        <Button
                          variant="secondary"
                          size="sm"
                          icon={<DownloadIcon size={14} />}
                          onClick={() => void ctrl.handleDownload()}
                          disabled={ctrl.updateDownloadDisabled}
                        >
                          {t('settings:about.redownload')}
                        </Button>
                      </div>
                    ) : ctrl.updateResult.downloadUrl ? (
                      <div className={styles.updateActions}>
                        <Button
                          variant="primary"
                          size="sm"
                          icon={<DownloadIcon size={14} />}
                          onClick={() => void ctrl.handleDownload()}
                          disabled={ctrl.updateDownloadDisabled}
                        >
                          {t('settings:about.downloadUpdate', {
                            size: ctrl.formatSize(ctrl.updateResult.size ?? 0),
                          })}
                        </Button>
                      </div>
                    ) : (
                      <span className={styles.aboutHint}>{t('settings:about.noAsset')}</span>
                    )}
                  </div>
                ) : null}
              </Card.Body>
            </Card>
          </div>
        ) : null}
      </div>
    </div>
  );
}

Settings.displayName = 'Settings';
