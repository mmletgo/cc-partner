/**
 * E2E-AGENT-HUB-SHELL-001 / E2E-AGENT-HUB-INSTR-3PANE-001 /
 * E2E-AGENT-HUB-DISCOVER-MANAGED-001 / E2E-AGENT-HUB-ADAPT-PREVIEW-001 —
 * Agent Hub 安全纠正 L1 mock journeys（壳层 / 三栏 / 发现即管理 / 选择性预览）。
 *
 * Business Logic（为什么需要这个套件）:
 *   交互重设计交付 Agent→范围→五 Tab 壳层、提示词三栏（初始空块）、portable
 *   发现即管理（无 Adopt 主动作）、跨 Agent 本机用户级 preview-only。本 L1 用
 *   backendHarness 锁定 UI 旅程，不宣称真实 CLI 写盘、多机 mDNS 或 full adapt。
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
    agentHubApiVersion: 4,
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
          write: 'supported',
          remove: 'blocked',
          activate: 'newSession',
          reasonCode: null,
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
          write: 'supported',
          remove: 'blocked',
          activate: 'newSession',
          reasonCode: null,
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
  harness.command('agent_hub_save_user_instruction_blocks', {
    kind: 'resolve',
    value: workspace.canonical,
  });
  harness.command('agent_hub_preview_user_instruction_update', {
    kind: 'resolve',
    value: {
      planToken: 'instruction-plan-e2e',
      expiresAt: '2027-08-10T12:05:00.000Z',
      baseRevisionId: 'rev-e2e-1',
      inventorySnapshotHash: workspace.inventorySnapshotHash,
      blockingReasons: [],
      changes: [
        {
          target: 'claude',
          path: '/Users/e2e/.claude/CLAUDE.md',
          operation: 'update',
          currentHash: 'claude-source-hash',
          expectedHash: 'claude-source-hash',
          renderedHash: 'claude-rendered-hash',
          unifiedDiff: '-old\n+shared',
          ownershipRequired: false,
          willShadowSourcePath: null,
          willReplaceFallbackSourcePath: null,
          emptyDueToTargetOnly: false,
          activation: 'newSession',
          warnings: [],
        },
        {
          target: 'codex',
          path: '/Users/e2e/.codex/AGENTS.md',
          operation: 'update',
          currentHash: 'codex-source-hash',
          expectedHash: 'codex-source-hash',
          renderedHash: 'codex-rendered-hash',
          unifiedDiff: '-old\n+shared',
          ownershipRequired: false,
          willShadowSourcePath: null,
          willReplaceFallbackSourcePath: null,
          emptyDueToTargetOnly: false,
          activation: 'newSession',
          warnings: [],
        },
      ],
    },
  });
  harness.command('agent_hub_apply_user_instruction_plan', {
    kind: 'resolve',
    value: {
      planToken: 'instruction-plan-e2e',
      setupState: 'configured',
      healthState: 'healthy',
      targets: [
        {
          target: 'claude',
          status: 'applied',
          path: '/Users/e2e/.claude/CLAUDE.md',
          errorCode: null,
          activation: 'newSession',
        },
        {
          target: 'codex',
          status: 'applied',
          path: '/Users/e2e/.codex/AGENTS.md',
          errorCode: null,
          activation: 'newSession',
        },
      ],
    },
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
      source: 'claude',
      kind: 'instruction',
      needsAdaptation: false,
      planHash: 'selective-preview-e2e',
      destinations: [
        {
          destination: 'codex',
          path: '/tmp/.codex/AGENTS.override.md',
          mode: 'shared',
          renderedHash: 'rendered-e2e',
          observedHash: 'observed-e2e',
          unifiedDiff: '+ Always run tests',
          canApply: false,
          partialBlockers: ['CROSS_AGENT_PREVIEW_ONLY'],
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

test.describe('E2E-AGENT-HUB-SHELL-001 Agent Hub shell context and keyboard', () => {
  test('local-user shell owns agent/lane/tabs and roving keyboard state', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-shell')).toBeVisible();
    await expect(page.getByTestId('agent-hub-agent-switcher')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-lane-switcher')).toBeVisible();
    await expect(page.getByTestId('agent-hub-scope-switcher')).toBeVisible();
    await expect(page.getByTestId('agent-hub-tablist')).toBeVisible();
    await expect(page.getByTestId('agent-hub-toolbar')).toBeVisible();
    await expect(page.getByTestId('agent-hub-device-select')).toBeVisible();
    await expect(page.getByTestId('agent-hub-project-select')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-lane-common')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await expect(page.getByTestId('agent-hub-lane-common')).toHaveAttribute('tabindex', '0');

    // lane radiogroup：方向键移动焦点并同步选择。
    await page.getByTestId('agent-hub-lane-common').focus();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByTestId('agent-hub-lane-adapted')).toBeFocused();
    await expect(page.getByTestId('agent-hub-lane-adapted')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await expect(page).toHaveURL(/lane=adapted/);

    // 适配槽恢复 Agent radiogroup，并同样只保留一个 tab stop。
    await expect(page.getByTestId('agent-hub-agent-switcher')).toBeVisible();
    await page.getByTestId('agent-hub-agent-claude').focus();
    await page.keyboard.press('End');
    await expect(page.getByTestId('agent-hub-agent-opencode')).toBeFocused();
    await expect(page.getByTestId('agent-hub-agent-opencode')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await page.getByTestId('agent-hub-agent-codex').click();
    await expect(page).toHaveURL(/agent=codex/);
    await expect(page.getByTestId('agent-hub-agent-codex')).toHaveAttribute(
      'aria-checked',
      'true',
    );

    // workspace tablist：End/ Home 维护 roving tabindex 与 tabpanel 关联。
    await page.getByTestId('agent-hub-tab-instructions').focus();
    await page.keyboard.press('End');
    await expect(page.getByTestId('agent-hub-tab-plugin')).toBeFocused();
    await expect(page.getByTestId('agent-hub-tab-plugin')).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page.getByTestId('agent-hub-shell-body')).toHaveAttribute(
      'aria-labelledby',
      'agent-hub-tab-plugin',
    );
    await expect(page).toHaveURL(/tab=plugin/);
    await expect(page).not.toHaveURL(/section=|target=|kind=|scope=project|deviceId=/);
  });

  test('legacy asset deep link canonicalizes while preserving its explicit project context', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto(
      '/agent-hub?scope=project&projectKey=local-1&deviceId=peer-ok&section=assets&target=claude&kind=skill',
    );
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-context-migration-notice')).toBeVisible();
    await expect(page.getByTestId('agent-hub-scope-switcher')).toBeVisible();
    await expect(page.getByTestId('agent-hub-device-select')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-project-select')).toBeVisible();
    await expect(page.getByTestId('agent-hub-agent-claude')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await expect(page).toHaveURL(/tab=skill/);
    await expect(page).toHaveURL(/scope=project/);
    await expect(page).toHaveURL(/project=local-1/);
    await expect(page).not.toHaveURL(/deviceId=|section=|target=|kind=/);
  });
});

test.describe('E2E-AGENT-HUB-INSTR-3PANE-001 instruction lane layouts', () => {
  test('common single / adapted dual / exclusive three-pane with reparse', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('instruction-three-pane')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-lane-switcher')).toBeVisible();

    // 公共槽：仅单列公共编辑；公共内容与 Agent 无关，因此隐藏 Agent context。
    await expect(page.getByTestId('instruction-panes-common')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-blocks')).toBeVisible();
    await expect(page.getByTestId('instruction-slot-textarea')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-preview')).toHaveCount(0);
    await expect(page.getByTestId('instruction-pane-original')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-agent-switcher')).toHaveCount(0);

    // 公共槽直接覆盖所有当前可写 Agent：页面已完成内容核对，点击后直接原子写入。
    await expect(page.getByTestId('instruction-sync-to-native')).toBeEnabled();
    await expect(page.getByTestId('instruction-sync-to-native')).toHaveText('写入原始文件');
    await page.getByTestId('instruction-sync-to-native').click();
    await expect(page.getByTestId('user-instruction-preview-dialog')).toHaveCount(0);
    await expect(page.getByTestId('instruction-three-pane-apply-result')).toBeVisible();

    // 适配槽 + Claude：仅公共底稿单列
    await page.getByTestId('agent-hub-lane-adapted').click();
    await expect(page).toHaveURL(/lane=adapted/);
    await expect(page.getByTestId('agent-hub-agent-switcher')).toBeVisible();
    await expect(page.getByTestId('instruction-panes-adapted')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-adapted-common')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-adapted-variant')).toHaveCount(0);
    await expect(page.getByTestId('instruction-pane-preview')).toHaveCount(0);

    // 适配槽 + Codex：双列（底稿 + 变体）
    await page.getByTestId('agent-hub-agent-codex').click();
    await expect(page).toHaveURL(/agent=codex/);
    await expect(page.getByTestId('instruction-pane-adapted-common')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-adapted-variant')).toBeVisible();

    // 独有槽：三列 + 原始栏 reparse
    await page.getByTestId('agent-hub-lane-exclusive').click();
    await expect(page).toHaveURL(/lane=exclusive/);
    await expect(page.getByTestId('instruction-panes-exclusive')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-blocks')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-preview')).toBeVisible();
    await expect(page.getByTestId('instruction-pane-original')).toBeVisible();
    await expect(page.getByTestId('instruction-reparse-from-original')).toBeVisible();
    await expect(
      page.getByTestId('instruction-pane-original').getByTestId('instruction-reparse-from-original'),
    ).toBeVisible();

    await expect(page.getByTestId('instruction-original-textarea')).toHaveValue(
      /Always run tests before commit/,
    );

    // 从原始解析后必须形成未保存草稿；切 lane 先进入统一 dirty Dialog。
    await page.getByTestId('instruction-reparse-from-original').click();
    await expect(page.getByTestId('instruction-unsaved-draft')).toBeVisible();
    await expect(page.getByTestId('instruction-save-blocks')).toBeEnabled();
    await page.getByTestId('agent-hub-lane-common').click();
    await expect(page.getByTestId('agent-hub-context-change-dialog')).toBeVisible();
    await expect(page.getByTestId('agent-hub-context-stay')).toBeFocused();
    await page.getByTestId('agent-hub-context-stay').click();
    await expect(page.getByTestId('instruction-panes-exclusive')).toBeVisible();
    await expect(page.getByTestId('instruction-preview-body')).toContainText(
      'Always run tests before commit',
    );
  });
});

test.describe('E2E-AGENT-HUB-DIRTY-HISTORY-001 committed context guard', () => {
  test('history change restores old shell until Stay or Discard resolves the draft', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub?agent=claude&tab=instructions&lane=common');
    await expect(page.getByTestId('instruction-slot-textarea')).toBeVisible({
      timeout: 15_000,
    });
    await page.getByTestId('instruction-slot-textarea').fill('Unsaved history guard draft');
    await expect(page.getByTestId('instruction-unsaved-draft')).toBeVisible();

    await page.evaluate(() => {
      window.history.pushState({}, '', '/agent-hub?agent=codex&tab=skill');
      window.dispatchEvent(new PopStateEvent('popstate'));
    });
    await expect(page.getByTestId('agent-hub-context-change-dialog')).toBeVisible();
    await expect(page.getByTestId('agent-hub-agent-switcher')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-tab-instructions')).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page).not.toHaveURL(/agent=codex|tab=skill/);

    await page.getByTestId('agent-hub-context-stay').click();
    await expect(page.getByTestId('agent-hub-context-change-dialog')).toHaveCount(0);
    await expect(page.getByTestId('instruction-slot-textarea')).toHaveValue(
      'Unsaved history guard draft',
    );

    await page.evaluate(() => {
      window.history.pushState({}, '', '/agent-hub?agent=codex&tab=skill');
      window.dispatchEvent(new PopStateEvent('popstate'));
    });
    await expect(page.getByTestId('agent-hub-context-change-dialog')).toBeVisible();
    await page.getByTestId('agent-hub-context-discard').click();
    await expect(page.getByTestId('portable-inventory-workspace')).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('agent-hub-agent-codex')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await expect(page).toHaveURL(/agent=codex/);
    await expect(page).toHaveURL(/tab=skill/);
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

test.describe('E2E-AGENT-HUB-ADAPT-PREVIEW-001 selective preview-only', () => {
  test('local-user selective preview shows bounded diff and exposes no apply/full control', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerInteractionBase(backendHarness);

    await page.goto('/agent-hub?view=adapt');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('cross-agent-adapt-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('cross-agent-preview-only')).toBeVisible();
    await expect(page.getByTestId('cross-agent-adapt-mode')).toHaveCount(0);
    await expect(page.getByTestId('cross-agent-adapt-apply')).toHaveCount(0);
    await expect(page.getByTestId('cross-agent-adapt-full-plan')).toHaveCount(0);

    await expect(page.getByTestId('cross-agent-adapt-dest-codex')).toBeChecked();
    await page.getByTestId('cross-agent-adapt-dest-opencode').uncheck();
    await page.getByTestId('cross-agent-adapt-scope-confirm').check();
    await page
      .getByTestId('cross-agent-adapt-markdown')
      .fill('## Shared rules\n\nAlways run tests before commit.\n');
    await expect(page.getByTestId('cross-agent-adapt-markdown')).not.toHaveValue('');

    await page.getByTestId('cross-agent-adapt-preview').click();
    await expect(page.getByTestId('cross-agent-adapt-preview-result')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByTestId('cross-agent-adapt-preview-codex')).toBeVisible();
    await expect(page.getByTestId('cross-agent-adapt-diff-codex')).toContainText(
      'Always run tests',
    );

    const selectivePreviewCalls = backendHarness
      .calls()
      .filter(
        (call) =>
          call.type === 'invoke' &&
          call.command === 'agent_hub_preview_cross_agent_instruction',
      );
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
    const selectiveApplyCalls = backendHarness
      .calls()
      .filter(
        (call) =>
          call.type === 'invoke' &&
          call.command === 'agent_hub_apply_cross_agent_instruction',
      );
    expect(selectivePreviewCalls.length).toBeGreaterThanOrEqual(1);
    expect(fullPreviewCalls).toHaveLength(0);
    expect(fullApplyCalls).toHaveLength(0);
    expect(selectiveApplyCalls).toHaveLength(0);
  });
});
