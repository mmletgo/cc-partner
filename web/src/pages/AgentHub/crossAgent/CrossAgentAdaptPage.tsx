/**
 * Cross-agent selective preview page.
 *
 * Business Logic（为什么需要）:
 *   当前只展示本机用户级指令的真实预览；未认证的 full/apply、peer/project 不应以可点击控件出现。
 *
 * Code Logic（做什么）:
 *   复用页面 controller 和 StatusMessage，渲染源、目标、正文及有界 diff，不暴露写入动作。
 */

import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { AgentHubContext } from '../context/agentHubContext';
import styles from '../AgentHub.module.css';
import { adaptModeTone } from './crossAgentPresentation';
import { useCrossAgentAdaptController } from './useCrossAgentAdaptController';

export interface CrossAgentAdaptPageProps {
  context: AgentHubContext;
  /** 三栏 original/preview 正文优先注入。 */
  initialSourceMarkdown?: string | null;
  onExit: () => void;
}

/**
 * Business Logic（为什么需要）:
 *   用户可以审阅跨 Agent 的转换差异，但当前不能从本页写入任何 CLI 文件。
 *
 * Code Logic（做什么）:
 *   blocked context 显示恢复动作；local-user 显示选择性预览表单和严格解析后的结果。
 */
export function CrossAgentAdaptPage(props: CrossAgentAdaptPageProps): JSX.Element {
  const { context, initialSourceMarkdown, onExit } = props;
  const { t } = useTranslation(['agentHub', 'common']);
  const c = useCrossAgentAdaptController({ context, t, initialSourceMarkdown });
  const contextBlocked = c.peerBlocked || c.scope !== 'user';

  return (
    <div className={styles.userDialogBody} data-testid="cross-agent-adapt-page">
      <header className={styles.userDialogHeader}>
        <div className={styles.userHeroCopy}>
          <h2 className={styles.userDialogTitle} id="cross-agent-adapt-title">
            {t('agentHub:crossAgent.pageTitle')}
          </h2>
          <p className={styles.userSectionDescription}>
            {t('agentHub:crossAgent.pageDescription')}
          </p>
        </div>
        <Button
          variant="secondary"
          size="sm"
          onClick={onExit}
          disabled={c.busy}
          data-testid="cross-agent-adapt-back"
        >
          {t('agentHub:crossAgent.back')}
        </Button>
      </header>

      <StatusMessage tone="info" live="off" data-testid="cross-agent-preview-only">
        {t('agentHub:crossAgent.previewOnly')}
      </StatusMessage>

      {contextBlocked ? (
        <StatusMessage
          tone="warn"
          data-testid="cross-agent-adapt-context-blocked"
          action={(
            <Button variant="secondary" size="sm" onClick={onExit}>
              {t('agentHub:crossAgent.backToLocalUser')}
            </Button>
          )}
        >
          {c.peerBlocked
            ? t('agentHub:crossAgent.errors.peerBlocked')
            : t('agentHub:crossAgent.errors.projectBlocked')}
        </StatusMessage>
      ) : null}

      {c.error ? (
        <StatusMessage tone="danger" data-testid="cross-agent-adapt-error">
          {c.error}
        </StatusMessage>
      ) : null}

      {c.contentError ? (
        <StatusMessage
          tone="warn"
          data-testid="cross-agent-adapt-content-error"
          action={(
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void c.refreshSourceContent()}
            >
              {t('common:action.retry')}
            </Button>
          )}
        >
          {c.contentError}
        </StatusMessage>
      ) : null}

      {!contextBlocked ? (
        <>
          <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-targets">
            <Card.Header>
              <span className={styles.sectionTitle}>
                {t('agentHub:crossAgent.targetsSection')}
              </span>
            </Card.Header>
            <Card.Body>
              <p className={styles.hint} data-testid="cross-agent-adapt-source">
                {t('agentHub:crossAgent.sourceLabel')}:{' '}
                <strong>{t(`agentHub:targets.${c.source}`)}</strong>
              </p>
              <fieldset
                className={styles.userTargetGrid}
                data-testid="cross-agent-adapt-destinations"
              >
                <legend>{t('agentHub:crossAgent.destinationsLabel')}</legend>
                {c.destinationOptions.map((target) => (
                  <label key={target} className={styles.filterField}>
                    <input
                      type="checkbox"
                      checked={c.destinations.includes(target)}
                      disabled={c.busy}
                      data-testid={`cross-agent-adapt-dest-${target}`}
                      onChange={() => c.toggleDestination(target)}
                    />
                    <span>{t(`agentHub:targets.${target}`)}</span>
                  </label>
                ))}
              </fieldset>
            </Card.Body>
          </Card>

          <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-scope">
            <Card.Header>
              <span className={styles.sectionTitle}>
                {t('agentHub:crossAgent.scopeSection')}
              </span>
            </Card.Header>
            <Card.Body>
              <p className={styles.hint} data-testid="cross-agent-adapt-scope-value">
                {t('agentHub:crossAgent.scopeUser')}
              </p>
              <label className={styles.filterField}>
                <input
                  type="checkbox"
                  checked={c.scopeConfirmed}
                  disabled={c.busy}
                  data-testid="cross-agent-adapt-scope-confirm"
                  onChange={(event) => c.setScopeConfirmed(event.currentTarget.checked)}
                />
                <span>{t('agentHub:crossAgent.scopeConfirmPreviewLabel')}</span>
              </label>
            </Card.Body>
          </Card>

          <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-content">
            <Card.Header>
              <div className={styles.userPlanHeader}>
                <span className={styles.sectionTitle}>
                  {t('agentHub:crossAgent.contentSection')}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  loading={c.contentLoading}
                  disabled={c.busy}
                  onClick={() => void c.refreshSourceContent()}
                  data-testid="cross-agent-adapt-reload-content"
                >
                  {t('agentHub:crossAgent.reloadContent')}
                </Button>
              </div>
            </Card.Header>
            <Card.Body>
              <p className={styles.hint} data-testid="cross-agent-adapt-content-chars">
                {t('agentHub:crossAgent.contentChars', {
                  count: c.sourceMarkdown.trim().length,
                })}
              </p>
              <label className={styles.filterField}>
                <span className={styles.hint}>{t('agentHub:crossAgent.contentLabel')}</span>
                <textarea
                  className={styles.blockBody}
                  value={c.sourceMarkdown}
                  disabled={c.busy}
                  rows={12}
                  data-testid="cross-agent-adapt-markdown"
                  onChange={(event) => c.setSourceMarkdown(event.currentTarget.value)}
                  aria-label={t('agentHub:crossAgent.contentLabel')}
                />
              </label>
            </Card.Body>
          </Card>

          {c.preview ? (
            <section
              className={styles.userPlanChanges}
              data-testid="cross-agent-adapt-preview-result"
              aria-label={t('agentHub:crossAgent.previewAria')}
            >
              {c.preview.needsAdaptation ? (
                <StatusMessage tone="warn" live="off">
                  {t('agentHub:crossAgent.needsAdaptation')}
                </StatusMessage>
              ) : null}
              {c.preview.destinations.map((row) => (
                <article
                  key={row.destination}
                  className={styles.userPlanChange}
                  data-testid={`cross-agent-adapt-preview-${row.destination}`}
                >
                  <div className={styles.userPlanHeader}>
                    <div>
                      <h3 className={styles.userTargetName}>
                        {t(`agentHub:targets.${row.destination}`)}
                      </h3>
                      <code className={styles.userPath}>{row.path || '—'}</code>
                    </div>
                    <Pill tone={adaptModeTone(row.mode)}>
                      {t(`agentHub:crossAgent.modes.${row.mode}`, {
                        defaultValue: row.mode,
                      })}
                    </Pill>
                  </div>
                  {row.partialBlockers.length > 0 ? (
                    <ul className={styles.userWarningList}>
                      {row.partialBlockers.map((blocker) => (
                        <li key={blocker}>{blocker}</li>
                      ))}
                    </ul>
                  ) : null}
                  {row.unifiedDiff == null ? (
                    <StatusMessage tone="warn" live="off">
                      {t('agentHub:crossAgent.diffUnavailable')}
                    </StatusMessage>
                  ) : row.unifiedDiff.length === 0 ? (
                    <StatusMessage tone="success" live="off">
                      {t('agentHub:crossAgent.noChanges')}
                    </StatusMessage>
                  ) : (
                    <pre
                      className={styles.blockBody}
                      data-testid={`cross-agent-adapt-diff-${row.destination}`}
                    >
                      {row.unifiedDiff}
                    </pre>
                  )}
                </article>
              ))}
            </section>
          ) : null}
        </>
      ) : null}

      <footer className={styles.dialogActions}>
        <Button
          variant="secondary"
          size="sm"
          disabled={c.busy}
          onClick={onExit}
          data-testid="cross-agent-adapt-close"
        >
          {t('common:action.cancel')}
        </Button>
        {!contextBlocked ? (
          <Button
            variant="primary"
            size="sm"
            loading={c.busy}
            disabled={!c.canPreview}
            title={c.previewBlockedReason ?? undefined}
            onClick={() => void c.runPreview()}
            data-testid="cross-agent-adapt-preview"
          >
            {t('agentHub:crossAgent.preview')}
          </Button>
        ) : null}
      </footer>
    </div>
  );
}
