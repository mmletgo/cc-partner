/**
 * Agent Hub 单一壳层（本机用户级）。
 *
 * Business Logic（为什么需要这个组件）:
 *   当前 project/peer 浏览上下文没有与单一项目或远端 owner 精确绑定的安全 API，因此 Shell
 *   只承诺本机用户级。Agent、指令 lane 与内容 Tab 是唯一可切换上下文；LAN 通过显式任务进入。
 *
 * Code Logic（这个组件做什么）:
 *   渲染受控 tablist 与两个 radiogroup；复用共享 roving 索引合同实现方向键/Home/End，
 *   并用关联 tabpanel 承载页面内容。无业务 API 调用。
 */

import {
  useRef,
  type KeyboardEvent,
  type MutableRefObject,
  type ReactElement,
  type ReactNode,
} from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { getRovingTabIndex, isRovingTabKey } from '@/lib/rovingTablist';
import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  AgentHubContext,
  AgentHubTab,
  InstructionLane,
} from '../context/agentHubContext';
import { getAgentHubContextCapability } from '../context/agentHubContext';
import styles from './AgentHubShell.module.css';

const AGENTS: AgentTarget[] = ['claude', 'codex', 'opencode'];
const TABS: AgentHubTab[] = ['instructions', 'skill', 'command', 'mcp', 'plugin'];
const ASSET_TABS = new Set<AgentHubTab>(['skill', 'command', 'mcp', 'plugin']);
const LANES: InstructionLane[] = ['common', 'adapted', 'exclusive'];
const PANEL_ID = 'agent-hub-active-panel';

export type AgentHubShellTabCounts = Partial<
  Record<'skill' | 'command' | 'mcp' | 'plugin', number>
>;

export interface AgentHubShellActions {
  onPull: () => void;
  onPush: () => void;
  onAdapt: () => void;
  pullDisabledReason?: string | null;
  pushDisabledReason?: string | null;
  adaptDisabledReason?: string | null;
}

export interface AgentHubShellProps {
  context: AgentHubContext;
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  actions: AgentHubShellActions;
  tabCounts?: AgentHubShellTabCounts | null;
  children: ReactNode;
}

/**
 * Business Logic（为什么需要）:
 *   tab/radio 组都必须恰好一个 tab stop，方向键移动焦点的同时提交选择。
 *
 * Code Logic（做什么）:
 *   校验共享按键 → 计算 wrap 索引 → 调用选择回调 → 聚焦目标按钮。
 */
function moveRovingSelection<T>(
  event: KeyboardEvent<HTMLButtonElement>,
  currentIndex: number,
  values: readonly T[],
  refs: MutableRefObject<Array<HTMLButtonElement | null>>,
  onSelect: (value: T) => void,
): void {
  if (!isRovingTabKey(event.key)) return;
  event.preventDefault();
  const nextIndex = getRovingTabIndex(currentIndex, event.key, values.length);
  const value = values[nextIndex];
  if (value === undefined) return;
  onSelect(value);
  refs.current[nextIndex]?.focus();
}

