/**
 * E2E-AGENT-HUB-USER-MIRROR-001 —
 * Agent Hub 用户级镜像 Pull/Push 对话框（L1 mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   生产 Pull/Push 必须一次镜像全部已登记 Agent，不得再出现 mode radio、
 *   库存勾选、冲突策略或 asset id 输入；Apply 必须先 preview 再勾选确认。
 *
 * Code Logic（这个套件做什么）:
 *   复用 appBootstrap + Agent Hub 页挂载 mock；注册 user-mirror preview/apply/get；
 *   走壳层 Pull/Push 打开 UserMirrorDialog，断言门闩与 apply 请求体。
 *   本机 5173 被其它仓库占用时用 `E2E_PORT=5174` 启动本 worktree 的 Vite。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { HarnessCall, PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-08-23T00:00:00.000Z';
const PLAN_TOKEN = 'plan-e2e-user-mirror';
const USER_MIRROR_CAPABILITY = 'agent-hub.user-mirror.v1';

/**
 * Business Logic: 首屏 status 与 writeCompatible 门闸。
 * Code Logic: 三端 probe 合法 DTO。
 */
function makeStatus() {
  return {
    enabled: true,
    backgroundEnabled: false,
    agentHubApiVersion: 5,
    ownerInstanceId: 'owner-e2e-user-mirror-1',
    writeCompatible: true,
    probes: [
      {
        target: 'claude',
        support: 'supported',
        version: '1.0.0',
        executable: '/usr/local/bin/claude',
        configRoot: '/tmp/.claude',
      },
      {
        target: 'codex',
        support: 'scanOnly',
        version: null,
        executable: null,
        configRoot: '/tmp/.codex',
      },
      {
        target: 'opencode',
        support: 'unsupported',
        version: null,
        executable: null,
        configRoot: null,
      },
    ],
    conflictCount: 0,
    blockedMaterializationCount: 0,
  };
}

/**
 * Business Logic: 三栏挂载需要合法 workspace，避免页面 error 挡住工具栏。
 * Code Logic: 最小 user-scope canonical + claude native source。
 */
function makeInstructionWorkspace() {
  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'configured',
    healthState: 'healthy',
    canonical: {
      assetId: 'asset-user-instruction-e2e',
      displayName: 'User instructions',
      headRevisionId: 'rev-e2e-1',
      commonContent: '## Shared rules\n\nAlways run tests before commit.\n',
      targetExtensions: {},
      deleted: false,
      contentTruncated: false,
    },
    targets: [
      {
        target: 'claude',
        cli: {
          installed: true,
          version: '1.0.0',
          configRoot: '/tmp/.claude',
        },
        sources: [
          {
            sourceId: 'claude-native',
            path: '/Users/e2e/.claude/CLAUDE.md',
            role: 'native',
            active: true,
            exists: true,
            nonEmpty: true,
            hash: 'claude-source-hash',
            modifiedAt: TS,
            ownership: 'external',
          },
        ],
        effectiveSourceId: 'claude-native',
        managedTargetPath: null,
        managementMode: 'unmanaged',
        capability: {
          scan: 'readOnly',
          write: 'supported',
          remove: 'blocked',
          activate: 'newSession',
          reasonCode: null,
          evidenceIds: ['E2E-AGENT-HUB-USER-MIRROR-001'],
        },
        projection: {
          state: 'none',
          desiredRevisionId: null,
          appliedRevisionId: null,
          observedHash: null,
          lastErrorCode: null,
        },
        availableActions: ['openFile'],
      },
    ],
    inventorySnapshotHash: 'user-instruction-inventory-user-mirror-e2e',
    refreshedAt: TS,
  };
}

/**
 * Business Logic: 资产 tab 冷路径可能 inspect；给一条空库存避免 unregistered。
 * Code Logic: 三 target 空 items。
 */
