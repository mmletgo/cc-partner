/**
 * E2E-AGENT-HUB-CORRECTION-001 — Agent Hub 生产边界与 legacy 迁移。
 *
 * Business Logic（为什么需要这个套件）:
 *   安全纠正后，用户只能进入本机 user Shell、三栏 Canonical 草稿与 observed
 *   portable inventory；旧 target matrix/conflict writer 不能因历史 URL 复活。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness 提供 scan-only workspace/inventory，验证 /claude-md、
 *   /claude-code、assetId/conflictId 的规范化、零 legacy API；库存行不打开详情侧栏。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-08-10T00:00:00.000Z';

/**
 * Business Logic（为什么需要）:
 *   GUI 与 sidecar 必须在 v4 scan-only 写策略下运行。
 *
 * Code Logic（做什么）:
 *   返回三 target 可扫描、不可认证写入的状态 DTO。
 */
function makeStatus(): Record<string, unknown> {
  return {
    enabled: true,
    backgroundEnabled: false,
    agentHubApiVersion: 4,
    ownerInstanceId: 'owner-agent-hub-correction-e2e',
    writeCompatible: true,
    probes: [
      {
        target: 'claude',
        support: 'scanOnly',
        version: '1.0.0',
        executable: '/usr/bin/claude',
        configRoot: '/tmp/.claude',
      },
      {
        target: 'codex',
        support: 'scanOnly',
        version: '1.0.0',
        executable: '/usr/bin/codex',
        configRoot: '/tmp/.codex',
      },
      {
        target: 'opencode',
        support: 'scanOnly',
        version: '1.0.0',
        executable: '/usr/bin/opencode',
        configRoot: '/tmp/.opencode',
      },
    ],
    conflictCount: 1,
    blockedMaterializationCount: 1,
  };
}

/**
 * Business Logic（为什么需要）:
 *   三栏必须能读取三个 Agent 的真实只读来源，同时保存 Hub Canonical 不依赖 CLI writer。
 *
 * Code Logic（做什么）:
 *   构造带 Canonical head、source content 与 blocked capabilities 的 workspace。
 */
function makeWorkspace(): Record<string, unknown> {
  const makeTarget = (
    target: 'claude' | 'codex' | 'opencode',
    path: string,
    role: 'native' | 'override',
  ) => ({
    target,
    cli: {
      installed: true,
      version: '1.0.0',
      configRoot: '/tmp/.' + target,
    },
    sources: [
      {
        sourceId: target + '-source',
        path,
        role,
        active: true,
        exists: true,
        nonEmpty: true,
        hash: target + '-source-hash',
        modifiedAt: TS,
        ownership: 'external',
        content: '# ' + target + ' source\n\nRun targeted tests.',
        contentTruncated: false,
      },
    ],
    effectiveSourceId: target + '-source',
    managedTargetPath: null,
    managementMode: 'unmanaged',
    capability: {
      scan: 'readOnly',
      write: 'blocked',
      remove: 'blocked',
      activate: 'blocked',
      reasonCode: 'USER_INSTRUCTION_WRITE_EVIDENCE_MISSING',
      evidenceIds: [],
    },
    projection: {
      state: 'none',
      desiredRevisionId: null,
      appliedRevisionId: null,
      observedHash: null,
      lastErrorCode: null,
    },
    availableActions: ['openFile'],
  });

  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'configured',
    healthState: 'healthy',
    canonical: {
      assetId: 'asset-user-instruction',
      displayName: 'User instructions',
      headRevisionId: 'revision-1',
      commonContent: 'Keep changes focused.',
      targetExtensions: {},
      deleted: false,
      contentTruncated: false,
    },
    targets: [
      makeTarget('claude', '/Users/e2e/.claude/CLAUDE.md', 'native'),
      makeTarget('codex', '/Users/e2e/.codex/AGENTS.override.md', 'override'),
      makeTarget('opencode', '/Users/e2e/.config/opencode/AGENTS.md', 'native'),
    ],
    inventorySnapshotHash: 'workspace-snapshot-correction-e2e',
    refreshedAt: TS,
  };
}

/**
 * Business Logic（为什么需要）:
 *   Portable 主 UI 必须从 observed inventory 渲染，且 scan-only 状态没有原生动作。
 *
 * Code Logic（做什么）:
 *   构造一个 Claude Skill 与三个 blocked target probe。
 */
