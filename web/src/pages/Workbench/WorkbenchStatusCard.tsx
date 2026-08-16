/**
 * Workbench 状态卡叶子视图 —— 当前会话/项目/worktree 的状态摘要 + 重命名/关闭按钮。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspectorPane 顶部状态卡渲染抽到独立叶子组件，便于页面降到 ≤1200 行。
 *   状态卡是 inspectorPane 的一部分，但不是 tab 切换内容，因此单独成件；组件只接收 controller 派生的渲染数据与回调，
 *   不持有自己的状态，也不导入文件/Git 域。运行时长由 SessionRuntimeText 叶子自持 1 Hz 时钟，避免根重渲染。
 *   速率优先用 live usage，缺省回退 ledger；上下文用量只用 live.contextLength（末轮占用），
 *   禁止把累计计费 token 当占用。上下文长度用 provider 窗口或 model 查表。null 显示「未提供」。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染会话状态 Pill、项目/worktree/session 元信息 grid、session rename 输入与 close 按钮；
 *   - statusRuntime 行挂 SessionRuntimeText（startedAt/endedAt/running/visible/emptyValue）；
 *   - TokenRateRow 用 billed tokens/duration；SessionQualityRow 用首响平均 + 缓存命中；
 *   - ContextMeter 用 occupancy + window。
 *   - 暴露 WorkbenchStatusCardProps 类型，所有数据均来自 useWorkbenchTerminalController + Workbench.tsx 跨域共享。
 */
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { ContextMeter, SessionQualityRow, TokenRateRow } from '@/components/domain/WorkbenchStatusCard';
import type { ProgressBarTone } from '@/components/primitives/ProgressBar';
import { DEFAULT_CONTEXT_WINDOW, resolveContextWindow } from '@/lib/agent/modelContextWindow';
import { EditIcon, XIcon } from '@/lib/icons';
import { computeCacheHitRate } from '@/lib/schemas/tokenStats';
import type { AgentLedgerEntry } from '@/lib/types/agentLedger';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { AgentSessionProjection } from '@/lib/types/agentRuntime';
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
 * Business Logic（为什么需要这个函数）:
 *   终态历史平均速率 = tokens / durationSec；duration 必须为正数才有效。
 *
 * Code Logic（这个函数做什么）:
 *   durationMs > 0 时计算 tok/s；否则返回 null（避免除零或负值）。
 */
