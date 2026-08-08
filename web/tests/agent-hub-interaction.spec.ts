/**
 * E2E-AGENT-HUB-SHELL-001 / E2E-AGENT-HUB-INSTR-3PANE-001 /
 * E2E-AGENT-HUB-DISCOVER-MANAGED-001 / E2E-AGENT-HUB-ADAPT-FULL-001 —
 * Agent Hub 交互重设计 L1 mock journeys（壳层 / 三栏 / 发现即管理 / 全量适配预览门闩）。
 *
 * Business Logic（为什么需要这个套件）:
 *   交互重设计交付 Agent→范围→五 Tab 壳层、提示词三栏（初始空块）、portable
 *   发现即管理（无 Adopt 主动作）、跨 Agent 全量适配强制 preview。本 L1 用
 *   backendHarness 锁定 UI 旅程，不宣称真实 CLI 写盘、多机 mDNS 或 full adapt
 *   非 stub 生成器。
 *
 * Code Logic（这个套件做什么）:
 *   复用 appBootstrap + agent_hub_* / list_devices / portable inventory mock；
 *   断言 data-testid 与 preview gate。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-08-08T00:00:00.000Z';
const PROJECT_ID = 'proj-local-1';

/**
 * Business Logic: 首屏 status 与 writeCompatible 门闸。
 * Code Logic: 三端 probe 合法 DTO。
 */
function makeStatus(overrides: Record<string, unknown> = {}) {
  return {
    enabled: true,
    backgroundEnabled: false,
    agentHubApiVersion: 1,
    ownerInstanceId: 'owner-e2e-interaction-1',
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
    ...overrides,
  };
}

/**
 * Business Logic: 三栏初始应只加载原始正文、块为空。
 * Code Logic: canonical.commonContent 带 ## 节，供 reparse 后出块。
 */
function makeInstructionWorkspace(overrides: Record<string, unknown> = {}) {
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
          write: 'blocked',
          remove: 'blocked',
          activate: 'blocked',
          reasonCode: 'USER_INSTRUCTION_WRITE_EVIDENCE_MISSING',
          evidenceIds: ['L1-AGENT-HUB-INSTR-3PANE-001'],
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
      {
        target: 'codex',
        cli: { installed: true, version: '1.0.0', configRoot: '/tmp/.codex' },
        sources: [
          {
            sourceId: 'codex-override',
            path: '/Users/e2e/.codex/AGENTS.override.md',
            role: 'override',
            active: true,
            exists: true,
            nonEmpty: true,
            hash: 'codex-source-hash',
            modifiedAt: TS,
            ownership: 'external',
          },
        ],
        effectiveSourceId: 'codex-override',
        managedTargetPath: null,
        managementMode: 'unmanaged',
        capability: {
          scan: 'readOnly',
          write: 'blocked',
          remove: 'blocked',
          activate: 'blocked',
          reasonCode: 'USER_INSTRUCTION_WRITE_EVIDENCE_MISSING',
          evidenceIds: ['L1-AGENT-HUB-INSTR-3PANE-001'],
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
      {
        target: 'opencode',
        cli: { installed: false, version: null, configRoot: '' },
        sources: [],
        effectiveSourceId: null,
        managedTargetPath: null,
        managementMode: 'unmanaged',
        capability: {
          scan: 'blocked',
          write: 'blocked',
          remove: 'blocked',
          activate: 'blocked',
          reasonCode: 'CLI_NOT_INSTALLED',
          evidenceIds: [],
        },
        projection: {
          state: 'none',
          desiredRevisionId: null,
          appliedRevisionId: null,
          observedHash: null,
          lastErrorCode: null,
        },
        availableActions: [],
      },
    ],
    inventorySnapshotHash: 'user-instruction-inventory-interaction-e2e',
    refreshedAt: TS,
    ...overrides,
  };
}

/**
 * Business Logic: portable inventory 含 canAdopt 历史项，UI 不得暴露 Adopt 主按钮。
 * Code Logic: hubManaged + 仅 canAdopt 的 unmanaged 各一条。
 */