function makePortableInventory(): Record<string, unknown> {
  return {
    inventorySnapshotHash: 'portable-snapshot-correction-e2e',
    refreshedAt: TS,
    stale: false,
    targets: (['claude', 'codex', 'opencode'] as const).map((target) => ({
      target,
      installed: true,
      version: '1.0.0',
      executable: '/usr/bin/' + target,
      configRoot: '/tmp/.' + target,
      scanCapability: 'supported',
      mutationCapability: 'blocked',
      reasonCode: 'CLI_WRITE_NOT_CERTIFIED',
      evidenceIds: [],
    })),
    items: [
      {
        inventoryItemId: 'claude-skill-safe-read',
        target: 'claude',
        loadedBy: 'claude',
        ownedBy: 'claude',
        originKind: 'native',
        nativeOutputCandidate: true,
        kind: 'skill',
        nativeId: 'safe-read',
        displayName: 'Safe Read Skill',
        description: 'Observed only',
        version: null,
        scopeId: 'user',
        scopeKind: 'user',
        projectId: null,
        projectOptedIn: true,
        sourcePath: '/tmp/.claude/skills/safe-read',
        sourceOrigin: 'standalone',
        parentPluginInventoryItemId: null,
        actualEnabled: true,
        contentHash: 'portable-content-hash',
        treeHash: 'portable-tree-hash',
        canonicalAssetId: 'asset-safe-read',
        canonicalRevisionId: 'revision-safe-read',
        managementState: 'hubManaged',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'blocked',
        capabilities: {
          canEnable: false,
          canDisable: false,
          canUninstall: false,
          canAdopt: false,
          canInstallToSourceTarget: false,
          reasonCode: 'CLI_WRITE_NOT_CERTIFIED',
          evidenceIds: [],
        },
        warnings: ['CLI_WRITE_NOT_CERTIFIED'],
      },
    ],
  };
}

/**
 * Business Logic（为什么需要）:
 *   每条 E2E 从相同的本机 user、scan-only 基线启动。
 *
 * Code Logic（做什么）:
 *   注册页面挂载所需命令；legacy list/get 故意返回错误，便于断言零调用。
 */
function registerAgentHubCorrectionBase(harness: PlaywrightBackendHarness): void {
  registerAppShellCommands(harness);
  harness.command('agent_hub_get_status', { kind: 'resolve', value: makeStatus() });
  harness.command('agent_hub_inspect_user_instruction_workspace', {
    kind: 'resolve',
    value: makeWorkspace(),
  });
  harness.command('agent_hub_inspect_portable_inventory', {
    kind: 'resolve',
    value: makePortableInventory(),
  });
  harness.command('agent_hub_list_assets', {
    kind: 'reject',
    error: { code: 'LEGACY_UI_UNAVAILABLE', message: 'legacy matrix is not a production source' },
  });
  harness.command('agent_hub_get_asset', {
    kind: 'reject',
    error: { code: 'LEGACY_UI_UNAVAILABLE', message: 'legacy detail is not a production source' },
  });
  harness.command('list_devices', { kind: 'resolve', value: [] });
}

/**
 * Business Logic（为什么需要）:
 *   URL 迁移不能暗中访问 legacy writer/read model。
 *
 * Code Logic（做什么）:
 *   从 harness 调用日志筛出旧 matrix 命令。
 */
function legacyCalls(harness: PlaywrightBackendHarness) {
  return harness.calls().filter(
    (call) =>
      call.type === 'invoke' &&
      (call.command === 'agent_hub_list_assets' || call.command === 'agent_hub_get_asset'),
  );
}

test.describe('E2E-AGENT-HUB-CORRECTION-001 production-only Agent Hub', () => {
  test('default route shows local-user three-pane and no legacy matrix', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubCorrectionBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-device-select')).toBeVisible();
    await expect(page.getByTestId('agent-hub-scope-lock')).toHaveCount(0);
    await expect(page.getByTestId('instruction-three-pane')).toBeVisible();
    await expect(page.getByTestId('instruction-write-blocked')).toBeVisible();
    await expect(page.getByTestId('agent-hub-asset-list')).toHaveCount(0);
    await expect(page.getByTestId('agent-hub-conflict-drawer')).toHaveCount(0);
    expect(legacyCalls(backendHarness)).toHaveLength(0);
  });

  test('/claude-md redirects to the canonical instruction workspace', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubCorrectionBase(backendHarness);

    await page.goto('/claude-md');
    await expect(page).toHaveURL(/\/agent-hub/, { timeout: 15_000 });
    await expect(page.getByTestId('instruction-three-pane')).toBeVisible();
    await expect(page.getByTestId('agent-hub-tab-instructions')).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(legacyCalls(backendHarness)).toHaveLength(0);
  });

  test('/claude-code canonicalizes to inventory and does not open a details sidebar', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubCorrectionBase(backendHarness);

    await page.goto('/claude-code');
    await expect(page).toHaveURL(/\/agent-hub\?tab=skill/, { timeout: 15_000 });
    await expect(page).not.toHaveURL(/section=|target=|kind=/);
    await expect(page.getByTestId('portable-inventory-workspace')).toBeVisible();

    await expect(
      page.getByTestId('portable-inventory-select-claude-skill-safe-read'),
    ).toBeVisible();
    await page.getByTestId('portable-inventory-select-claude-skill-safe-read').click();
    await expect(page.getByTestId('portable-asset-details-drawer')).toHaveCount(0);
    expect(legacyCalls(backendHarness)).toHaveLength(0);
  });

  test('assetId/conflictId deep link is migration-only and cannot reopen legacy drawer', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubCorrectionBase(backendHarness);

    await page.goto(
      '/agent-hub?section=assets&target=claude&kind=skill&assetId=legacy-a&conflictId=legacy-c',
    );
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-context-migration-notice')).toBeVisible();
    await expect(page.getByTestId('portable-inventory-workspace')).toBeVisible();
    await expect(page.getByTestId('agent-hub-conflict-drawer')).toHaveCount(0);
    await expect(page).not.toHaveURL(/assetId=|conflictId=|section=|target=|kind=/);
    expect(legacyCalls(backendHarness)).toHaveLength(0);
  });
});
