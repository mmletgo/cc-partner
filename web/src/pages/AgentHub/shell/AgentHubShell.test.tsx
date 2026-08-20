// @vitest-environment jsdom
/**
 * AgentHubShell pure view 测试。
 *
 * Business Logic: 锁定本机用户级壳层、能力门禁与三组 roving 键盘合同。
 * Code Logic: 注入 props；无 @/api；I18nextProvider 仅渲染标签。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { allHubTargets } from '@/lib/agentCatalog';
import i18n from '@/i18n';
import {
  DEFAULT_AGENT_HUB_CONTEXT,
  type AgentHubContext,
} from '../context/agentHubContext';
import { AgentHubShell, type AgentHubShellProps } from './AgentHubShell';

const shellDir = dirname(fileURLToPath(import.meta.url));

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic: 构造可交互的 shell 快照。
 * Code Logic: 默认 local/user/claude；外部上下文仅用于验证 fail-closed。
 */
function buildProps(overrides: Partial<AgentHubShellProps> = {}): AgentHubShellProps {
  return {
    context: { ...DEFAULT_AGENT_HUB_CONTEXT },
    onContextChange: vi.fn(),
    actions: {
      onPull: vi.fn(),
      onPush: vi.fn(),
    },
    peers: [],
    children: <div data-testid="shell-slot">content</div>,
    ...overrides,
  };
}

/**
 * Business Logic: 统一挂载 i18n + shell。
 * Code Logic: I18nextProvider 包装。
 */
function renderShell(overrides: Partial<AgentHubShellProps> = {}) {
  const props = buildProps(overrides);
  const result = render(
    <I18nextProvider i18n={i18n}>
      <AgentHubShell {...props} />
    </I18nextProvider>,
  );
  return { ...result, props };
}