function makePortableInventorySnapshot() {
  return {
    inventorySnapshotHash: 'snap-hash-interaction-e2e',
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
    items: [
      {
        inventoryItemId: 'claude-skill-managed',
        target: 'claude',
        kind: 'skill',
        nativeId: 'managed-skill',
        displayName: 'Managed Skill',
        description: null,
        version: null,
        scopeId: 'user',
        scopeKind: 'user',
        projectId: null,
        projectOptedIn: true,
        sourcePath: '/tmp/claude/skill/managed-skill',
        sourceOrigin: 'standalone',
        parentPluginInventoryItemId: null,
        actualEnabled: true,
        contentHash: 'hash-managed',
        treeHash: 'tree-managed',
        canonicalAssetId: 'asset-managed-1',
        canonicalRevisionId: 'rev-managed-1',
        managementState: 'hubManaged',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        capabilities: {
          canEnable: true,
          canDisable: true,
          canUninstall: true,
          canAdopt: false,
          canInstallToSourceTarget: false,
          reasonCode: null,
          evidenceIds: [] as string[],
        },
        warnings: [] as string[],
      },
      {
        inventoryItemId: 'claude-skill-orphan-adopt',
        target: 'claude',
        kind: 'skill',
        nativeId: 'orphan-skill',
        displayName: 'Orphan Skill',
        description: 'Historical unmanaged with canAdopt only',
        version: null,
        scopeId: 'user',
        scopeKind: 'user',
        projectId: null,
        projectOptedIn: true,
        sourcePath: '/tmp/claude/skill/orphan-skill',
        sourceOrigin: 'standalone',
        parentPluginInventoryItemId: null,
        actualEnabled: true,
        contentHash: 'hash-orphan',
        treeHash: null,
        canonicalAssetId: null,
        canonicalRevisionId: null,
        managementState: 'unmanaged',
        desiredPresence: null,
        desiredEnabled: null,
        materializationStatus: null,
        capabilities: {
          canEnable: false,
          canDisable: false,
          canUninstall: false,
          canAdopt: true,
          canInstallToSourceTarget: false,
          reasonCode: null,
          evidenceIds: [] as string[],
        },
        warnings: [] as string[],
      },
    ],
  };
}

/**
 * Business Logic: Agent Hub 页挂载并行 getStatus/listAssets/inspect。
 * Code Logic: AppShell + agent_hub 基线；可覆盖 portable / workspace / full adapt。
 */
