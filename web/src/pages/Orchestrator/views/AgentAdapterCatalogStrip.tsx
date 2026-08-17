/**
 * Agent adapter catalog strip — pure view for Workbench automation / Orchestrator.
 *
 * Business Logic（为什么需要这个组件）:
 *   项目自动化操作面只应列出当前可选用的 Agent；不可用 adapter 的诊断留在 Settings。
 *
 * Code Logic（这个组件做什么）:
 *   过滤 effectively available 后渲染紧凑 chip 行；空列表返回 null。不直接调用 API 模块。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Pill } from '@/components/primitives';
import {
  effectiveOpenCodeBridgeStatus,
  listEffectivelyAvailableAgentAdapters,
  agentProviderLabelKey,
} from '@/lib/agentAdapterPresentation';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import styles from '../Orchestrator.module.css';

export interface AgentAdapterCatalogStripProps {
  agentAdapters: OrchestratorAgentAdapterCatalogItem[];
}

/**
 * Business Logic: 工作台自动化工具栏需要一眼看到本机当前能跑的 Agent。
 * Code Logic: filter available → Pill chips；保留 data-* 供测试与可访问性。
 */
export function AgentAdapterCatalogStrip(
  props: AgentAdapterCatalogStripProps,
): ReactElement | null {
  const { agentAdapters } = props;
  const { t } = useTranslation(['orchestrator']);
  const available = listEffectivelyAvailableAgentAdapters(agentAdapters);

  if (available.length === 0) {
    return null;
  }

  return (
    <div
      className={styles.availableAgents}
      role="group"
      aria-label={t('orchestrator:queue.availableAgentsAria')}
      data-testid="agent-adapter-catalog-strip"
    >
      <span className={styles.availableAgentsLabel}>
        {t('orchestrator:queue.availableAgents')}
      </span>
      <ul className={styles.availableAgentList}>
        {available.map((item) => {
          const isOpenCode = item.provider === 'openCodeVisible';
          const bridgeStatus = isOpenCode
            ? effectiveOpenCodeBridgeStatus(item)
            : (item.bridgeStatus ?? null);
          const providerKey = agentProviderLabelKey(String(item.provider));
          const providerLabel = providerKey
            ? t(`orchestrator:${providerKey}`)
            : String(item.provider);
          return (
            <li
              key={String(item.provider)}
              className={styles.availableAgentItem}
              data-testid={`agent-adapter-${item.provider}`}
              data-provider={item.provider}
              data-bridge-status={bridgeStatus ?? undefined}
              data-completion={item.completionContract}
              data-effectively-available="true"
            >
              <Pill tone="success" dot>
                {providerLabel}
              </Pill>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