function makePortableInventorySnapshot() {
  return {
    inventorySnapshotHash: 'snap-hash-user-mirror-e2e',
    refreshedAt: TS,
    stale: false,
    targets: (['claude', 'codex', 'opencode'] as const).map((target) => ({
      target,
      installed: true,
      version: '1.0.0',
      executable: `/usr/bin/${target}`,
      configRoot: `/tmp/.${target}`,
      scanCapability: 'supported',
      mutationCapability: 'supported',
      reasonCode: null,
      evidenceIds: [] as string[],
    })),
    items: [] as unknown[],
  };
}

/**
 * Business Logic: preview 必须带 TTL 未过期的绑定 plan，否则 Apply 会因 stale 禁用。
 * Code Logic: 单 Agent 计数非零；expiresAt 固定远期。
 */
function makeUserMirrorPlan(direction: 'pull' | 'push' = 'pull') {
  return {
    planToken: PLAN_TOKEN,
    expiresAt: '2099-01-01T00:00:00.000Z',
    direction,
    sourceDeviceId: direction === 'pull' ? 'peer-ok' : 'self-1',
    destinationDeviceId: direction === 'pull' ? 'self-1' : 'peer-ok',
    remoteInventorySnapshotHash: 'remote-hash-e2e',
    localInventorySnapshotHash: 'local-hash-e2e',
    credentialBearingCount: 1,
    hasCredentialBearingAssets: true,
    agents: [
      {
        target: 'claude',
        instructionWrites: [
          {
            logicalId: 'claude.native.CLAUDE.md',
            op: 'replace',
            sourceHash: 'src-hash',
            destHash: 'dst-hash',
          },
        ],
        portableUpserts: [
          {
            kind: 'skill',
            nativeId: 'skill-a',
            displayName: 'Skill A',
            op: 'write',
            credentialBearing: false,
          },
        ],
        portableDeletes: [
          {
            kind: 'command',
            nativeId: 'cmd-x',
            displayName: 'Cmd X',
            op: 'delete',
            credentialBearing: false,
          },
        ],
        pluginDisables: [
          {
            kind: 'plugin',
            nativeId: 'plug-x',
            displayName: 'Plug X',
            op: 'disable',
            credentialBearing: false,
          },
        ],
        mcpDeletes: [
          {
            kind: 'mcp',
            nativeId: 'github',
            displayName: 'GitHub MCP',
            op: 'delete',
            credentialBearing: true,
          },
        ],
      },
    ],
    blockingReasons: [] as string[],
  };
}

/**
 * Business Logic: apply 后按对端报告区需要合法 result。
 * Code Logic: 全成功、非 partial。
 */
function makeUserMirrorResult(direction: 'pull' | 'push' = 'pull') {
  return {
    planToken: PLAN_TOKEN,
    clientRequestId: 'req-e2e-user-mirror',
    sourceDeviceId: direction === 'pull' ? 'peer-ok' : 'self-1',
    destinationDeviceId: direction === 'pull' ? 'self-1' : 'peer-ok',
    partial: false,
    agents: [
      {
        target: 'claude',
        state: 'succeeded',
        errorCode: null,
        message: null,
      },
    ],
  };
}

/**
 * Business Logic: 镜像只对在线对端；capability 必须宣告 user-mirror.v1。
 * Code Logic: 两台在线 + 一台离线。
 */
function makeDevices() {
  return [
    {
      id: 'peer-ok',
      name: 'Peer OK',
      address: '192.168.1.10',
      port: 62116,
      online: true,
      capabilities: ['agent-hub.v1', 'agent-hub.portable-pull.v1', USER_MIRROR_CAPABILITY],
      protoVersion: 1,
    },
    {
      id: 'peer-ok-2',
      name: 'Peer OK 2',
      address: '192.168.1.11',
      port: 62116,
      online: true,
      capabilities: ['agent-hub.v1', 'agent-hub.portable-pull.v1', USER_MIRROR_CAPABILITY],
      protoVersion: 1,
    },
    {
      id: 'peer-offline',
      name: 'Peer Offline',
      address: '192.168.1.12',
      port: 62116,
      online: false,
      capabilities: [] as string[],
      protoVersion: 0,
    },
  ];
}

