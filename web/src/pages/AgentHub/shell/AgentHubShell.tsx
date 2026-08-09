/**
 * Agent Hub 壳层 chrome（tab × scope × lane × agent）。
 *
 * Business Logic（为什么需要）:
 *   新 IA 以能力类型优先：先选 Tab，再选用户/项目范围，提示词再选三槽，最后选 Agent。
 *   工具栏提供拉取/推送/跨 Agent 适配入口。
 *
 * Code Logic（做什么）:
 *   pure 受控视图：仅渲染 props 并调用 onContextChange / actions；无 @/api。
 *   scope=user 显示设备选择；scope=project 显示项目选择并隐藏设备；
 *   instructionLane 仅 tab=instructions 时渲染；deviceId≠null 时禁用 Adapt。
 */

import type { ReactElement, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  AgentHubContext,
  AgentHubScope,
  AgentHubTab,
  InstructionLane,
} from '../context/agentHubContext';
import styles from './AgentHubShell.module.css';

const AGENTS: AgentTarget[] = ['claude', 'codex', 'opencode'];
const SCOPES: AgentHubScope[] = ['user', 'project'];
const TABS: AgentHubTab[] = ['instructions', 'skill', 'command', 'mcp', 'plugin'];
const LANES: InstructionLane[] = ['common', 'adapted', 'exclusive'];

/** 壳层 peer 摘要（本机由 deviceId=null 表示）。 */
export interface AgentHubShellPeer {
  deviceId: string;
  name: string;
  online: boolean;
}

/** 壳层项目选项（本机或远端身份 key）。 */
export interface AgentHubShellProject {
  key: string;
  label: string;
  remote: boolean;
}

/** 工具栏动作回调。 */
export interface AgentHubShellActions {
  onPull: () => void;
  onPush: () => void;
  onAdapt: () => void;
  /** 非空时禁用 Adapt 并作为 title/辅助文案（同机 only 等）。 */
  adaptDisabledReason?: string | null;
}

/**
 * AgentHubShell pure 视图 props。
 */
export interface AgentHubShellProps {
  context: AgentHubContext;
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  peers: AgentHubShellPeer[];
  projects: AgentHubShellProject[];
  actions: AgentHubShellActions;
  children: ReactNode;
}

/**
 * Business Logic: 渲染 Agent Hub 顶栏导航与内容 slot。
 * Code Logic: 全受控；层级 tab → scope → (lane) → agent；Adapt 在 peer 设备上下文禁用。
 */