function registerInteractionBase(
  harness: PlaywrightBackendHarness,
  options: {
    workspace?: ReturnType<typeof makeInstructionWorkspace>;
    portableInventory?: ReturnType<typeof makePortableInventorySnapshot>;
  } = {},
): void {
  registerAppShellCommands(harness);

  const workspace = options.workspace ?? makeInstructionWorkspace();
  const portableInventory = options.portableInventory ?? makePortableInventorySnapshot();

  harness.command('agent_hub_get_status', { kind: 'resolve', value: makeStatus() });
  harness.command('agent_hub_list_assets', { kind: 'resolve', value: [] });
  harness.command('agent_hub_get_asset', {
    kind: 'reject',
    error: { code: 'NOT_FOUND', message: 'no asset in interaction e2e' },
  });
  harness.command('agent_hub_inspect_user_instruction_workspace', {
    kind: 'resolve',
    value: workspace,
  });
  harness.command('agent_hub_inspect_portable_inventory', {
    kind: 'resolve',
    value: portableInventory,
  });
  harness.command('agent_hub_preview_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in interaction e2e' },
  });
  harness.command('agent_hub_apply_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in interaction e2e' },
  });
  harness.command('agent_hub_get_portable_asset_action', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used in interaction e2e' },
  });
  harness.command('agent_hub_list_remote_portable_inventory', {
    kind: 'resolve',
    value: {
      sourceDeviceId: 'peer-ok',
      sourceTarget: 'claude',
      inventorySnapshotHash: 'remote-snap-interaction',
      refreshedAt: TS,
      stale: false,
      items: [],
    },
  });
  harness.command('agent_hub_preview_portable_pull', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used' },
  });
  harness.command('agent_hub_apply_portable_pull', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used' },
  });
  harness.command('agent_hub_get_portable_pull', {
    kind: 'reject',
    error: { code: 'NOT_USED', message: 'not used' },
  });
  harness.command('agent_hub_preview_project', {
    kind: 'resolve',
    value: {
      projectId: PROJECT_ID,
      hubProjectId: 'hub-proj-1',
      path: '/tmp/demo-project',
      optedIn: false,
      checkouts: [{ path: '/tmp/demo-project', role: 'main' }],
      plannedActions: [],
      warnings: [],
      noCommitNotice: 'preview only',
      gitRemoteFingerprint: null,
    },
  });
  harness.command('agent_hub_enable_project', {
    kind: 'resolve',
    value: { projectId: PROJECT_ID, hubProjectId: 'hub-proj-1', optedIn: true, warnings: [] },
  });
  harness.command('list_devices', {
    kind: 'resolve',
    value: [
      {
        id: 'peer-ok',
        name: 'Peer OK',
        address: '192.168.1.10',
        port: 62116,
        online: true,
        capabilities: ['agent-hub.v1', 'agent-hub.portable-pull.v1'],
        protoVersion: 1,
      },
      {
        id: 'peer-offline',
        name: 'Peer Offline',
        address: '192.168.1.12',
        port: 62116,
        online: false,
        capabilities: [],
        protoVersion: 0,
      },
    ],
  });
  harness.command('list_workbench_projects', {
    kind: 'resolve',
    value: [
      {
        id: 'local-1',
        name: 'Local Repo',
        path: '/tmp/local-repo',
        kind: 'local',
        deviceId: 'self',
        deviceName: 'This device',
        lastOpenedAt: TS,
        createdAt: TS,
        updatedAt: TS,
      },
      {
        id: 'remote:dev-hk:inner',
        name: 'Remote Repo',
        path: '/remote/repo',
        kind: 'remote',
        deviceId: 'peer-ok',
        deviceName: 'HK Peer',
        lastOpenedAt: TS,
        createdAt: TS,
        updatedAt: TS,
      },
    ],
  });

  // Cross-agent full adapt (stub generator contract)
  harness.command('agent_hub_preview_cross_agent_full', {
    kind: 'resolve',
    value: {
      source: 'claude',
      destination: 'codex',
      scope: 'user',
      planHash: 'full-plan-hash-e2e-interaction-001',
      generator: 'stub',
      items: [
        {
          kind: 'instruction',
          logicalKey: 'instruction:user',
          action: 'create',
          path: '/tmp/.codex/AGENTS.md',
          content: 'Always run tests before commit.',
          residualReason: null,
          included: true,
        },
        {
          kind: 'skill',
          logicalKey: 'skill:demo',
          action: 'skip',
          path: '/tmp/skill',
          residualReason: 'stub:skill_copy_not_ready',
          included: true,
        },
        {
          kind: 'command',
          logicalKey: 'inventory:empty:command',
          action: 'skip',
          path: '',
          residualReason: 'no_command_on_source',
          included: false,
        },
        {
          kind: 'mcp',
          logicalKey: 'inventory:empty:mcp',
          action: 'skip',
          path: '',
          residualReason: 'no_mcp_on_source',
          included: false,
        },
        {
          kind: 'plugin',
          logicalKey: 'inventory:empty:plugin',
          action: 'skip',
          path: '',
          residualReason: 'no_plugin_on_source',
          included: false,
        },
      ],
    },
  });
  harness.command('agent_hub_apply_cross_agent_full', {
    kind: 'resolve',
    value: [
      {
        kind: 'instruction',
        logicalKey: 'instruction:user',
        status: 'applied',
        path: '/tmp/.codex/AGENTS.md',
        errorCode: null,
      },
    ],
  });
  harness.command('agent_hub_preview_cross_agent_instruction', {
    kind: 'resolve',
    value: {
      needsAdaptation: false,
      destinations: [
        {
          destination: 'codex',
          path: '/tmp/.codex/AGENTS.override.md',
          mode: 'shared',
          unifiedDiff: '+ Always run tests',
          canApply: true,
          partialBlockers: [],
        },
      ],
    },
  });
  harness.command('agent_hub_apply_cross_agent_instruction', {
    kind: 'resolve',
    value: [
      {
        destination: 'codex',
        status: 'applied',
        path: '/tmp/.codex/AGENTS.override.md',
        errorCode: null,
      },
    ],
  });
}

