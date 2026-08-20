/**
 * Agent Hub 单一壳层（本机 / 远端 / 项目）。
 *
 * Business Logic（为什么需要这个组件）:
 *   Agent Hub 是跨设备、跨项目管理入口；上下文选择不能因为某个写动作尚未认证而消失。
 *   Shell 负责选择 owner，内容区和动作层再按能力证据决定只读、预览或写入。
 *
 * Code Logic（这个组件做什么）:
 *   渲染受控 tablist、提示词槽或存放面、Agent radiogroup；用户级只保留设备选择器与 Pull/Push，
 *   项目级不展示项目选择器、项目名或跨设备复制（资产随项目走），也不展示「范围 / 范围：项目」。
 *   用户级提示词展示三槽 lane；公共槽隐藏 Agent 切换。项目级提示词不展示三槽，
 *   始终显示 Agent 切换（仓库根多数 Agent 共用一份 AGENTS.md）。
 *   当前 tab 可重读时工具栏只保留一个「刷新」（提示词三栏、项目文件与资产列表同一入口）。
 *   Skill/Command 仓库面隐藏 Agent 切换。
 *   复用共享 roving 索引合同，并用关联 tabpanel 承载页面内容。无业务 API 调用。
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
import { allHubTargets } from '@/lib/agentCatalog';
import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  AgentHubContext,
  AgentHubTab,
  InstructionLane,
  PortableAssetLane,
} from '../context/agentHubContext';
import {
  DEFAULT_AGENT_HUB_CONTEXT,
  getAgentHubContextCapability,
  isPortableStoreTab,
} from '../context/agentHubContext';
import styles from './AgentHubShell.module.css';

const AGENTS: AgentTarget[] = allHubTargets();
const TABS: AgentHubTab[] = ['instructions', 'skill', 'command', 'mcp', 'plugin'];
const ASSET_TABS = new Set<AgentHubTab>(['skill', 'command', 'mcp', 'plugin']);
/** 提示词槽顺序：独有 → 适配 → 公共。 */
const LANES: InstructionLane[] = ['exclusive', 'adapted', 'common'];
/** Skill/Command 存放面：已装备 → 仓库。 */
const ASSET_LANES: PortableAssetLane[] = ['equipped', 'store'];
const PANEL_ID = 'agent-hub-active-panel';

export type AgentHubShellTabCounts = Partial<
  Record<'skill' | 'command' | 'mcp' | 'plugin', number>
>;

export interface AgentHubShellActions {
  onPull: () => void;
  onPush: () => void;
  pullDisabledReason?: string | null;
  pushDisabledReason?: string | null;
  /** 当前 tab 可重读时提供；提示词三栏与资产列表共用这一入口。 */
  onReload?: () => void;
  reloadBusy?: boolean;
}

/** 壳层远端设备摘要。 */
export interface AgentHubShellPeer {
  deviceId: string;
  name: string;
  online: boolean;
  /** health capabilities；用户级三栏按 agent-hub.user-instructions.v1 门闩。 */
  capabilities?: string[];
}

export interface AgentHubShellProps {
  context: AgentHubContext;
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  actions: AgentHubShellActions;
  peers: AgentHubShellPeer[];
  tabCounts?: AgentHubShellTabCounts | null;
  /** 生产路径锁定 user；Workbench 项目 Agent 锁定 project。不再提供 user|project 切换。 */
  scopeLock?: 'user' | 'project';
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
  const {
    context,
    onContextChange,
    actions,
    peers,
    tabCounts,
    children,
  } = props;
  const scopeLock = props.scopeLock ?? (context.scope === 'project' ? 'project' : 'user');
  const { t } = useTranslation(['agentHub', 'common']);
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const laneRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const assetLaneRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const agentRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const activeTabIndex = Math.max(0, TABS.indexOf(context.tab));
  const showCopyActions = scopeLock !== 'project';
  const showReload = typeof actions.onReload === 'function';
  const showToolbar = showCopyActions || showReload;
  const activeScopeIndex = scopeLock === 'project' ? 1 : 0;
  const activeLaneIndex = Math.max(0, LANES.indexOf(context.instructionLane));
  const activeAssetLaneIndex = Math.max(0, ASSET_LANES.indexOf(context.assetLane));
  const activeAgentIndex = Math.max(0, AGENTS.indexOf(context.agent));
  const showInstructionLanes = context.tab === 'instructions' && scopeLock !== 'project';
  const showAgentSwitcher =
    !(context.tab === 'instructions' && context.instructionLane === 'common' && scopeLock !== 'project') &&
    !(isPortableStoreTab(context.tab) && context.assetLane === 'store');
  const activeTabId = `agent-hub-tab-${context.tab}`;
  const capability = getAgentHubContextCapability(context);
  const projectMissing = context.scope === 'project' && context.projectKey === null;
  const contextDisabledReason = capability === 'unsupported'
    ? t('agentHub:shell.contextUnsupported')
    : projectMissing
      ? t('agentHub:shell.projectRequired')
      : null;
  const pullDisabledReason =
    actions.pullDisabledReason ?? contextDisabledReason;
  const pushDisabledReason =
    actions.pushDisabledReason ?? contextDisabledReason;