/**
 * Business Logic: Agent Hub 页挂载 + 用户级镜像三条命令。
 * Code Logic: AppShell + inspect/status/inventory + user-mirror preview/apply/get。
 */
function registerUserMirrorBase(harness: PlaywrightBackendHarness): void {
  registerAppShellCommands(harness);

  const workspace = makeInstructionWorkspace();
  harness.command('agent_hub_get_status', { kind: 'resolve', value: makeStatus() });
  harness.command('agent_hub_list_assets', { kind: 'resolve', value: [] });
  harness.command('agent_hub_get_asset', {
    kind: 'reject',
    error: { code: 'NOT_FOUND', message: 'no asset in user-mirror e2e' },
  });
  harness.command('agent_hub_inspect_user_instruction_workspace', {
    kind: 'resolve',
    value: workspace,
  });
  harness.command('agent_hub_save_user_instruction_blocks', {
    kind: 'resolve',
    value: workspace.canonical,
  });
  harness.command('agent_hub_inspect_portable_inventory', {
    kind: 'resolve',
    value: makePortableInventorySnapshot(),
  });
  harness.command('agent_hub_preview_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in user-mirror e2e' },
  });
  harness.command('agent_hub_apply_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in user-mirror e2e' },
  });
  harness.command('agent_hub_get_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in user-mirror e2e' },
  });
  harness.command('list_devices', { kind: 'resolve', value: makeDevices() });
  harness.command('agent_hub_preview_user_mirror', {
    kind: 'resolve',
    value: makeUserMirrorPlan('pull'),
  });
  harness.command('agent_hub_apply_user_mirror', {
    kind: 'resolve',
    value: makeUserMirrorResult('pull'),
  });
  harness.command('agent_hub_get_user_mirror', {
    kind: 'resolve',
    value: makeUserMirrorResult('pull'),
  });
}

/**
 * Business Logic: apply 请求体只能带 planToken + clientRequestId。
 * Code Logic: 从 harness invoke args 取出 request 对象。
 */
function readInvokeRequest(call: HarnessCall | undefined): Record<string, unknown> {
  if (!call || call.type !== 'invoke') {
    throw new Error('expected invoke call');
  }
  const args = call.args;
  if (!args || typeof args !== 'object' || Array.isArray(args)) {
    throw new Error('invoke args missing');
  }
  const request = (args as { request?: unknown }).request;
  if (!request || typeof request !== 'object' || Array.isArray(request)) {
    throw new Error('invoke request missing');
  }
  return request as Record<string, unknown>;
}