test.describe('E2E-AGENT-HUB-SHELL-001 Agent Hub shell agent/scope/tabs', () => {
  test('shell shows agent switcher, scope, tabs; context clicks update URL', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-shell')).toBeVisible();
    await expect(page.getByTestId('agent-hub-agent-switcher')).toBeVisible();
    await expect(page.getByTestId('agent-hub-scope-switcher')).toBeVisible();
    await expect(page.getByTestId('agent-hub-tablist')).toBeVisible();
    await expect(page.getByTestId('agent-hub-toolbar')).toBeVisible();
    await expect(page.getByTestId('agent-hub-device-select')).toBeVisible();
    await expect(page.getByTestId('agent-hub-project-select')).toHaveCount(0);

    await page.getByTestId('agent-hub-agent-codex').click();
    await expect(page).toHaveURL(/agent=codex/);
    await expect(page.getByTestId('agent-hub-agent-codex')).toHaveAttribute(
      'aria-selected',
      'true',
    );

    await page.getByTestId('agent-hub-tab-skill').click();
    // dual-path portable URL may rewrite tab=skill → section=assets&kind/target
    await expect(page.getByTestId('agent-hub-tab-skill')).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page).toHaveURL(/section=assets|tab=skill|kind=skill/);

    await page.getByTestId('agent-hub-scope-project').click();
    await expect(page).toHaveURL(/scope=project/);
    await expect(page.getByTestId('agent-hub-device-select')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-project-select')).toBeVisible();
    await expect(page.getByTestId('agent-hub-project-option-local-1')).toBeAttached();
  });
});

test.describe('E2E-AGENT-HUB-INSTR-3PANE-001 instruction three-pane empty blocks', () => {
  test('three panes present; blocks empty until reparse from original', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('instruction-three-pane')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('instruction-pane-blocks')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-preview')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-original')).toBeVisible();

    // Spec: open must not auto-parse blocks
    await expect(page.getByTestId('instruction-blocks-empty')).toBeVisible();
    await expect(page.getByTestId('instruction-block-list')).toHaveCount(0);
    await expect(page.getByTestId('instruction-reparse-from-original')).toBeVisible();
    await expect(
      page.getByTestId('instruction-pane-original').getByTestId('instruction-reparse-from-original'),
    ).toBeVisible();

    await expect(page.getByTestId('instruction-original-textarea')).toHaveValue(
      /Always run tests before commit/,
    );

    await page.getByTestId('instruction-reparse-from-original').click();
    await expect(page.getByTestId('instruction-blocks-empty')).toHaveCount(0);
    await expect(page.getByTestId('instruction-block-list')).toBeVisible();
    // preview 由块按 agent 合成（commonMarkdown），不再含 `## 标题`（标题在 headingPath）
    await expect(page.getByTestId('instruction-preview-body')).toContainText(
      'Always run tests before commit',
    );
  });
});

