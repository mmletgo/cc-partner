/**
 * Workbench 状态卡叶子视图 —— 当前会话/项目/worktree 的状态摘要 + 重命名/关闭按钮。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspectorPane 顶部状态卡渲染抽到独立叶子组件，便于页面降到 ≤1200 行。
 *   状态卡是 inspectorPane 的一部分，但不是 tab 切换内容，因此单独成件；组件只接收 controller 派生的渲染数据与回调，
 *   不持有自己的状态，也不导入文件/Git 域。运行时长由 SessionRuntimeText 叶子自持 1 Hz 时钟，避免根重渲染。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染会话状态 Pill、项目/worktree/session 元信息 grid、session rename 输入与 close 按钮；
 *   - statusRuntime 行挂 SessionRuntimeText（startedAt/endedAt/running/visible/emptyValue）；
 *   - 暴露 WorkbenchStatusCardProps 类型，所有数据均来自 useWorkbenchTerminalController + Workbench.tsx 跨域共享。
 */
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { EditIcon, XIcon } from '@/lib/icons';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { AgentSessionProjection } from '@/lib/types/agentRuntime';
import {
  agentFreshnessI18nKey,
  agentPhaseI18nKey,
  agentPhaseTone,
  agentProviderShortLabel,
} from './agentPhasePresentation';
import styles from './Workbench.module.css';
import { SessionRuntimeText } from './SessionRuntimeText';

/**
 * Business Logic（为什么需要这个函数）:
 *   状态 Pill 需要把 session status 映射为稳定 tone，便于用户快速判断运行/退出/断开。
 *
 * Code Logic（这个函数做什么）:
 *   running→success，exited→neutral，disconnected→danger，其余状态使用 warn。
 */