describe('AgentHubShell', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(shellDir, './AgentHubShell.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('agent switcher lists catalog hub targets including grok and gemini', () => {
    renderShell({
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane: 'adapted' },
    });
    expect(screen.getByTestId('agent-hub-agent-grok')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-agent-gemini')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-agent-cursor')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-agent-pi')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-agent-switcher').querySelectorAll('[role="radio"]')).toHaveLength(
      allHubTargets().length,
    );
  });

  test('click Codex emits onContextChange({ agent: "codex" }) in an agent-specific lane', () => {
    const onContextChange = vi.fn();
    renderShell({
      onContextChange,
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane: 'adapted' },
    });
    fireEvent.click(screen.getByTestId('agent-hub-agent-codex'));
    expect(onContextChange).toHaveBeenCalledWith({ agent: 'codex' });
  });

  test('common instruction lane hides agent switcher while agent-specific lanes show it', () => {
    const { rerender, props } = renderShell({
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane: 'common' },
    });
    expect(screen.queryByTestId('agent-hub-agent-switcher')).toBeNull();
    for (const instructionLane of ['adapted', 'exclusive'] as const) {
      rerender(
        <I18nextProvider i18n={i18n}>
          <AgentHubShell
            {...props}
            context={{ ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane }}
          />
        </I18nextProvider>,
      );
      expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
    }
  });

  test('instructions tab shows lane switcher ordered exclusive → adapted → common', () => {
    const onContextChange = vi.fn();
    const { rerender, props } = renderShell({ onContextChange });
    expect(screen.getByTestId('agent-hub-lane-switcher')).toBeTruthy();
    const lanes = Array.from(
      screen.getByTestId('agent-hub-lane-switcher').querySelectorAll('[role="radio"]'),
    ).map((node) => node.getAttribute('data-testid'));
    expect(lanes).toEqual([
      'agent-hub-lane-exclusive',
      'agent-hub-lane-adapted',
      'agent-hub-lane-common',
    ]);
    fireEvent.click(screen.getByTestId('agent-hub-lane-adapted'));
    expect(onContextChange).toHaveBeenCalledWith({ instructionLane: 'adapted' });

    fireEvent.click(screen.getByTestId('agent-hub-tab-skill'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'skill',
      instructionLane: 'exclusive',
    });

    const skillContext: AgentHubContext = {
      ...DEFAULT_AGENT_HUB_CONTEXT,
      tab: 'skill',
      instructionLane: 'exclusive',
    };
    rerender(
      <I18nextProvider i18n={i18n}>
        <AgentHubShell {...props} context={skillContext} onContextChange={onContextChange} />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('agent-hub-lane-switcher')).toBeNull();
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
  });

  test('skill and command tabs show equipped/store switcher between scope and agent', () => {
    const onContextChange = vi.fn();
    const skillContext: AgentHubContext = {
      ...DEFAULT_AGENT_HUB_CONTEXT,
      tab: 'skill',
    };
    const { rerender, props } = renderShell({ onContextChange, context: skillContext });
    expect(screen.getByTestId('agent-hub-asset-lane-switcher')).toBeTruthy();
    const lanes = Array.from(
      screen.getByTestId('agent-hub-asset-lane-switcher').querySelectorAll('[role="radio"]'),
    ).map((node) => node.getAttribute('data-testid'));
    expect(lanes).toEqual([
      'agent-hub-asset-lane-equipped',
      'agent-hub-asset-lane-store',
    ]);
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
    fireEvent.click(screen.getByTestId('agent-hub-asset-lane-store'));
    expect(onContextChange).toHaveBeenCalledWith({ assetLane: 'store' });

    rerender(
      <I18nextProvider i18n={i18n}>
        <AgentHubShell
          {...props}
          context={{ ...skillContext, assetLane: 'store' }}
          onContextChange={onContextChange}
        />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('agent-hub-agent-switcher')).toBeNull();
    fireEvent.click(screen.getByTestId('agent-hub-tab-command'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'command',
      instructionLane: 'exclusive',
    });

    rerender(
      <I18nextProvider i18n={i18n}>
        <AgentHubShell
          {...props}
          context={{ ...DEFAULT_AGENT_HUB_CONTEXT, tab: 'mcp' }}
          onContextChange={onContextChange}
        />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('agent-hub-asset-lane-switcher')).toBeNull();
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
  });

  test('leaving skill store for mcp resets assetLane', () => {
    const onContextChange = vi.fn();
    renderShell({
      onContextChange,
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, tab: 'skill', assetLane: 'store' },
    });
    fireEvent.click(screen.getByTestId('agent-hub-tab-mcp'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'mcp',
      instructionLane: 'exclusive',
      assetLane: 'equipped',
    });
  });

  test('project scope lock hides project identity, copy actions, device picker, and scope copy', () => {
    renderShell({
      scopeLock: 'project',
    });
    expect(screen.queryByTestId('agent-hub-scope-lock')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-project-lock')).toBeNull();
    expect(screen.queryByText('Scope: project')).toBeNull();
    expect(screen.queryByText(/^Scope$/)).toBeNull();
    expect(screen.getByTestId('agent-hub-shell').getAttribute('data-scope-lock')).toBe('project');
    expect(screen.queryByTestId('agent-hub-frozen-project')).toBeNull();
    expect(screen.queryByText('Project')).toBeNull();
    expect(screen.queryByTestId('agent-hub-device-select')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-switcher')).toBeNull();
    expect(screen.queryByTestId('agent-hub-project-select')).toBeNull();
    expect(screen.queryByTestId('agent-hub-toolbar')).toBeNull();
    expect(screen.queryByTestId('agent-hub-action-pull')).toBeNull();
    expect(screen.queryByTestId('agent-hub-action-push')).toBeNull();
    expect(screen.queryByTestId('agent-hub-push-reason')).toBeNull();
    expect(screen.queryByTestId('agent-hub-pull-reason')).toBeNull();
  });

  test('project lock hides instruction lanes and keeps the agent switcher', () => {
    renderShell({
      scopeLock: 'project',
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        scope: 'project',
        projectKey: 'wb-1',
        tab: 'instructions',
        instructionLane: 'common',
      },
    });
    expect(screen.queryByTestId('agent-hub-lane-switcher')).toBeNull();
    expect(screen.getByTestId('agent-hub-agent-switcher')).toBeTruthy();
  });

  test('project lock shows a single reload without Pull or Push', () => {
    const onReload = vi.fn();
    renderShell({
      scopeLock: 'project',
      actions: {
        onPull: vi.fn(),
        onPush: vi.fn(),
        onReload,
      },
    });
    fireEvent.click(screen.getByTestId('agent-hub-action-reload'));
    expect(onReload).toHaveBeenCalledOnce();
    expect(screen.queryByTestId('agent-hub-action-pull')).toBeNull();
    expect(screen.queryByTestId('agent-hub-action-push')).toBeNull();
  });

  test('user shell shows device selector and hides scope copy', () => {
    renderShell();
    expect(screen.getByTestId('agent-hub-device-select')).toBeTruthy();
    expect(screen.queryByTestId('agent-hub-scope-lock')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-user-lock')).toBeNull();
    expect(screen.queryByText('Scope: user')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-switcher')).toBeNull();
    expect(screen.queryByTestId('agent-hub-project-select')).toBeNull();
    expect(screen.queryByTestId('agent-hub-scope-project')).toBeNull();
  });

  test('peer context keeps Pull and Push and does not expose Adapt toolbar button', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    renderShell({
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        deviceId: 'peer-online',
      },
      actions: {
        onPull,
        onPush,
      },
    });

    expect((screen.getByTestId('agent-hub-action-pull') as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByTestId('agent-hub-action-push') as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByTestId('agent-hub-action-adapt')).toBeNull();
    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    fireEvent.click(screen.getByTestId('agent-hub-action-push'));
    expect(onPull).toHaveBeenCalledOnce();
    expect(onPush).toHaveBeenCalledOnce();
  });

  test('project context hides Pull/Push because assets follow the project', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    renderShell({
      context: {
        ...DEFAULT_AGENT_HUB_CONTEXT,
        scope: 'project',
        projectKey: 'local:p1',
      },
      actions: { onPull, onPush },
    });

    expect(screen.queryByTestId('agent-hub-action-pull')).toBeNull();
    expect(screen.queryByTestId('agent-hub-action-push')).toBeNull();
    expect(screen.queryByTestId('agent-hub-toolbar')).toBeNull();
    expect(screen.queryByTestId('agent-hub-action-adapt')).toBeNull();
    expect(screen.queryByTestId('agent-hub-frozen-project')).toBeNull();
    expect(screen.queryByTestId('agent-hub-project-select')).toBeNull();
  });

  test('five tabs emit tab patch; skill selects skill', () => {
    const onContextChange = vi.fn();
    renderShell({ onContextChange });

    fireEvent.click(screen.getByTestId('agent-hub-tab-skill'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'skill',
      instructionLane: 'exclusive',
    });

    fireEvent.click(screen.getByTestId('agent-hub-tab-instructions'));
    expect(onContextChange).toHaveBeenCalledWith({ tab: 'instructions' });
  });

  test('selected navigation keeps the orange primary variant while unselected items stay ghost', () => {
    renderShell({
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane: 'adapted' },
    });

    expect(screen.getByTestId('agent-hub-tab-instructions').getAttribute('data-variant'))
      .toBe('primary');
    expect(screen.getByTestId('agent-hub-tab-skill').getAttribute('data-variant'))
      .toBe('ghost');
    expect(screen.getByTestId('agent-hub-lane-common').getAttribute('data-variant'))
      .toBe('ghost');
    expect(screen.getByTestId('agent-hub-lane-adapted').getAttribute('data-variant'))
      .toBe('primary');
    expect(screen.getByTestId('agent-hub-agent-claude').getAttribute('data-variant'))
      .toBe('primary');
    expect(screen.getByTestId('agent-hub-agent-codex').getAttribute('data-variant'))
      .toBe('ghost');
  });

  test('asset tabs show migrated kindCounts from tabCounts prop', () => {
    renderShell({
      tabCounts: { skill: 3, command: 1, mcp: 0, plugin: 2 },
    });

    const skill = screen.getByTestId('agent-hub-tab-skill');
    expect(skill.getAttribute('data-count')).toBe('3');
    expect(skill.textContent).toMatch(/\(3\)/);

    const plugin = screen.getByTestId('agent-hub-tab-plugin');
    expect(plugin.getAttribute('data-count')).toBe('2');
    expect(plugin.textContent).toMatch(/\(2\)/);

    const instructions = screen.getByTestId('agent-hub-tab-instructions');
    expect(instructions.getAttribute('data-count')).toBeNull();
    expect(instructions.textContent).not.toMatch(/\(\d+\)/);
  });

  test('toolbar pull/push invoke action callbacks and Adapt is absent', () => {
    const onPull = vi.fn();
    const onPush = vi.fn();
    renderShell({
      actions: { onPull, onPush },
    });

    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    fireEvent.click(screen.getByTestId('agent-hub-action-push'));
    expect(onPull).toHaveBeenCalled();
    expect(onPush).toHaveBeenCalled();
    expect(screen.queryByTestId('agent-hub-action-adapt')).toBeNull();
  });

  test('each group has one tab stop and keyboard navigation wraps with focus + callback', () => {
    const onContextChange = vi.fn();
    renderShell({
      onContextChange,
      context: { ...DEFAULT_AGENT_HUB_CONTEXT, instructionLane: 'adapted' },
    });

    const tabs = screen.getAllByRole('tab') as HTMLButtonElement[];
    const laneRadios = screen.getByTestId('agent-hub-lane-switcher').querySelectorAll('[role="radio"]');
    const agentRadios = screen.getByTestId('agent-hub-agent-switcher').querySelectorAll('[role="radio"]');
    expect(tabs.filter((node) => node.tabIndex === 0)).toHaveLength(1);
    expect(Array.from(laneRadios).filter((node) => (node as HTMLElement).tabIndex === 0)).toHaveLength(1);
    expect(Array.from(agentRadios).filter((node) => (node as HTMLElement).tabIndex === 0)).toHaveLength(1);

    fireEvent.keyDown(screen.getByTestId('agent-hub-tab-instructions'), { key: 'ArrowLeft' });
    expect(onContextChange).toHaveBeenLastCalledWith({
      tab: 'plugin',
      instructionLane: 'exclusive',
    });
    expect(document.activeElement).toBe(screen.getByTestId('agent-hub-tab-plugin'));

    // lane 顺序 exclusive → adapted → common；End 落在 common
    fireEvent.keyDown(screen.getByTestId('agent-hub-lane-exclusive'), { key: 'End' });
    expect(onContextChange).toHaveBeenLastCalledWith({ instructionLane: 'common' });
    expect(document.activeElement).toBe(screen.getByTestId('agent-hub-lane-common'));

    fireEvent.keyDown(screen.getByTestId('agent-hub-agent-claude'), { key: 'ArrowLeft' });
  });
});