/** 渲染 Agent Hub 顶栏、可访问选择器与活动 tabpanel。 */
export function AgentHubShell(props: AgentHubShellProps): ReactElement {
  const { context, onContextChange, actions, tabCounts, children } = props;
  const { t } = useTranslation(['agentHub', 'common']);
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const laneRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const agentRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const activeTabIndex = Math.max(0, TABS.indexOf(context.tab));
  const activeLaneIndex = Math.max(0, LANES.indexOf(context.instructionLane));
  const activeAgentIndex = Math.max(0, AGENTS.indexOf(context.agent));
  const activeTabId = `agent-hub-tab-${context.tab}`;
  const capability = getAgentHubContextCapability(context);
  const contextDisabledReason =
    capability === 'pullOnly'
      ? t('agentHub:shell.peerPullOnly')
      : capability === 'unsupported'
        ? t('agentHub:shell.contextUnsupported')
        : null;
  const pullDisabledReason =
    actions.pullDisabledReason ??
    (capability === 'unsupported' ? contextDisabledReason : null);
  const pushDisabledReason =
    actions.pushDisabledReason ??
    (capability !== 'direct' ? contextDisabledReason : null);
  const adaptDisabledReason =
    actions.adaptDisabledReason ??
    (capability !== 'direct' ? contextDisabledReason : null);

  function handleTabChange(tab: AgentHubTab): void {
    onContextChange(
      tab === 'instructions' ? { tab } : { tab, instructionLane: 'common' },
    );
  }

  return (
    <div className={styles.shell} data-testid="agent-hub-shell">
      <div className={styles.chrome}>
        <div className={styles.rowBetween}>
          <div
            className={styles.segment}
            role="tablist"
            aria-label={t('agentHub:shell.tabsAria')}
            data-testid="agent-hub-tablist"
          >
            {TABS.map((tab, index) => {
              const selected = context.tab === tab;
              const count =
                ASSET_TABS.has(tab) && tabCounts
                  ? tabCounts[tab as keyof AgentHubShellTabCounts]
                  : undefined;
              const label = t(`agentHub:shell.tabs.${tab}`);
              return (
                <Button
                  key={tab}
                  ref={(node) => {
                    tabRefs.current[index] = node;
                  }}
                  id={`agent-hub-tab-${tab}`}
                  variant={selected ? 'secondary' : 'ghost'}
                  size="sm"
                  role="tab"
                  tabIndex={selected ? 0 : -1}
                  aria-selected={selected}
                  aria-controls={PANEL_ID}
                  onClick={() => handleTabChange(tab)}
                  onKeyDown={(event) =>
                    moveRovingSelection(event, index, TABS, tabRefs, handleTabChange)
                  }
                  data-testid={`agent-hub-tab-${tab}`}
                  data-count={typeof count === 'number' ? String(count) : undefined}
                >
                  {typeof count === 'number' ? `${label} (${count})` : label}
                </Button>
              );
            })}
          </div>

          <div
            className={styles.cluster}
            role="toolbar"
            aria-label={t('agentHub:shell.toolbarAria')}
            data-testid="agent-hub-toolbar"
          >
            <Button
              variant="secondary"
              size="sm"
              disabled={Boolean(pullDisabledReason)}
              onClick={actions.onPull}
              data-testid="agent-hub-action-pull"
            >
              {t('agentHub:shell.pull')}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={Boolean(pushDisabledReason)}
              onClick={actions.onPush}
              data-testid="agent-hub-action-push"
            >
              {t('agentHub:shell.push')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              disabled={Boolean(adaptDisabledReason)}
              onClick={actions.onAdapt}
              data-testid="agent-hub-action-adapt"
            >
              {t('agentHub:shell.adapt')}
            </Button>
          </div>
        </div>

        {pullDisabledReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-pull-reason">
            {pullDisabledReason}
          </p>
        ) : null}
        {pushDisabledReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-push-reason">
            {pushDisabledReason}
          </p>
        ) : null}
        {adaptDisabledReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-adapt-reason">
            {adaptDisabledReason}
          </p>
        ) : null}

        <div className={styles.row}>
          <span className={styles.label}>{t('agentHub:shell.scopeLabel')}</span>
          <span className={styles.scopeValue} data-testid="agent-hub-fixed-scope">
            {t('agentHub:shell.localDevice')} · {t('agentHub:shell.scopeUser')}
          </span>
          <span className={styles.scopeHint}>{t('agentHub:shell.localUserOnlyHint')}</span>
        </div>

        {context.tab === 'instructions' ? (
          <div className={styles.row}>
            <span className={styles.label}>{t('agentHub:shell.laneLabel')}</span>
            <div
              className={styles.segment}
              role="radiogroup"
              aria-label={t('agentHub:shell.laneAria')}
              data-testid="agent-hub-lane-switcher"
            >
              {LANES.map((lane, index) => {
                const selected = context.instructionLane === lane;
                return (
                  <Button
                    key={lane}
                    ref={(node) => {
                      laneRefs.current[index] = node;
                    }}
                    variant={selected ? 'secondary' : 'ghost'}
                    size="sm"
                    role="radio"
                    tabIndex={selected ? 0 : -1}
                    aria-checked={selected}
                    onClick={() => onContextChange({ instructionLane: lane })}
                    onKeyDown={(event) =>
                      moveRovingSelection(event, index, LANES, laneRefs, (next) =>
                        onContextChange({ instructionLane: next }),
                      )
                    }
                    data-testid={`agent-hub-lane-${lane}`}
                  >
                    {t(`agentHub:shell.lanes.${lane}`)}
                  </Button>
                );
              })}
            </div>
          </div>
        ) : null}

        <div className={styles.row}>
          <span className={styles.label}>{t('agentHub:shell.agentLabel')}</span>
          <div
            className={styles.segment}
            role="radiogroup"
            aria-label={t('agentHub:shell.agentAria')}
            data-testid="agent-hub-agent-switcher"
          >
            {AGENTS.map((agent, index) => {
              const selected = context.agent === agent;
              return (
                <Button
                  key={agent}
                  ref={(node) => {
                    agentRefs.current[index] = node;
                  }}
                  variant={selected ? 'secondary' : 'ghost'}
                  size="sm"
                  role="radio"
                  tabIndex={selected ? 0 : -1}
                  aria-checked={selected}
                  onClick={() => onContextChange({ agent })}
                  onKeyDown={(event) =>
                    moveRovingSelection(event, index, AGENTS, agentRefs, (next) =>
                      onContextChange({ agent: next }),
                    )
                  }
                  data-testid={`agent-hub-agent-${agent}`}
                >
                  {t(`agentHub:targets.${agent}`)}
                </Button>
              );
            })}
          </div>
        </div>
      </div>

      <div
        id={PANEL_ID}
        className={styles.body}
        role="tabpanel"
        tabIndex={0}
        aria-labelledby={activeTabId}
        data-active-tab-index={activeTabIndex}
        data-active-lane-index={activeLaneIndex}
        data-active-agent-index={activeAgentIndex}
        data-testid="agent-hub-shell-body"
      >
        {children}
      </div>
    </div>
  );
}