export function AgentHubShell(props: AgentHubShellProps): ReactElement {
  const { context, onContextChange, peers, projects, actions, children } = props;
  const { t } = useTranslation(['agentHub', 'common']);

  const adaptDisabled =
    context.deviceId !== null || Boolean(actions.adaptDisabledReason);
  const adaptReason =
    actions.adaptDisabledReason ||
    (context.deviceId !== null ? t('agentHub:shell.adaptLocalOnly') : null);

  /**
   * Business Logic: 切到用户级时清空 projectKey。
   * Code Logic: 单次 patch 带 scope + 互斥清理。
   */
  function handleScopeChange(scope: AgentHubScope) {
    if (scope === 'user') {
      onContextChange({ scope: 'user', projectKey: null });
      return;
    }
    onContextChange({ scope: 'project', deviceId: null });
  }

  /**
   * Business Logic: 设备选择（空串 = 本机）。
   * Code Logic: 转 null/string 后 patch deviceId。
   */
  function handleDeviceChange(value: string) {
    const deviceId = value.trim().length > 0 ? value : null;
    onContextChange({ deviceId });
  }

  /**
   * Business Logic: 项目选择（空串 = 未选）。
   * Code Logic: 转 null/string 后 patch projectKey。
   */
  function handleProjectChange(value: string) {
    const projectKey = value.trim().length > 0 ? value : null;
    onContextChange({ projectKey });
  }

  /**
   * Business Logic: 切换 Tab；离开提示词时清 lane 为默认。
   */
  function handleTabChange(tab: AgentHubTab) {
    if (tab === 'instructions') {
      onContextChange({ tab });
      return;
    }
    onContextChange({ tab, instructionLane: 'common' });
  }

  return (
    <div className={styles.shell} data-testid="agent-hub-shell">
      <div className={styles.chrome}>
        {/* L1: Tab + toolbar */}
        <div className={styles.rowBetween}>
          <div
            className={styles.segment}
            role="tablist"
            aria-label={t('agentHub:shell.tabsAria')}
            data-testid="agent-hub-tablist"
          >
            {TABS.map((tab) => (
              <Button
                key={tab}
                variant={context.tab === tab ? 'secondary' : 'ghost'}
                size="sm"
                role="tab"
                aria-selected={context.tab === tab}
                onClick={() => handleTabChange(tab)}
                data-testid={`agent-hub-tab-${tab}`}
              >
                {t(`agentHub:shell.tabs.${tab}`)}
              </Button>
            ))}
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
              onClick={actions.onPull}
              data-testid="agent-hub-action-pull"
            >
              {t('agentHub:shell.pull')}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={actions.onPush}
              data-testid="agent-hub-action-push"
            >
              {t('agentHub:shell.push')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              disabled={adaptDisabled}
              title={adaptReason ?? undefined}
              onClick={() => {
                if (adaptDisabled) return;
                actions.onAdapt();
              }}
              data-testid="agent-hub-action-adapt"
            >
              {t('agentHub:shell.adapt')}
            </Button>
          </div>
        </div>

        {adaptDisabled && adaptReason ? (
          <p className={styles.adaptHint} data-testid="agent-hub-adapt-reason">
            {adaptReason}
          </p>
        ) : null}

        {/* L2: Scope + device/project */}
        <div className={styles.row}>
          <span className={styles.label}>{t('agentHub:shell.scopeLabel')}</span>
          <div
            className={styles.segment}
            role="tablist"
            aria-label={t('agentHub:shell.scopeAria')}
            data-testid="agent-hub-scope-switcher"
          >
            {SCOPES.map((scope) => (
              <Button
                key={scope}
                variant={context.scope === scope ? 'secondary' : 'ghost'}
                size="sm"
                role="tab"
                aria-selected={context.scope === scope}
                onClick={() => handleScopeChange(scope)}
                data-testid={`agent-hub-scope-${scope}`}
              >
                {scope === 'user'
                  ? t('agentHub:shell.scopeUser')
                  : t('agentHub:shell.scopeProject')}
              </Button>
            ))}
          </div>

          {context.scope === 'user' ? (
            <label className={styles.cluster}>
              <span className={styles.label}>{t('agentHub:shell.deviceLabel')}</span>
              <select
                className={styles.select}
                aria-label={t('agentHub:shell.deviceAria')}
                value={context.deviceId ?? ''}
                onChange={(event) => handleDeviceChange(event.currentTarget.value)}
                data-testid="agent-hub-device-select"
              >
                <option value="" data-testid="agent-hub-device-option-local">
                  {t('agentHub:shell.localDevice')}
                </option>
                {peers.map((peer) => (
                  <option
                    key={peer.deviceId}
                    value={peer.deviceId}
                    disabled={!peer.online}
                    data-testid={`agent-hub-device-option-${peer.deviceId}`}
                  >
                    {peer.online
                      ? peer.name
                      : `${peer.name} (${t('agentHub:shell.offline')})`}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <label className={styles.cluster}>
              <span className={styles.label}>{t('agentHub:shell.projectLabel')}</span>
              <select
                className={styles.select}
                aria-label={t('agentHub:shell.projectAria')}
                value={context.projectKey ?? ''}
                onChange={(event) => handleProjectChange(event.currentTarget.value)}
                data-testid="agent-hub-project-select"
              >
                <option value="">{t('agentHub:shell.projectPlaceholder')}</option>
                {projects.map((project) => (
                  <option
                    key={project.key}
                    value={project.key}
                    data-testid={`agent-hub-project-option-${project.key}`}
                  >
                    {project.remote
                      ? `${project.label} (${t('agentHub:shell.projectRemote')})`
                      : project.label}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>

        {/* L3: instruction lane — only on instructions tab */}
        {context.tab === 'instructions' ? (
          <div className={styles.row}>
            <span className={styles.label}>{t('agentHub:shell.laneLabel')}</span>
            <div
              className={styles.segment}
              role="tablist"
              aria-label={t('agentHub:shell.laneAria')}
              data-testid="agent-hub-lane-switcher"
            >
              {LANES.map((lane) => (
                <Button
                  key={lane}
                  variant={context.instructionLane === lane ? 'secondary' : 'ghost'}
                  size="sm"
                  role="tab"
                  aria-selected={context.instructionLane === lane}
                  onClick={() => onContextChange({ instructionLane: lane })}
                  data-testid={`agent-hub-lane-${lane}`}
                >
                  {t(`agentHub:shell.lanes.${lane}`)}
                </Button>
              ))}
            </div>
          </div>
        ) : null}

        {/* L4: Agent */}
        <div className={styles.row}>
          <span className={styles.label}>{t('agentHub:shell.agentLabel')}</span>
          <div
            className={styles.segment}
            role="tablist"
            aria-label={t('agentHub:shell.agentAria')}
            data-testid="agent-hub-agent-switcher"
          >
            {AGENTS.map((agent) => (
              <Button
                key={agent}
                variant={context.agent === agent ? 'secondary' : 'ghost'}
                size="sm"
                role="tab"
                aria-selected={context.agent === agent}
                onClick={() => onContextChange({ agent })}
                data-testid={`agent-hub-agent-${agent}`}
              >
                {t(`agentHub:targets.${agent}`)}
              </Button>
            ))}
          </div>
        </div>
      </div>

      <div className={styles.body} data-testid="agent-hub-shell-body">
        {children}
      </div>
    </div>
  );
}