test.describe('E2E-AGENT-HUB-USER-MIRROR-001 user-mirror pull/push dialogs', () => {
  test('pull has no picker controls and gates apply until preview plus confirm', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerUserMirrorBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-action-pull')).toBeVisible();

    await page.getByTestId('agent-hub-action-pull').click();
    await expect(page.getByTestId('user-mirror-dialog')).toBeVisible();
    await expect(page.getByTestId('user-mirror-lan-risk')).toBeVisible();
    await expect(page.getByTestId('user-mirror-source-peer-ok')).toBeChecked({ timeout: 10_000 });
    await expect(page.getByTestId('user-mirror-source-peer-ok-2')).toBeVisible();

    await expect(page.getByTestId('lan-push-mode-fullHub')).toHaveCount(0);
    await expect(page.getByTestId('lan-push-mode-userScope')).toHaveCount(0);
    await expect(page.getByTestId('lan-push-mode-project')).toHaveCount(0);
    await expect(page.getByTestId('lan-push-mode-assets')).toHaveCount(0);
    await expect(page.getByTestId('portable-pull-item-list')).toHaveCount(0);
    await expect(page.locator('[data-testid^="portable-pull-item-"]')).toHaveCount(0);
    await expect(page.getByTestId('portable-pull-filter-kind')).toHaveCount(0);
    await expect(page.getByTestId('portable-pull-policy-skipExisting')).toHaveCount(0);
    await expect(page.getByTestId('portable-pull-policy-replaceAfterPreview')).toHaveCount(0);
    await expect(page.getByLabel(/conflict/i)).toHaveCount(0);
    await expect(page.getByTestId('user-mirror-plan')).toHaveCount(0);

    const apply = page.getByTestId('user-mirror-apply');
    await expect(apply).toBeDisabled();

    await page.getByTestId('user-mirror-preview').click();
    await expect(page.getByTestId('user-mirror-plan')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('user-mirror-agent-claude')).toBeVisible();
    await expect(apply).toBeDisabled();

    const previewCalls = backendHarness
      .calls()
      .filter(
        (call) => call.type === 'invoke' && call.command === 'agent_hub_preview_user_mirror',
      );
    expect(previewCalls.length).toBeGreaterThanOrEqual(1);
    expect(readInvokeRequest(previewCalls[0])).toEqual({
      direction: 'pull',
      sourceDeviceId: 'peer-ok',
      peerDeviceIds: [],
    });

    await page.getByTestId('user-mirror-confirm-overwrite').check();
    await expect(apply).toBeEnabled();

    await apply.click();
    await expect(page.getByTestId('user-mirror-report')).toBeVisible({ timeout: 10_000 });

    const applyCalls = backendHarness
      .calls()
      .filter((call) => call.type === 'invoke' && call.command === 'agent_hub_apply_user_mirror');
    expect(applyCalls).toHaveLength(1);
    const applyRequest = readInvokeRequest(applyCalls[0]);
    expect(Object.keys(applyRequest).sort()).toEqual(['clientRequestId', 'planToken']);
    expect(applyRequest.planToken).toBe(PLAN_TOKEN);
    expect(typeof applyRequest.clientRequestId).toBe('string');
    expect(String(applyRequest.clientRequestId).length).toBeGreaterThan(0);

    await page.getByTestId('user-mirror-source-peer-ok-2').check();
    await expect(page.getByTestId('user-mirror-plan')).toHaveCount(0);
    await expect(apply).toBeDisabled();
    await expect(page.getByTestId('user-mirror-confirm-overwrite')).not.toBeChecked();
  });

  test('push lists peer checkboxes without asset-id input and keeps a report region', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerUserMirrorBase(backendHarness);
    backendHarness.command('agent_hub_preview_user_mirror', {
      kind: 'resolve',
      value: makeUserMirrorPlan('push'),
    });
    backendHarness.command('agent_hub_apply_user_mirror', {
      kind: 'resolve',
      value: makeUserMirrorResult('push'),
    });

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await page.getByTestId('agent-hub-action-push').click();
    await expect(page.getByTestId('user-mirror-dialog')).toBeVisible();

    await expect(page.getByTestId('lan-push-mode-fullHub')).toHaveCount(0);
    await expect(page.getByTestId('lan-push-asset-ids')).toHaveCount(0);
    await expect(page.getByTestId('lan-push-project-ids')).toHaveCount(0);
    await expect(page.getByTestId('user-mirror-peer-peer-ok')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('user-mirror-peer-peer-ok-2')).toBeVisible();
    await expect(page.getByTestId('user-mirror-peer-peer-offline')).toHaveCount(0);

    await page.getByTestId('user-mirror-peer-peer-ok').check();
    await page.getByTestId('user-mirror-preview').click();
    await expect(page.getByTestId('user-mirror-plan')).toBeVisible({ timeout: 10_000 });
    await page.getByTestId('user-mirror-confirm-overwrite').check();
    await page.getByTestId('user-mirror-apply').click();
    await expect(page.getByTestId('user-mirror-report')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('user-mirror-report-peer-ok')).toBeVisible();
  });
});