test.describe('E2E-AGENT-HUB-DISCOVER-MANAGED-001 no primary Adopt in portable inventory', () => {
  test('skill tab inventory has no Adopt primary CTA even when canAdopt is true', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-shell')).toBeVisible({ timeout: 15_000 });
    await page.getByTestId('agent-hub-tab-skill').click();
    await expect(page.getByTestId('portable-inventory-workspace')).toBeVisible({
      timeout: 15_000,
    });

    await expect(page.getByTestId('portable-inventory-row-claude-skill-managed')).toBeVisible();
    await expect(
      page.getByTestId('portable-inventory-row-claude-skill-orphan-adopt'),
    ).toBeVisible();

    // 主路径不得出现 Adopt / Manage existing file 主动作
    await expect(page.getByRole('button', { name: /^Adopt$/i })).toHaveCount(0);
    await expect(
      page.getByRole('button', { name: /Manage existing file/i }),
    ).toHaveCount(0);
    await expect(page.getByTestId('portable-action-adopt')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-adoption-dialog')).toHaveCount(0);

    // canAdopt-only orphan：无 enable/disable 主动作，展示 refresh 纳入提示
    const orphan = page.getByTestId('portable-inventory-row-claude-skill-orphan-adopt');
    await expect(orphan.getByRole('button', { name: /Disable|Enable|Install/i })).toHaveCount(
      0,
    );
    await expect(orphan.getByTestId('portable-row-unmanaged-refresh-hint')).toBeVisible();
  });
});

test.describe('E2E-AGENT-HUB-ADAPT-FULL-001 full adapt preview gate', () => {
  test('adapt full mode requires preview before apply; plan items appear after preview', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub?view=adapt');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('cross-agent-adapt-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('cross-agent-adapt-mode')).toBeVisible();

    await page.getByTestId('cross-agent-adapt-mode-full').check();
    await expect(page.getByTestId('cross-agent-adapt-full-hint')).toBeVisible();
    await expect(page.getByTestId('cross-agent-adapt-full-destination')).toBeVisible();
    await expect(page.getByTestId('cross-agent-adapt-full-dest-codex')).toBeChecked();

    // 无 plan 时 apply 关闭
    await expect(page.getByTestId('cross-agent-adapt-apply')).toBeDisabled();
    await expect(page.getByTestId('cross-agent-adapt-full-plan')).toHaveCount(0);

    // scope confirm + 源正文（填入确保 preview gate 不依赖 inspect 时序）
    await page.getByTestId('cross-agent-adapt-scope-confirm').check();
    await page
      .getByTestId('cross-agent-adapt-markdown')
      .fill('## Shared rules\n\nAlways run tests before commit.\n');
    await expect(page.getByTestId('cross-agent-adapt-markdown')).not.toHaveValue('');

    await page.getByTestId('cross-agent-adapt-preview').click();
    await expect(page.getByTestId('cross-agent-adapt-full-plan')).toBeVisible({
      timeout: 10_000,
    });
    // UI truncates planHash to 12 chars + ellipsis
    await expect(page.getByTestId('cross-agent-adapt-full-plan-hash')).toContainText(
      'full-plan-ha',
    );
    await expect(page.getByTestId('cross-agent-adapt-full-plan-hash')).toContainText('stub');
    await expect(
      page.getByTestId('cross-agent-adapt-full-item-instruction:user'),
    ).toBeVisible();

    // preview 之后 apply 才可点
    await expect(page.getByTestId('cross-agent-adapt-apply')).toBeEnabled();

    await page.getByTestId('cross-agent-adapt-apply').click();
    await expect(page.getByTestId('cross-agent-adapt-full-apply-result')).toBeVisible({
      timeout: 10_000,
    });

    const fullPreviewCalls = backendHarness
      .calls()
      .filter(
        (call) =>
          call.type === 'invoke' && call.command === 'agent_hub_preview_cross_agent_full',
      );
    const fullApplyCalls = backendHarness
      .calls()
      .filter(
        (call) =>
          call.type === 'invoke' && call.command === 'agent_hub_apply_cross_agent_full',
      );
    expect(fullPreviewCalls.length).toBeGreaterThanOrEqual(1);
    expect(fullApplyCalls.length).toBeGreaterThanOrEqual(1);
  });
});
