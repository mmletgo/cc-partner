/**
 * Settings AI 设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在 AI tab 配置 GitHub Trending / Prompt 优化共用的 Claude CLI；
 *   状态与 API 调用由 controller 持有，本组件只渲染受控表单。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 githubTrending Card 与内部 Claude provider Card；无 @/api 导入，无业务副作用状态。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon, InfoIcon } from '@/lib/icons';
import type { ClaudeCliTestResult, GithubTrendingConfig } from '@/lib/types';
import { InternalClaudeProviderCard } from '@/components/domain/InternalClaudeProviderCard';
import type { GithubTrendingForm } from './settingsState';
import styles from './Settings.module.css';

/**
 * AI 面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层把 controller 的 AI 相关状态透传给 pure panel，避免 panel 直接读 hook。
 *
 * Code Logic（这个接口做什么）:
 *   声明 githubTrending 受控值、loading/error 与 patch/reset/apply/test/retry 回调。
 */
export interface SettingsAiPanelProps {
  githubTrendingForm: GithubTrendingForm;
  githubTrendingConfig: GithubTrendingConfig | null;
  claudeCliTest: ClaudeCliTestResult | null;
  githubTrendingError: string | null;
  testingClaudeCli: boolean;
  applyingGithubTrending: boolean;
  githubTrendingLoadError: Error | null;
  canResetGithubTrendingDefaults: boolean;
  onPatchGithubTrending: (partial: Partial<GithubTrendingForm>) => void;
  onResetGithubTrendingDefaults: () => void;
  onApplyGithubTrending: () => void;
  onTestClaudeCli: () => void;
  onRetryGithubTrendingLoad: () => void;
  retryingGithubTrending: boolean;
}

/**
 * AI 设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   AI tab 是独立业务组（CLI 配置 + Prompt 优化偏好），需要 pure 视图配合 ownership 守卫。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 置顶；原样渲染 githubTrending 与内部 Claude provider Card。
 *
 * @param props 受控 AI 表单与动作
 * @returns AI tab 内容
 */
export function SettingsAiPanel({
  githubTrendingForm,
  githubTrendingConfig,
  claudeCliTest,
  githubTrendingError,
  testingClaudeCli,
  applyingGithubTrending,
  githubTrendingLoadError,
  canResetGithubTrendingDefaults,
  onPatchGithubTrending,
  onResetGithubTrendingDefaults,
  onApplyGithubTrending,
  onTestClaudeCli,
  onRetryGithubTrendingLoad,
  retryingGithubTrending,
}: SettingsAiPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);

  return (
    <>
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
                onPatchGithubTrending({
                  aiEnabled: !githubTrendingForm.aiEnabled,
                })
              }
              role="switch"
              aria-checked={githubTrendingForm.aiEnabled}
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
                {githubTrendingForm.aiEnabled ? (
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
              value={githubTrendingForm.claudeCliPath}
              onChange={(e) => onPatchGithubTrending({ claudeCliPath: e.target.value })}
              mono
            />
            <p className={styles.helper}>{t('settings:githubTrending.claudeCliPath.helper')}</p>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="settings-github-claude-model">
              {t('settings:githubTrending.claudeModel.label')}
            </label>
            <Input
              id="settings-github-claude-model"
              type="text"
              value={githubTrendingForm.claudeModel}
              onChange={(e) => onPatchGithubTrending({ claudeModel: e.target.value })}
              mono
            />
            <p className={styles.helper}>{t('settings:githubTrending.claudeModel.helper')}</p>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="settings-github-cache-ttl">
              {t('settings:githubTrending.cacheTtlHours.label')}
            </label>
            <Input
              id="settings-github-cache-ttl"
              type="number"
              value={githubTrendingForm.cacheTtlHours}
              onChange={(e) =>
                onPatchGithubTrending({
                  cacheTtlHours: Number(e.target.value) || 24,
                })
              }
              min={1}
              max={168}
              mono
            />
            <p className={styles.helper}>{t('settings:githubTrending.cacheTtlHours.helper')}</p>
          </div>

          {githubTrendingConfig ? (
            <div className={styles.metaRow}>
              <span className={styles.metaKey}>{t('settings:githubTrending.appliedConfig')}</span>
              <span className={styles.metaValue}>
                {githubTrendingConfig.aiEnabled
                  ? t('settings:sync.enabled')
                  : t('settings:sync.disabled')}
                {' · '}
                {githubTrendingConfig.claudeCliPath || 'claude'}
                {' · '}
                {githubTrendingConfig.claudeModel || 'sonnet'}
              </span>
            </div>
          ) : null}

          <div className={styles.aboutActions}>
            <Button
              variant="secondary"
              size="md"
              icon={<InfoIcon />}
              onClick={onTestClaudeCli}
              disabled={testingClaudeCli}
            >
              {testingClaudeCli
                ? t('settings:githubTrending.testing')
                : t('settings:githubTrending.testCli')}
            </Button>
            <Button
              variant="ghost"
              size="md"
              onClick={onResetGithubTrendingDefaults}
              disabled={!canResetGithubTrendingDefaults}
              title={
                canResetGithubTrendingDefaults
                  ? undefined
                  : t('settings:resource.defaultsUnavailable')
              }
            >
              {t('settings:action.resetDefault')}
            </Button>
            <Button
              variant="primary"
              size="md"
              onClick={onApplyGithubTrending}
              disabled={applyingGithubTrending}
            >
              {applyingGithubTrending
                ? t('settings:githubTrending.applying')
                : t('settings:githubTrending.apply')}
            </Button>
          </div>

          {claudeCliTest ? (
            <span
              className={`${styles.aboutHint} ${claudeCliTest.ok ? '' : styles.dangerText}`}
            >
              <InfoIcon size={14} />
              <span>
                {claudeCliTest.ok
                  ? t('settings:githubTrending.testOk', {
                      version: claudeCliTest.version ?? '—',
                    })
                  : t('settings:githubTrending.testFailed', {
                      error: claudeCliTest.error ?? '',
                    })}
              </span>
            </span>
          ) : null}

          {githubTrendingLoadError ? (
            <div className={styles.resourceError} role="alert">
              <span className={styles.updateError}>
                {t('settings:resource.loadFailed', {
                  error: githubTrendingLoadError.message,
                })}
              </span>
              <Button
                variant="secondary"
                size="sm"
                onClick={onRetryGithubTrendingLoad}
                disabled={retryingGithubTrending}
              >
                {retryingGithubTrending
                  ? t('settings:resource.retrying')
                  : t('settings:resource.retry')}
              </Button>
            </div>
          ) : null}

          {githubTrendingError ? (
            <span className={styles.updateError}>{githubTrendingError}</span>
          ) : null}
        </Card.Body>
      </Card>

      {/* Card: 内部 Claude provider 覆盖（自包含 domain 卡，不改 controller） */}
      <InternalClaudeProviderCard />
    </>
  );
}