  function handleTabChange(tab: AgentHubTab): void {
    if (tab === 'instructions') {
      onContextChange({ tab });
      return;
    }
    const patch: Partial<AgentHubContext> = {
      tab,
      instructionLane: DEFAULT_AGENT_HUB_CONTEXT.instructionLane,
    };
    if (!isPortableStoreTab(tab) && isPortableStoreTab(context.tab)) {
      patch.assetLane = DEFAULT_AGENT_HUB_CONTEXT.assetLane;
    }
    onContextChange(patch);
  }

  return (
    <div
      className={styles.shell}
      data-testid="agent-hub-shell"
      data-scope-lock={scopeLock}
    >
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
                  variant={selected ? 'primary' : 'ghost'}
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

          {showToolbar ? (
          <div
            className={styles.cluster}
            role="toolbar"
            aria-label={t('agentHub:shell.toolbarAria')}
            data-testid="agent-hub-toolbar"
          >
            {showReload ? (
              <Button
                variant="secondary"
                size="sm"
                loading={Boolean(actions.reloadBusy)}
                onClick={() => actions.onReload?.()}
                data-testid="agent-hub-action-reload"
              >
                {t('common:action.refresh')}
              </Button>
            ) : null}
            {showCopyActions ? (
              <>
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
              </>
            ) : null}
          </div>
          ) : null}
        </div>

        {showCopyActions && pullDisabledReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-pull-reason">
            {pullDisabledReason}
          </p>
        ) : null}
        {showCopyActions && pushDisabledReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-push-reason">
            {pushDisabledReason}
          </p>
        ) : null}

        {scopeLock === 'project' ? null : (
          <div className={styles.row}>
            <label className={styles.cluster}>
              <span className={styles.label}>{t('agentHub:shell.deviceLabel')}</span>
              <select
                className={styles.select}
                aria-label={t('agentHub:shell.deviceAria')}
                value={context.deviceId ?? ''}
                onChange={(event) =>
                  onContextChange({ deviceId: event.currentTarget.value || null })
                }
                data-testid="agent-hub-device-select"
              >
                <option value="">{t('agentHub:shell.localDevice')}</option>
                {peers.map((peer) => (
                  <option key={peer.deviceId} value={peer.deviceId} disabled={!peer.online}>
                    {peer.online
                      ? peer.name
                      : `${peer.name} (${t('agentHub:shell.offline')})`}
                  </option>
                ))}
              </select>
            </label>
          </div>
        )}

        {showInstructionLanes ? (
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
                    variant={selected ? 'primary' : 'ghost'}
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

        {isPortableStoreTab(context.tab) ? (
          <div className={styles.row}>
            <span className={styles.label}>{t('agentHub:shell.assetLaneLabel')}</span>
            <div
              className={styles.segment}
              role="radiogroup"
              aria-label={t('agentHub:shell.assetLaneAria')}
              data-testid="agent-hub-asset-lane-switcher"
            >
              {ASSET_LANES.map((lane, index) => {
                const selected = context.assetLane === lane;
                return (
                  <Button
                    key={lane}
                    ref={(node) => {
                      assetLaneRefs.current[index] = node;
                    }}
                    variant={selected ? 'primary' : 'ghost'}
                    size="sm"
                    role="radio"
                    tabIndex={selected ? 0 : -1}
                    aria-checked={selected}
                    onClick={() => onContextChange({ assetLane: lane })}
                    onKeyDown={(event) =>
                      moveRovingSelection(event, index, ASSET_LANES, assetLaneRefs, (next) =>
                        onContextChange({ assetLane: next }),
                      )
                    }
                    data-testid={`agent-hub-asset-lane-${lane}`}
                  >
                    {t(`agentHub:shell.assetLanes.${lane}`)}
                  </Button>
                );
              })}
            </div>
          </div>
        ) : null}

        {showAgentSwitcher ? (
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
                    variant={selected ? 'primary' : 'ghost'}
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
        ) : null}
      </div>

      <div
        id={PANEL_ID}
        className={styles.body}
        role="tabpanel"
        tabIndex={0}
        aria-labelledby={activeTabId}
        data-active-tab-index={activeTabIndex}
        data-active-scope-index={activeScopeIndex}
        data-active-lane-index={activeLaneIndex}
        data-active-asset-lane-index={activeAssetLaneIndex}
        data-active-agent-index={activeAgentIndex}
        data-testid="agent-hub-shell-body"
      >
        {children}
      </div>
    </div>
  );
}