function computeTokenRate(tokens: number | null | undefined, durationMs: number): number | null {
  if (tokens == null || !Number.isFinite(tokens) || tokens <= 0) return null;
  if (!Number.isFinite(durationMs) || durationMs <= 0) return null;
  return (tokens / durationMs) * 1000;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live 平均 tok/s 用 agent.startedAt 到 extractedAt（终态优先 endedAt），避免绑 1Hz 时钟。
 *
 * Code Logic（这个函数做什么）:
 *   解析 RFC3339；end<=start 或非法返回 0。
 */
function computeLiveDurationMs(
  startedAt: string | null | undefined,
  extractedAt: string,
  endedAt: string | null | undefined,
): number {
  if (!startedAt) return 0;
  const start = Date.parse(startedAt);
  if (!Number.isFinite(start)) return 0;
  const endSource = endedAt && endedAt.trim().length > 0 ? endedAt : extractedAt;
  const end = Date.parse(endSource);
  if (!Number.isFinite(end) || end <= start) return 0;
  return end - start;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   ProgressBar tone 不能硬编码到叶子组件；StatusCard 根据 phase 与百分比阈值统一决策。
 *
 * Code Logic（这个函数做什么）:
 *   终态 → success；其它阶段按百分比阈值：<0.6 accent，<0.85 warn，>=0.85 danger。
 */
function decideContextTone(
  isTerminal: boolean,
  pct: number | null,
): ProgressBarTone {
  if (isTerminal) return 'success';
  if (pct == null) return 'accent';
  if (pct >= 0.85) return 'danger';
  if (pct >= 0.6) return 'warn';
  return 'accent';
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
  /** 当前 active terminal 的 Agent 投影（含可选 live usage）。 */
  activeAgent?: AgentSessionProjection | null;
  /**
   * 当前 active agent session 的 ledger 单行（终态回退；live usage 优先）。
   * 未命中时为 null。
   */
  ledgerEntry?: AgentLedgerEntry | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 右侧 inspectorPane 顶部需要稳定的状态摘要，让用户在不切换 tab 的情况下看到当前项目/worktree/session 状态。
 *
 * Code Logic（这个组件做什么）:
 *   渲染状态 Pill + 6 行元信息 grid + TokenRateRow + ContextMeter + rename 输入 + close 按钮；
 *   不持有状态、不调用 workbenchApi。
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
    ledgerEntry = null,
  } = props;

  const emptyValue = t('workbench:emptyValue');
  const unavailableLabel = t('workbench:metricsUnavailable');
  const noWindowLabel = t('workbench:metricsContextNoWindow');

  const sessionStatusLabel = activeSession
    ? activeSession.status === 'running'
      ? t('workbench:sessionStatus.running')
      : activeSession.status === 'exited'
        ? t('workbench:sessionStatus.exited')
        : activeSession.status === 'disconnected'
          ? t('workbench:sessionStatus.disconnected')
          : activeSession.status
    : t('workbench:sessionStatus.none');

  const isAgentTerminal =
    activeAgent?.phase === 'completed' ||
    activeAgent?.phase === 'failed' ||
    activeAgent?.phase === 'disconnected';

  const liveUsage = activeAgent?.usage ?? null;
  const inputTokens = liveUsage?.inputTokens ?? ledgerEntry?.inputTokens ?? null;
  const outputTokens = liveUsage?.outputTokens ?? ledgerEntry?.outputTokens ?? null;
  const modelId = liveUsage?.modelId ?? ledgerEntry?.modelId ?? null;

  const wallDurationMs = liveUsage
    ? computeLiveDurationMs(
        activeAgent?.startedAt ?? null,
        liveUsage.extractedAt,
        isAgentTerminal ? (activeAgent?.endedAt ?? null) : null,
      )
    : (ledgerEntry?.durationMs ?? 0);
  // 速率分母对齐 ccstatusline：用户→助手有效生成区间；没有区间才回落墙钟。
  const activeDurationMs =
    liveUsage?.activeDurationMs != null &&
    Number.isFinite(liveUsage.activeDurationMs) &&
    liveUsage.activeDurationMs > 0
      ? liveUsage.activeDurationMs
      : null;
  const durationMs = activeDurationMs ?? wallDurationMs;

  // 平均 tok/s：billed input/output ÷ 有效生成时长。
  const speedInTps = computeTokenRate(inputTokens, durationMs);
  const speedOutTps = computeTokenRate(outputTokens, durationMs);

  const firstTokenAvgMs =
    liveUsage?.firstTokenAvgMs != null &&
    Number.isFinite(liveUsage.firstTokenAvgMs) &&
    liveUsage.firstTokenAvgMs > 0
      ? liveUsage.firstTokenAvgMs
      : null;
  const cacheHitRate = computeCacheHitRate(
    liveUsage?.cacheReadTokens ?? ledgerEntry?.cacheReadTokens ?? null,
    liveUsage?.inputTokens ?? ledgerEntry?.inputTokens ?? null,
  );

  // 用量 = 末轮 occupancy（live.contextLength）；禁止把累计计费 token 当占用。
  const contextUsed =
    liveUsage?.contextLength != null && Number.isFinite(liveUsage.contextLength)
      ? liveUsage.contextLength
      : null;
  const lookedUpWindow = resolveContextWindow(modelId);
  const contextWindow =
    liveUsage?.contextWindow != null &&
    Number.isFinite(liveUsage.contextWindow) &&
    liveUsage.contextWindow > 0
      ? liveUsage.contextWindow
      : lookedUpWindow ?? (contextUsed != null ? DEFAULT_CONTEXT_WINDOW : null);
  const pct =
    contextWindow != null && contextWindow > 0 && contextUsed != null && contextUsed > 0
      ? Math.min(1, contextUsed / contextWindow)
      : null;
  const contextTone = decideContextTone(isAgentTerminal, pct);

  // Business Logic: 元信息以 (label key, value) 数组驱动；runtime 行挂叶子 SessionRuntimeText 自持时钟。
  // 已移除的字段：statusCommand / statusState（lifecycle 由 Pill 表达）/ statusAgent / statusSize / statusExit。
  // 改由 TokenRateRow + ContextMeter 表达 agent session 指标。
  const rows: Array<{ label: string; value: ReactNode }> = [
    { label: t('workbench:statusDevice'), value: activeProject?.deviceName ?? emptyValue },
    { label: t('workbench:statusProject'), value: activeProject?.name ?? emptyValue },
    { label: t('workbench:statusWorktree'), value: activeWorktree?.name ?? emptyValue },
    { label: t('workbench:statusProjectPath'), value: activeRootPath || emptyValue },
    { label: t('workbench:statusSession'), value: activeSession?.name ?? emptyValue },
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
      label: t('workbench:statusStarted'),
      value: formatDateTime(activeSession?.startedAt ?? null, emptyValue),
    },
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
      <TokenRateRow
        speedInTps={speedInTps}
        speedOutTps={speedOutTps}
        unavailableLabel={unavailableLabel}
      />
      <SessionQualityRow
        firstTokenAvgMs={firstTokenAvgMs}
        cacheHitRate={cacheHitRate}
        unavailableLabel={unavailableLabel}
      />
      <ContextMeter
        contextUsed={contextUsed}
        contextWindow={contextWindow}
        unavailableLabel={unavailableLabel}
        noWindowLabel={noWindowLabel}
        tone={contextTone}
      />
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