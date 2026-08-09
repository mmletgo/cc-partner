/**
 * Cross-agent adapt independent page (selective + full-volume).
 *
 * Business Logic（为什么需要）:
 *   阶段三适配入口必须是独立全页：选择性多目标指令适配，或全量单目标五类清单
 *   强制预览后 apply；peer 上下文 blocked。
 *
 * Code Logic（做什么）:
 *   pure 视图 + 本页 controller；mode toggle 切换 UI 区块。
 */

import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { AgentHubContext } from '../context/agentHubContext';
import styles from '../AgentHub.module.css';
import {
  adaptModeTone,
  type CrossAgentAdaptMode,
} from './crossAgentPresentation';
import {
  useCrossAgentAdaptController,
  type UseCrossAgentAdaptControllerResult,
} from './useCrossAgentAdaptController';

export interface CrossAgentAdaptPageProps {
  context: AgentHubContext;
  /** 三栏 original/preview 正文优先注入。 */
  initialSourceMarkdown?: string | null;
  onExit: () => void;
}

/**
 * Business Logic: 渲染适配全页（选择性 + Claude 全量强制预览）。
 * Code Logic: controller 驱动；sections 单滚动。
 */
export function CrossAgentAdaptPage(props: CrossAgentAdaptPageProps): JSX.Element {
  const { context, initialSourceMarkdown, onExit } = props;
  const { t } = useTranslation(['agentHub', 'common']);
  const c: UseCrossAgentAdaptControllerResult = useCrossAgentAdaptController({
    context,
    t,
    initialSourceMarkdown,
  });

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

      {c.peerBlocked ? (
        <StatusMessage tone="danger" data-testid="cross-agent-adapt-peer-blocked">
          {t('agentHub:crossAgent.errors.peerBlocked')}
        </StatusMessage>
      ) : null}

      {c.error ? (
        <StatusMessage tone="danger" data-testid="cross-agent-adapt-error">
          {c.error}
        </StatusMessage>
      ) : null}

      {c.contentError ? (
        <StatusMessage tone="warn" data-testid="cross-agent-adapt-content-error">
          {c.contentError}
        </StatusMessage>
      ) : null}

      {/* Mode toggle: selective | full */}
      <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-mode">
        <Card.Header>
          <span className={styles.sectionTitle}>{t('agentHub:crossAgent.modeSection')}</span>
        </Card.Header>
        <Card.Body>
          <fieldset className={styles.userTargetGrid} data-testid="cross-agent-adapt-mode-toggle">
            <legend>{t('agentHub:crossAgent.modeLabel')}</legend>
            <label className={styles.filterField}>
              <input
                type="radio"
                name="cross-agent-adapt-mode"
                checked={c.mode === 'selective'}
                disabled={c.busy || c.peerBlocked}
                data-testid="cross-agent-adapt-mode-selective"
                onChange={() => c.setMode('selective')}
              />
              <span>{t('agentHub:crossAgent.modeSelective')}</span>
            </label>
            <label className={styles.filterField}>
              <input
                type="radio"
                name="cross-agent-adapt-mode"
                checked={c.mode === 'full'}
                disabled={c.busy || c.peerBlocked}
                data-testid="cross-agent-adapt-mode-full"
                onChange={() => c.setMode('full')}
              />
              <span>{t('agentHub:crossAgent.modeFull')}</span>
            </label>
          </fieldset>
          {c.mode === 'full' ? (
            <p className={styles.hint} data-testid="cross-agent-adapt-full-hint">
              {t('agentHub:crossAgent.fullModeHint')}
            </p>
          ) : (
            <p className={styles.hint} data-testid="cross-agent-adapt-selective-hint">
              {t('agentHub:crossAgent.selectiveThreeSlotHint')}
            </p>
          )}
        </Card.Body>
      </Card>

      {/* 1. Source + destinations */}
      <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-targets">
        <Card.Header>
          <span className={styles.sectionTitle}>{t('agentHub:crossAgent.targetsSection')}</span>
        </Card.Header>
        <Card.Body>
          <p className={styles.hint} data-testid="cross-agent-adapt-source">
            {t('agentHub:crossAgent.sourceLabel')}:{' '}
            <strong>{t(`agentHub:targets.${c.source}`)}</strong>
          </p>
          {c.mode === 'selective' ? (
            <fieldset className={styles.userTargetGrid} data-testid="cross-agent-adapt-destinations">
              <legend>{t('agentHub:crossAgent.destinationsLabel')}</legend>
              {c.destinationOptions.map((target) => (
                <label key={target} className={styles.filterField}>
                  <input
                    type="checkbox"
                    checked={c.destinations.includes(target)}
                    disabled={c.busy || c.peerBlocked}
                    data-testid={`cross-agent-adapt-dest-${target}`}
                    onChange={() => c.toggleDestination(target)}
                  />
                  <span>{t(`agentHub:targets.${target}`)}</span>
                </label>
              ))}
            </fieldset>
          ) : (
            <fieldset className={styles.userTargetGrid} data-testid="cross-agent-adapt-full-destination">
              <legend>{t('agentHub:crossAgent.fullDestinationLabel')}</legend>
              {c.destinationOptions.map((target) => (
                <label key={target} className={styles.filterField}>
                  <input
                    type="radio"
                    name="cross-agent-full-destination"
                    checked={c.fullDestination === target}
                    disabled={c.busy || c.peerBlocked}
                    data-testid={`cross-agent-adapt-full-dest-${target}`}
                    onChange={() => c.setFullDestination(target)}
                  />
                  <span>{t(`agentHub:targets.${target}`)}</span>
                </label>
              ))}
            </fieldset>
          )}
        </Card.Body>
      </Card>

      {/* 2. Scope confirm */}
      <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-scope">
        <Card.Header>
          <span className={styles.sectionTitle}>{t('agentHub:crossAgent.scopeSection')}</span>
        </Card.Header>
        <Card.Body>
          <p className={styles.hint} data-testid="cross-agent-adapt-scope-value">
            {c.scope === 'user'
              ? t('agentHub:crossAgent.scopeUser')
              : t('agentHub:crossAgent.scopeProject', {
                  project: c.projectKey ?? '—',
                })}
          </p>
          {c.projectOptInNeeded ? (
            <StatusMessage tone="warn" live="off" data-testid="cross-agent-adapt-project-opt-in">
              {t('agentHub:crossAgent.projectOptInNeeded')}
            </StatusMessage>
          ) : null}
          {c.scope === 'project' && c.projectKey ? (
            <StatusMessage tone="info" live="off" data-testid="cross-agent-adapt-project-opt-in-hint">
              {t('agentHub:crossAgent.projectOptInHint')}
            </StatusMessage>
          ) : null}
          <label className={styles.filterField}>
            <input
              type="checkbox"
              checked={c.scopeConfirmed}
              disabled={c.busy || c.peerBlocked || c.projectOptInNeeded}
              data-testid="cross-agent-adapt-scope-confirm"
              onChange={(event) => c.setScopeConfirmed(event.currentTarget.checked)}
            />
            <span>{t('agentHub:crossAgent.scopeConfirmLabel')}</span>
          </label>
        </Card.Body>
      </Card>

      {/* 3. Content */}
      <Card variant="outlined" padding="md" data-testid="cross-agent-adapt-content">
        <Card.Header>
          <div className={styles.userPlanHeader}>
            <span className={styles.sectionTitle}>{t('agentHub:crossAgent.contentSection')}</span>
            <Button
              variant="ghost"
              size="sm"
              loading={c.contentLoading}
              disabled={c.busy || c.peerBlocked}
              onClick={() => {
                void c.refreshSourceContent();
              }}
              data-testid="cross-agent-adapt-reload-content"
            >
              {t('agentHub:crossAgent.reloadContent')}
            </Button>
          </div>
        </Card.Header>
        <Card.Body>
          <p className={styles.hint} data-testid="cross-agent-adapt-content-chars">
            {t('agentHub:crossAgent.contentChars', { count: c.sourceMarkdown.trim().length })}
          </p>
          <label className={styles.filterField}>
            <span className={styles.hint}>{t('agentHub:crossAgent.contentLabel')}</span>
            <textarea
              className={styles.blockBody}
              value={c.sourceMarkdown}
              disabled={c.busy || c.peerBlocked}
              rows={12}
              data-testid="cross-agent-adapt-markdown"
              onChange={(event) => c.setSourceMarkdown(event.currentTarget.value)}
              aria-label={t('agentHub:crossAgent.contentLabel')}
            />
          </label>
          {c.mode === 'selective' ? (
            <p className={styles.hint} data-testid="cross-agent-adapt-claude-block-stub">
              {t('agentHub:crossAgent.claudeBlockStub')}
            </p>
          ) : (
            <p className={styles.hint} data-testid="cross-agent-adapt-full-stub-note">
              {t('agentHub:crossAgent.fullStubNote')}
            </p>
          )}
        </Card.Body>
      </Card>

      {/* 4a. Selective preview */}
      {c.mode === 'selective' && c.preview ? (
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
                <Pill tone={adaptModeTone(row.mode as CrossAgentAdaptMode)}>
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
              {row.unifiedDiff ? (
                <pre className={styles.blockBody} data-testid={`cross-agent-adapt-diff-${row.destination}`}>
                  {row.unifiedDiff}
                </pre>
              ) : null}
              {!row.canApply ? (
                <StatusMessage tone="warn" live="off">
                  {t('agentHub:crossAgent.cannotApply')}
                </StatusMessage>
              ) : null}
            </article>
          ))}
        </section>
      ) : null}

      {/* 4b. Full plan items with include toggles */}
      {c.mode === 'full' && c.fullPlan ? (
        <section
          className={styles.userPlanChanges}
          data-testid="cross-agent-adapt-full-plan"
          aria-label={t('agentHub:crossAgent.fullPreviewAria')}
        >
          <p className={styles.hint} data-testid="cross-agent-adapt-full-plan-hash">
            {t('agentHub:crossAgent.planHash')}: <code>{c.fullPlan.planHash.slice(0, 12)}…</code>
            {' · '}
            {t('agentHub:crossAgent.generator')}: {c.fullPlan.generator}
          </p>
          {c.fullPlan.items.map((item) => (
            <article
              key={item.logicalKey}
              className={styles.userPlanChange}
              data-testid={`cross-agent-adapt-full-item-${item.logicalKey}`}
            >
              <div className={styles.userPlanHeader}>
                <div>
                  <label className={styles.filterField}>
                    <input
                      type="checkbox"
                      checked={item.included}
                      disabled={c.busy || c.peerBlocked}
                      data-testid={`cross-agent-adapt-full-include-${item.logicalKey}`}
                      onChange={() => c.toggleFullItemIncluded(item.logicalKey)}
                    />
                    <span className={styles.userTargetName}>
                      {t(`agentHub:crossAgent.kinds.${item.kind}`, {
                        defaultValue: item.kind,
                      })}
                      {' · '}
                      {item.logicalKey}
                    </span>
                  </label>
                  <code className={styles.userPath}>{item.path || '—'}</code>
                </div>
                <Pill tone={item.residualReason ? 'danger' : item.action === 'skip' ? 'warn' : 'success'}>
                  {item.action}
                </Pill>
              </div>
              {item.residualReason ? (
                <p className={styles.hint} data-testid={`cross-agent-adapt-full-residual-${item.logicalKey}`}>
                  {item.residualReason}
                </p>
              ) : null}
            </article>
          ))}
        </section>
      ) : null}

      {c.applyResults ? (
        <StatusMessage
          tone={c.applyResults.some((r) => r.status !== 'applied') ? 'warn' : 'success'}
          data-testid="cross-agent-adapt-apply-result"
        >
          <ul className={styles.userResultList}>
            {c.applyResults.map((row) => (
              <li key={`${row.destination}-${row.path}`}>
                {t(`agentHub:targets.${row.destination}`)} · {row.status} · {row.path}
                {row.errorCode ? ` (${row.errorCode})` : ''}
              </li>
            ))}
          </ul>
        </StatusMessage>
      ) : null}

      {c.fullApplyResults ? (
        <StatusMessage
          tone={c.fullApplyResults.some((r) => r.status !== 'applied' && r.status !== 'skipped') ? 'warn' : 'success'}
          data-testid="cross-agent-adapt-full-apply-result"
        >
          <ul className={styles.userResultList}>
            {c.fullApplyResults.map((row) => (
              <li key={`${row.logicalKey}-${row.path}`}>
                {row.kind} · {row.logicalKey} · {row.status}
                {row.errorCode ? ` (${row.errorCode})` : ''}
              </li>
            ))}
          </ul>
        </StatusMessage>
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
        <Button
          variant="secondary"
          size="sm"
          loading={c.busy}
          disabled={!c.canPreview}
          title={c.previewBlockedReason ?? undefined}
          onClick={() => {
            void c.runPreview();
          }}
          data-testid="cross-agent-adapt-preview"
        >
          {t('agentHub:crossAgent.preview')}
        </Button>
        <Button
          variant="primary"
          size="sm"
          loading={c.busy}
          disabled={!c.canApply}
          title={c.applyBlockedReason ?? undefined}
          onClick={() => {
            void c.runApply();
          }}
          data-testid="cross-agent-adapt-apply"
        >
          {c.mode === 'full'
            ? t('agentHub:crossAgent.applyFull', { count: c.applicableCount })
            : t('agentHub:crossAgent.apply', { count: c.applicableCount })}
        </Button>
      </footer>
    </div>
  );
}