function statusTone(status: string): 'neutral' | 'success' | 'warn' | 'danger' {
  if (status === 'running') return 'success';
  if (status === 'exited') return 'neutral';
  if (status === 'disconnected') return 'danger';
  return 'warn';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   最近打开时间、文件修改时间需要展示成用户本地可读格式。
 *
 * Code Logic（这个函数做什么）:
 *   使用浏览器本地化短日期时间；解析失败时回退原始字符串。
 */
function formatDateTime(value: string | null, emptyValue: string): string {
  if (!value) return emptyValue;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * 状态卡叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchTerminalController + Workbench.tsx 跨域共享派生；
 * 组件本身不持有状态、不导入文件/Git 域。
 */
export interface WorkbenchStatusCardProps {
  activeProject: WorkbenchProject | null;
  activeWorktree: WorkbenchWorktree | null;
  activeSession: WorkbenchSession | null;
  activeRootPath: string;
  remoteWriteDisabled: boolean;
  sessionNameDraft: string;
  setSessionNameDraft: (next: string) => void;
  handleRenameSession: () => Promise<void>;
  handleCloseSession: (sessionId: string) => Promise<void>;
  /**
   * 状态卡所属 inspector/workspace 表面是否可见。
   * 由 Workbench 从既有 active 状态派生（如 !terminalFullscreen），不使用 IntersectionObserver。
   */
  runtimeVisible: boolean;
  /** 当前 active terminal 的 Agent 投影（无则不展示 Agent 行）。 */
  activeAgent?: AgentSessionProjection | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 右侧 inspectorPane 顶部需要稳定的状态摘要，让用户在不切换 tab 的情况下看到当前项目/worktree/session 状态。
 *
 * Code Logic（这个组件做什么）:
 *   渲染状态 Pill + 11 行元信息 grid + rename 输入 + close 按钮；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchStatusCard(props: WorkbenchStatusCardProps) {
  const { t } = useTranslation(['workbench']);
  const {
    activeProject,
    activeWorktree,
    activeSession,
    activeRootPath,
    remoteWriteDisabled,
    sessionNameDraft,
    setSessionNameDraft,
    handleRenameSession,
    handleCloseSession,
    runtimeVisible,
    activeAgent = null,
  } = props;

  const emptyValue = t('workbench:emptyValue');
  const sessionStatusLabel = activeSession
    ? activeSession.status === 'running'
      ? t('workbench:sessionStatus.running')
      : activeSession.status === 'exited'
        ? t('workbench:sessionStatus.exited')
        : activeSession.status === 'disconnected'
          ? t('workbench:sessionStatus.disconnected')
          : activeSession.status
    : t('workbench:sessionStatus.none');

  const agentPhaseLabel = activeAgent
    ? t(`workbench:${agentPhaseI18nKey(activeAgent.phase)}`)
    : null;
  const agentFreshnessKey = activeAgent
    ? agentFreshnessI18nKey(activeAgent.freshness)
    : null;
  const agentStatusValue =
    activeAgent && agentPhaseLabel
      ? [
          agentProviderShortLabel(activeAgent.providerId),
          agentPhaseLabel,
          agentFreshnessKey ? t(`workbench:${agentFreshnessKey}`) : null,
        ]
          .filter(Boolean)
          .join(' · ')
      : emptyValue;

  // Business Logic: 元信息以 (label key, value) 数组驱动；runtime 行挂叶子 SessionRuntimeText 自持时钟。
  const rows: Array<{ label: string; value: ReactNode }> = [
    { label: t('workbench:statusDevice'), value: activeProject?.deviceName ?? emptyValue },
    { label: t('workbench:statusProject'), value: activeProject?.name ?? emptyValue },
    { label: t('workbench:statusWorktree'), value: activeWorktree?.name ?? emptyValue },
    { label: t('workbench:statusProjectPath'), value: activeRootPath || emptyValue },
    { label: t('workbench:statusSession'), value: activeSession?.name ?? emptyValue },
    { label: t('workbench:statusCommand'), value: activeSession?.command ?? emptyValue },
    { label: t('workbench:statusState'), value: sessionStatusLabel },
    {
      label: t('workbench:statusAgent'),
      value:
        activeAgent && agentPhaseLabel ? (
          <Pill tone={agentPhaseTone(activeAgent.phase)} dot>
            {agentStatusValue}
          </Pill>
        ) : (
          emptyValue
        ),
    },
    {
      label: t('workbench:statusRuntime'),
      value: (
        <SessionRuntimeText
          startedAt={activeSession?.startedAt ?? null}
          endedAt={activeSession?.exitedAt ?? null}
          running={activeSession?.status === 'running'}
          visible={runtimeVisible}
          emptyValue={emptyValue}
        />
      ),
    },
    {
      label: t('workbench:statusSize'),
      value: activeSession ? `${activeSession.cols} × ${activeSession.rows}` : emptyValue,
    },
    {
      label: t('workbench:statusStarted'),
      value: formatDateTime(activeSession?.startedAt ?? null, emptyValue),
    },
    { label: t('workbench:statusExit'), value: activeSession?.exitCode !== null && activeSession?.exitCode !== undefined ? String(activeSession.exitCode) : emptyValue },
  ];

  return (
    <Card className={styles.statusCard} padding="sm">
      <div className={styles.cardTitleRow}>
        <h3 className={styles.cardTitle}>{t('workbench:sessionStatusTitle')}</h3>
        <Pill tone={activeSession ? statusTone(activeSession.status) : 'neutral'} dot>
          {sessionStatusLabel}
        </Pill>
      </div>
      <dl className={styles.statusGrid}>
        {rows.map((row) => (
          <div key={row.label}>
            <dt>{row.label}</dt>
            <dd>{row.value}</dd>
          </div>
        ))}
      </dl>
      <div className={styles.statusActions}>
        <Input
          value={sessionNameDraft}
          onChange={(event) => setSessionNameDraft(event.target.value)}
          placeholder={t('workbench:sessionNamePlaceholder')}
          size="sm"
          disabled={!activeSession || remoteWriteDisabled}
        />
        <div className={styles.statusButtonRow}>
          <Button
            size="sm"
            variant="secondary"
            icon={<EditIcon />}
            disabled={!activeSession || !sessionNameDraft.trim() || remoteWriteDisabled}
            onClick={() => void handleRenameSession()}
          >
            {t('workbench:renameSession')}
          </Button>
          <Button
            size="sm"
            variant="danger"
            icon={<XIcon />}
            disabled={!activeSession || remoteWriteDisabled}
            onClick={() => activeSession && void handleCloseSession(activeSession.id)}
          >
            {t('workbench:closeTerminal')}
          </Button>
        </div>
      </div>
    </Card>
  );
}
