/**
 * Agent adapter catalog strip — pure view for Workbench automation / Orchestrator.
 *
 * Business Logic（为什么需要这个组件）:
 *   Settings 之外的自动化表面也必须展示 OpenCode bridge/completion/blocked，且 fail-closed。
 *
 * Code Logic（这个组件做什么）:
 *   只收 catalog props + optional preview navigate callback；纯展示，不直接调用 API 模块。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import {
  OPENCODE_RUNTIME_BRIDGE_REL_PATH,
  agentAdapterAvailabilityTone,
  agentAdapterBlockedReason,
  effectiveOpenCodeBridgeStatus,
  isAgentAdapterEffectivelyAvailable,
  agentProviderLabelKey,
} from '@/lib/agentAdapterPresentation';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import styles from '../Orchestrator.module.css';

export interface AgentAdapterCatalogStripProps {
  agentAdapters: OrchestratorAgentAdapterCatalogItem[];
  onOpenOpenCodeBridgePreview?: () => void;
}

/**
 * Business Logic: 渲染 redacted adapter 行（OpenCode 含 bridge 路径与 preview CTA）。
 * Code Logic: map adapters → list rows with Pill tone from shared helper。
 */
export function AgentAdapterCatalogStrip(
  props: AgentAdapterCatalogStripProps,
): ReactElement | null {
  const { agentAdapters, onOpenOpenCodeBridgePreview } = props;
  const { t } = useTranslation(['orchestrator', 'settings', 'workbench']);

  if (agentAdapters.length === 0) {
    return null;
  }

  const hasUnavailable = agentAdapters.some((item) => !isAgentAdapterEffectivelyAvailable(item));

  return (
    <details
      className={styles.adaptersDetails}
      aria-label={t('settings:automation.agentAdaptersAriaLabel')}
      data-testid="agent-adapter-catalog-strip"
      {...(hasUnavailable ? { open: true } : {})}
    >
      <summary className={`${styles.adaptersSummary} ${styles.groupHeader}`}>
        <span>{t('settings:automation.agentAdaptersTitle')}</span>
        <Pill tone="neutral">{agentAdapters.length}</Pill>
      </summary>
      <ul className={styles.runtimeList}>
        {agentAdapters.map((item) => {
          const isOpenCode = item.provider === 'openCodeVisible';
          const bridgeStatus = isOpenCode
            ? effectiveOpenCodeBridgeStatus(item)
            : (item.bridgeStatus ?? null);
          const blocked = agentAdapterBlockedReason(item);
          const tone = agentAdapterAvailabilityTone(item);
          const effectivelyAvailable = isAgentAdapterEffectivelyAvailable(item);
          const providerKey = agentProviderLabelKey(String(item.provider));
          const providerLabel = providerKey
            ? t(`orchestrator:${providerKey}`)
            : String(item.provider);
          const needsPreview =
            isOpenCode && bridgeStatus !== null && bridgeStatus !== 'ready';
          return (
            <li
              key={String(item.provider)}
              className={styles.runtimeItem}
              data-testid={`agent-adapter-${item.provider}`}
              data-provider={item.provider}
              data-bridge-status={bridgeStatus ?? undefined}
              data-completion={item.completionContract}
              data-effectively-available={effectivelyAvailable ? 'true' : 'false'}
            >
              <div>
                <strong>{providerLabel}</strong>
                <span>
                  {t('settings:automation.completionContract', {
                    contract: item.completionContract,
                  })}
                  {item.executable
                    ? ` · ${t('settings:automation.executable', { path: item.executable })}`
                    : ''}
                  {item.version
                    ? ` · ${t('settings:automation.version', { version: item.version })}`
                    : ''}
                  {item.supportEvidence
                    ? ` · ${t('settings:automation.supportEvidence', {
                        evidence: item.supportEvidence,
                      })}`
                    : ''}
                  {isOpenCode && bridgeStatus
                    ? ` · ${t(`settings:automation.bridgeStatus.${bridgeStatus}`)}`
                    : ''}
                  {isOpenCode
                    ? ` · ${t('settings:automation.bridgePath', {
                        path: OPENCODE_RUNTIME_BRIDGE_REL_PATH,
                      })}`
                    : ''}
                  {blocked
                    ? ` · ${t('settings:automation.blockedReason', { reason: blocked })}`
                    : ''}
                </span>
                {needsPreview && onOpenOpenCodeBridgePreview ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={onOpenOpenCodeBridgePreview}
                    data-testid={`open-code-bridge-preview-${item.provider}`}
                  >
                    {t('workbench:openCodeBridge.openPreview')}
                  </Button>
                ) : null}
              </div>
              <Pill tone={tone} dot>
                {effectivelyAvailable
                  ? t('settings:automation.adapterAvailable')
                  : isOpenCode && bridgeStatus === 'previewRequired'
                    ? t('settings:automation.bridgeStatus.previewRequired')
                    : t('settings:automation.adapterUnavailable')}
              </Pill>
            </li>
          );
        })}
      </ul>
    </details>
  );
}
