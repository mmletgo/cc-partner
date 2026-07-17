/**
 * AgentLedgerWorkbenchChrome — Workbench 上 Agent 历史入口与 Drawer 挂载
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench.tsx 有行数硬顶；历史 drawer 与入口按钮下沉到 view，避免页面膨胀。
 *
 * Code Logic（这个组件做什么）:
 *   可选渲染工具栏按钮；始终可挂载 Drawer；不 import @/api。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { HistoryIcon } from '@/lib/icons';
import type { AgentLedgerPage, AgentLedgerSummary } from '@/lib/types/agentLedger';
import { AgentLedgerDrawer } from './AgentLedgerDrawer';
import styles from '../Workbench.module.css';

export interface AgentLedgerWorkbenchChromeProps {
  showTrigger: boolean;
  disabled: boolean;
  open: boolean;
  localOnlyAvailable: boolean;
  page: AgentLedgerPage | null;
  summary: AgentLedgerSummary | null;
  loading: boolean;
  loadingMore: boolean;
  error: string | null;
  onOpen: () => void;
  onClose: () => void;
  onLoadMore: () => void;
  onRefresh: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   组合入口按钮与二级 drawer。
 *
 * Code Logic（这个组件做什么）:
 *   showTrigger 时渲染 Button；始终渲染 AgentLedgerDrawer。
 */
export function AgentLedgerWorkbenchChrome(props: AgentLedgerWorkbenchChromeProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  return (
    <>
      {props.showTrigger ? (
        <Button
          className={styles.terminalActionButton}
          variant="secondary"
          size="sm"
          icon={<HistoryIcon />}
          title={t('workbench:agentLedger.open')}
          aria-label={t('workbench:agentLedger.open')}
          data-workbench-responsive-action="true"
          data-testid="agent-usage-stats-open"
          disabled={props.disabled}
          onClick={props.onOpen}
        >
          <span data-workbench-responsive-label="true">{t('workbench:agentLedger.open')}</span>
        </Button>
      ) : null}
      <AgentLedgerDrawer
        open={props.open}
        onClose={props.onClose}
        localOnlyAvailable={props.localOnlyAvailable}
        page={props.page}
        summary={props.summary}
        loading={props.loading}
        loadingMore={props.loadingMore}
        error={props.error}
        onLoadMore={props.onLoadMore}
        onRefresh={props.onRefresh}
      />
    </>
  );
}
