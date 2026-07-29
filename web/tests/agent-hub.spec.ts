/**
 * E2E-AGENT-HUB-A-001 / E2E-AGENT-HUB-B-001 / E2E-AGENT-HUB-C-001 —
 * Agent Hub Gate A + Gate B + Gate C UI journeys.
 *
 * Business Logic（为什么需要这个套件）:
 *   Gate A 交付 Multi-CLI Agent Hub 指令基础：用户必须能看到 CLI probe 状态、
 *   资产 target matrix、项目 opt-in preview/confirm，以及 Attention 冲突 deep link；
 *   旧 `/claude-md` 入口必须落到 Hub。Gate B 扩展 portable 资产矩阵：scope/kind 过滤、
 *   invocation alias、externalCollision adoption 预览、detached restore/remove、
 *   target enable 与 delete-everywhere。Gate C 扩展 LAN source-push 选择/进度、
 *   unsupported peer 报告、Git lane inspect/preview/confirm、credential 披露、
 *   stale preview 错误与 project mapping。本 L1 用 mock 锁定 UI 旅程，
 *   不宣称真实 Claude/Codex/OpenCode 写盘、marketplace 激活或真实多机 LAN Hub 复制。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness + installAppLocalStorage + registerAppShellCommands；
 *   mock agent_hub_* 命令与合法 DTO；断言 data-testid 旅程。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-29T00:00:00.000Z';

const ASSET_ID = 'asset-user-instruction-1';
const CONFLICT_ID = 'conflict-1';
const PROJECT_ID = 'proj-local-1';

type TargetCell = {
  target: 'claude' | 'codex' | 'opencode';
  desiredPresence: 'present' | 'absent';
  desiredEnabled: boolean;
  materializationStatus: string | null;
  lastError: string | null;
  requested: boolean;
  supported: boolean;
  sourceOnly: boolean;
  verified: boolean;
  invocationAlias?: string | null;
};

type AssetSummary = {
  assetId: string;
  scopeId: string;
  kind: string;
  displayName: string;
  logicalKey: string;
  originNamespace: string;
  policy: string;
  currentRevisionId: string | null;
  hasConflict: boolean;
  aggregateStatus: string;
  targets: TargetCell[];
};

type AssetDetail = AssetSummary & {
  blocks: Array<{
    id: string;
    mode: 'shared' | 'adapted' | 'targetOnly';
    commonMarkdown: string;
    variants?: Record<string, string> | null;
    sourceTarget?: 'claude' | 'codex' | 'opencode' | null;
  }>;
  contentMarkdown: string;
  conflicts: Array<{
    id: string;
    target?: 'claude' | 'codex' | 'opencode' | null;
    detailJson?: string;
    createdAt: string;
  }>;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   首屏 status 卡与 writeCompatible 门闸依赖合法 AgentHubStatus。
 *
 * Code Logic（这个函数做什么）:
 *   返回 enabled/writeCompatible 的三端 probe DTO。
 */
function makeStatus(overrides: Record<string, unknown> = {}) {
  return {
    enabled: true,
    backgroundEnabled: false,
    agentHubApiVersion: 1,
    ownerInstanceId: 'owner-e2e-1',
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
    conflictCount: 1,
    blockedMaterializationCount: 0,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表行需要三端 target cells 展示 present/absent 与 materialization。
 *
 * Code Logic（这个函数做什么）:
 *   构造含 Claude present + Codex/OpenCode absent 的 instruction 摘要。
 */
function makeAssetSummary(): AssetSummary {
  return {
    assetId: ASSET_ID,
    scopeId: 'user',
    kind: 'instruction',
    displayName: 'User instruction',
    logicalKey: 'user/instruction',
    originNamespace: 'claude',
    policy: 'targetOnly',
    currentRevisionId: 'rev-1',
    hasConflict: true,
    aggregateStatus: 'partial',
    targets: [
      {
        target: 'claude',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        lastError: null,
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: true,
      },
      {
        target: 'codex',
        desiredPresence: 'absent',
        desiredEnabled: false,
        materializationStatus: null,
        lastError: null,
        requested: false,
        supported: true,
        sourceOnly: false,
        verified: false,
      },
      {
        target: 'opencode',
        desiredPresence: 'absent',
        desiredEnabled: false,
        materializationStatus: 'unsupported',
        lastError: null,
        requested: false,
        supported: false,
        sourceOnly: false,
        verified: false,
      },
    ],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   冲突 deep link 与 resolve 需要 asset detail + conflicts。
 *
 * Code Logic（这个函数做什么）:
 *   扩展 summary，附带 targetOnly block 与 conflict 条目。
 */
function makeAssetDetail(): AssetDetail {
  const summary = makeAssetSummary();
  return {
    ...summary,
    contentMarkdown: '# User rules\n\nAlways confirm before edits.',
    blocks: [
      {
        id: 'block-1',
        mode: 'targetOnly',
        commonMarkdown: 'Always confirm before edits.',
        sourceTarget: 'claude',
        variants: { claude: 'Always confirm before edits.' },
      },
    ],
    conflicts: [
      {
        id: CONFLICT_ID,
        target: 'claude',
        detailJson: '{"reason":"external_drift"}',
        createdAt: TS,
      },
    ],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Agent Hub 页挂载即并行 getStatus/listAssets，preview/enable 走独立命令。
 *
 * Code Logic（这个函数做什么）:
 *   注册 AppShell + agent_hub_* 基线 resolve；可覆盖。
 */
function registerAgentHubBase(
  harness: PlaywrightBackendHarness,
  options: {
    status?: ReturnType<typeof makeStatus>;
    assets?: AssetSummary[];
    detail?: AssetDetail;
  } = {},
): void {
  registerAppShellCommands(harness);

  const status = options.status ?? makeStatus();
  const assets = options.assets ?? [makeAssetSummary()];
  const detail = options.detail ?? makeAssetDetail();

  harness.command('agent_hub_get_status', { kind: 'resolve', value: status });
  harness.command('agent_hub_list_assets', { kind: 'resolve', value: assets });
  harness.command('agent_hub_get_asset', { kind: 'resolve', value: detail });
  harness.command('agent_hub_preview_project', {
    kind: 'resolve',
    value: {
      projectId: PROJECT_ID,
      hubProjectId: 'hub-proj-1',
      path: '/tmp/demo-project',
      optedIn: false,
      checkouts: [{ path: '/tmp/demo-project', role: 'main' }],
      plannedActions: [{ action: 'bindCheckout', target: 'claude' }],
      warnings: ['preview-only: no disk write until enable'],
      noCommitNotice: 'Gate A preview never commits or mutates git index.',
      gitRemoteFingerprint: null,
    },
  });
  harness.command('agent_hub_enable_project', {
    kind: 'resolve',
    value: {
      projectId: PROJECT_ID,
      hubProjectId: 'hub-proj-1',
      optedIn: true,
      warnings: [],
    },
  });
  harness.command('agent_hub_resolve_conflict', {
    kind: 'resolve',
    value: {
      ...detail,
      hasConflict: false,
      conflicts: [],
    },
  });
  harness.command('agent_hub_set_target_binding', {
    kind: 'resolve',
    value: assets[0],
  });
  harness.command('agent_hub_set_target_enabled', {
    kind: 'resolve',
    value: assets[0],
  });
  harness.command('agent_hub_set_target_presence', {
    kind: 'resolve',
    value: assets[0],
  });
  harness.command('agent_hub_restore_detached_target', {
    kind: 'resolve',
    value: assets[0],
  });
  harness.command('agent_hub_delete_asset_everywhere', {
    kind: 'resolve',
    value: assets[0],
  });

  // Gate C LAN / Git 默认 no-op 基线（C-001 用例可覆盖）
  harness.command('list_devices', {
    kind: 'resolve',
    value: [
      {
        id: 'peer-ok',
        name: 'Peer OK',
        address: '192.168.1.10',
        port: 62116,
        online: true,
        capabilities: ['agent-hub.v1'],
        protoVersion: 1,
      },
      {
        id: 'peer-unsupported',
        name: 'Peer Old',
        address: '192.168.1.11',
        port: 62116,
        online: true,
        capabilities: [],
        protoVersion: 0,
      },
    ],
  });
  harness.command('agent_hub_preview_lan_push', {
    kind: 'resolve',
    value: {
      snapshotHash: 'a'.repeat(64),
      snapshotId: 'snap-e2e-1',
      selectionHash: 'b'.repeat(64),
      assetCount: 2,
      revisionCount: 3,
      credentialBearingAssetCount: 1,
      peerDeviceIds: ['peer-ok'],
      mode: 'fullHub',
      plaintextBackupDisclosure:
        'Hub snapshots store credential-bearing assets as plaintext bytes in CAS/archive.',
      hasCredentialBearingAssets: true,
    },
  });
  harness.command('agent_hub_start_lan_push', {
    kind: 'resolve',
    value: {
      requestId: 'req-e2e-1',
      selectionHash: 'b'.repeat(64),
      snapshotHash: 'a'.repeat(64),
      status: 'completed',
      targets: [
        {
          peerDeviceId: 'peer-ok',
          peerLabel: 'Peer OK',
          clientRequestId: 'req-e2e-1:peer-ok',
          status: 'committed',
          retryable: false,
          errorCode: null,
          transferId: 'xfer-1',
          missingObjectCount: 0,
          transferredObjectCount: 2,
          updatedAt: TS,
        },
        {
          peerDeviceId: 'peer-unsupported',
          peerLabel: 'Peer Old',
          clientRequestId: 'req-e2e-1:peer-unsupported',
          status: 'failed',
          retryable: false,
          errorCode: 'unsupported',
          transferId: null,
          missingObjectCount: 0,
          transferredObjectCount: 0,
          updatedAt: TS,
        },
      ],
    },
  });
  harness.command('agent_hub_inspect_git_lanes', {
    kind: 'resolve',
    value: {
      workdirPresent: true,
      localDeviceId: 'local-e2e',
      lanes: [
        {
          laneDeviceId: 'device-remote-1',
          snapshotHash: 'c'.repeat(64),
          snapshotId: 'snap-lane-1',
          sourceReplicaId: 'replica-1',
          assetCount: 2,
          revisionCount: 4,
          status: 'ok',
          errorCode: null,
        },
      ],
    },
  });
  harness.command('agent_hub_preview_git_import', {
    kind: 'resolve',
    value: {
      laneDeviceId: 'device-remote-1',
      snapshotId: 'snap-lane-1',
      snapshotHash: 'c'.repeat(64),
      sourceReplicaId: 'replica-1',
      assetCount: 2,
      revisionCount: 4,
      changeCounts: {
        added: 1,
        modified: 0,
        deleted: 0,
        conflict: 0,
        unchanged: 1,
        credentialBearing: 1,
      },
      assets: [
        {
          assetId: 'asset-mcp-1',
          kind: 'mcp',
          logicalKey: 'secret-mcp',
          displayName: 'Secret MCP',
          changeKind: 'added',
          hasCredential: true,
          localHead: null,
          remoteHead: 'rev-1',
          remoteDeleted: false,
        },
      ],
      projectCandidates: [
        {
          hubProjectId: 'hub-mapped',
          candidateKind: 'hubProjectId',
          candidateExternalId: 'hub-mapped',
          localWorkbenchProjectId: null,
        },
        {
          hubProjectId: 'hub-unmapped',
          candidateKind: 'hubProjectId',
          candidateExternalId: 'hub-unmapped',
          localWorkbenchProjectId: null,
        },
      ],
      resolvedMappings: [],
      plaintextBackupDisclosure:
        'Hub snapshots store credential-bearing assets as plaintext bytes in CAS/archive.',
      hasCredentialBearingAssets: true,
    },
  });
  harness.command('agent_hub_confirm_project_mapping', {
    kind: 'resolve',
    value: {
      hubProjectId: 'hub-mapped',
      localWorkbenchProjectId: 'wb-local-1',
      optedIn: false,
    },
  });
  harness.command('agent_hub_confirm_git_import', {
    kind: 'resolve',
    value: {
      laneDeviceId: 'device-remote-1',
      snapshotHash: 'c'.repeat(64),
      import: {
        snapshotId: 'snap-lane-1',
        snapshotHash: 'c'.repeat(64),
        importedAssetIds: ['asset-mcp-1'],
        insertedRevisions: 1,
        dedupedRevisions: 0,
        headsAdvanced: 1,
        conflictsOpened: 0,
        projectionsScheduled: 0,
        importedObjectHashes: ['d'.repeat(64)],
      },
      resolvedMappings: [
        {
          hubProjectId: 'hub-mapped',
          localWorkbenchProjectId: 'wb-local-1',
          optedIn: false,
        },
      ],
    },
  });
}

test.describe('E2E-AGENT-HUB-A-001 Agent Hub Gate A journey', () => {
  test('status card, target matrix, preview + enable confirmation', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-status-card')).toBeVisible();
    await expect(page.getByTestId('probe-claude')).toBeVisible();
    await expect(page.getByTestId('probe-codex')).toBeVisible();
    await expect(page.getByTestId('probe-opencode')).toBeVisible();

    // target matrix cells（AgentAssetRow）
    await expect(page.getByTestId(`agent-asset-row-${ASSET_ID}`)).toBeVisible();
    await expect(page.getByTestId(`agent-asset-targets-${ASSET_ID}`)).toBeVisible();
    await expect(
      page.getByTestId(`agent-asset-targets-${ASSET_ID}`).getByTestId('agent-target-claude'),
    ).toBeVisible();
    await expect(
      page.getByTestId(`agent-asset-targets-${ASSET_ID}`).getByTestId('agent-target-codex'),
    ).toBeVisible();
    await expect(
      page.getByTestId(`agent-asset-targets-${ASSET_ID}`).getByTestId('agent-target-opencode'),
    ).toBeVisible();

    // project preview dialog
    await page.getByTestId('agent-hub-open-preview').click();
    await expect(page.getByTestId('agent-hub-preview-dialog')).toBeVisible();
    await page.getByTestId('agent-hub-preview-project-id').fill(PROJECT_ID);
    await page.getByTestId('agent-hub-run-preview').click();
    await expect(page.getByTestId('agent-hub-preview-result')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByTestId('agent-hub-preview-result')).toContainText(
      'Gate A preview never commits',
    );

    // enable confirmation closes dialog after success
    await page.getByTestId('agent-hub-run-enable').click();
    await expect(page.getByTestId('agent-hub-preview-dialog')).toHaveCount(0, {
      timeout: 5_000,
    });
    await expect(page.getByTestId('agent-hub-page')).toBeVisible();
  });

  test('conflict deep link opens conflict drawer', async ({ page, backendHarness }) => {
    await installAppLocalStorage(page);
    registerAgentHubBase(backendHarness);

    await page.goto(
      `/agent-hub?assetId=${encodeURIComponent(ASSET_ID)}&conflictId=${encodeURIComponent(CONFLICT_ID)}`,
    );
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-conflict-drawer')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByTestId(`conflict-${CONFLICT_ID}`)).toBeVisible();
    await expect(page.getByTestId('agent-hub-conflict-drawer')).toContainText(CONFLICT_ID);
  });

  test('legacy /claude-md redirects to /agent-hub', async ({ page, backendHarness }) => {
    await installAppLocalStorage(page);
    registerAgentHubBase(backendHarness);

    await page.goto('/claude-md');
    await expect(page).toHaveURL(/\/agent-hub/, { timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-status-card')).toBeVisible();
  });

  test('legacy /claude-code redirects to /agent-hub and Gate C LAN push copy is visible', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubBase(backendHarness);

    await page.goto('/claude-code');
    await expect(page).toHaveURL(/\/agent-hub/, { timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-lan-push-notice')).toBeVisible();
    await expect(page.getByTestId(`agent-asset-aggregate-${ASSET_ID}`)).toBeVisible();
  });
});

/**
 * Business Logic（为什么需要这个函数）:
 *   Gate B 需要 portable Skill 行：alias、collision、detached、delete everywhere 与 scope/kind 过滤。
 *
 * Code Logic（这个函数做什么）:
 *   构造 skill 摘要，含 Claude present/verified、Codex collision、OpenCode detached。
 */
function makePortableSkillSummary(): AssetSummary {
  return {
    assetId: 'asset-skill-review-1',
    scopeId: 'user',
    kind: 'skill',
    displayName: 'review',
    logicalKey: 'review',
    originNamespace: 'cc-partner',
    policy: 'shared',
    currentRevisionId: 'rev-skill-1',
    hasConflict: false,
    aggregateStatus: 'partial',
    targets: [
      {
        target: 'claude',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        lastError: null,
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: true,
        invocationAlias: 'cc-partner__review',
      },
      {
        target: 'codex',
        desiredPresence: 'present',
        desiredEnabled: false,
        materializationStatus: 'externalCollision',
        lastError: 'pending_legacy_adoption',
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: false,
        invocationAlias: 'cc-partner__review',
      },
      {
        target: 'opencode',
        desiredPresence: 'absent',
        desiredEnabled: false,
        materializationStatus: 'detached',
        lastError: null,
        requested: false,
        supported: true,
        sourceOnly: false,
        verified: false,
        invocationAlias: null,
      },
    ],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   kind/scope 过滤需要第二条不同 kind 的资产对照。
 *
 * Code Logic（这个函数做什么）:
 *   构造 project-scoped MCP 摘要。
 */
function makeMcpAssetSummary(): AssetSummary {
  return {
    assetId: 'asset-mcp-private-1',
    scopeId: 'project-demo',
    kind: 'mcp',
    displayName: 'private-api',
    logicalKey: 'private-api',
    originNamespace: 'legacy',
    policy: 'shared',
    currentRevisionId: 'rev-mcp-1',
    hasConflict: false,
    aggregateStatus: 'full',
    targets: [
      {
        target: 'claude',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        lastError: null,
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: true,
      },
      {
        target: 'codex',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        lastError: null,
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: true,
      },
      {
        target: 'opencode',
        desiredPresence: 'present',
        desiredEnabled: true,
        materializationStatus: 'synced',
        lastError: null,
        requested: true,
        supported: true,
        sourceOnly: false,
        verified: true,
      },
    ],
  };
}

test.describe('E2E-AGENT-HUB-B-001 Agent Hub Gate B portable matrix', () => {
  test('scope/kind filters, alias, collision recovery, target and everywhere delete', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    const skill = makePortableSkillSummary();
    const mcp = makeMcpAssetSummary();
    registerAgentHubBase(backendHarness, {
      assets: [skill, mcp],
      detail: {
        ...skill,
        contentMarkdown: '',
        blocks: [],
        conflicts: [],
      },
    });

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-filters')).toBeVisible();
    await expect(page.getByTestId(`agent-asset-row-${skill.assetId}`)).toBeVisible();
    await expect(page.getByTestId(`agent-asset-row-${mcp.assetId}`)).toBeVisible();

    // kind filter keeps skill, hides mcp
    await page.getByTestId('agent-hub-filter-kind').fill('skill');
    await expect(page.getByTestId(`agent-asset-row-${skill.assetId}`)).toBeVisible();
    await expect(page.getByTestId(`agent-asset-row-${mcp.assetId}`)).toHaveCount(0);

    // scope filter can isolate project MCP after clearing kind
    await page.getByTestId('agent-hub-filter-kind').fill('');
    await page.getByTestId('agent-hub-filter-scope').fill('project-demo');
    await expect(page.getByTestId(`agent-asset-row-${mcp.assetId}`)).toBeVisible();
    await expect(page.getByTestId(`agent-asset-row-${skill.assetId}`)).toHaveCount(0);
    await page.getByTestId('agent-hub-filter-scope').fill('');
    await expect(page.getByTestId(`agent-asset-row-${skill.assetId}`)).toBeVisible();

    // target status cells + invocation alias
    const skillTargets = page.getByTestId(`agent-asset-targets-${skill.assetId}`);
    await expect(skillTargets.getByTestId('agent-target-claude')).toBeVisible();
    await expect(skillTargets.getByTestId('agent-target-codex')).toBeVisible();
    await expect(skillTargets.getByTestId('agent-target-opencode')).toBeVisible();
    await expect(skillTargets.getByTestId('agent-target-verified-claude')).toBeVisible();
    await expect(skillTargets.getByTestId('agent-target-invocation-claude')).toContainText(
      'cc-partner__review',
    );
    await expect(page.getByTestId(`agent-asset-canonical-${skill.assetId}`)).toContainText(
      'review',
    );
    await expect(page.getByTestId(`agent-asset-aggregate-${skill.assetId}`)).toBeVisible();

    // externalCollision → adoption preview dialog
    await page.getByTestId(`agent-target-collision-${skill.assetId}-codex`).click();
    await expect(page.getByTestId('agent-hub-adoption-dialog')).toBeVisible();
    await expect(page.getByTestId('agent-hub-adoption-preview')).toBeVisible();
    await expect(page.getByTestId('adoption-canonical')).toContainText('review');
    await expect(page.getByTestId('agent-hub-lan-push-gate-c')).toBeVisible();
    await page.getByTestId('agent-hub-adoption-close').click();
    await expect(page.getByTestId('agent-hub-adoption-dialog')).toHaveCount(0);

    // detached restore / remove on OpenCode cell
    await expect(
      page.getByTestId(`agent-target-restore-${skill.assetId}-opencode`),
    ).toBeVisible();
    await page.getByTestId(`agent-target-restore-${skill.assetId}-opencode`).click();
    await expect(
      page.getByTestId(`agent-target-remove-${skill.assetId}-opencode`),
    ).toBeVisible();
    await page.getByTestId(`agent-target-remove-${skill.assetId}-opencode`).click();

    // delete everywhere confirm flow
    await page.getByTestId(`agent-asset-delete-everywhere-${skill.assetId}`).click();
    await expect(page.getByTestId('agent-hub-delete-everywhere-dialog')).toBeVisible();
    await page.getByTestId('agent-hub-delete-everywhere-confirm').click();
    await expect(page.getByTestId('agent-hub-delete-everywhere-dialog')).toHaveCount(0, {
      timeout: 5_000,
    });
  });

  test('target enable toggle remains available on present verified cell', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    const skill = makePortableSkillSummary();
    // present + enabled + synced should still expose disable on Claude
    registerAgentHubBase(backendHarness, {
      assets: [skill],
      detail: {
        ...skill,
        contentMarkdown: '',
        blocks: [],
        conflicts: [],
      },
    });
    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId(`agent-target-toggle-${skill.assetId}-claude`)).toBeVisible();
    await page.getByTestId(`agent-target-toggle-${skill.assetId}-claude`).click();
    // still on page after toggle mutation mock
    await expect(page.getByTestId('agent-hub-page')).toBeVisible();
  });
});

test.describe('E2E-AGENT-HUB-C-001 Agent Hub Gate C replication UI', () => {
  test('LAN selection/progress, unsupported peer, Git inspect, credential, mapping, Attention link', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAgentHubBase(backendHarness);

    await page.goto('/agent-hub');
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-lan-push-notice')).toBeVisible();
    await expect(page.getByTestId('agent-hub-open-lan-push')).toBeVisible();
    await expect(page.getByTestId('agent-hub-open-git-import')).toBeVisible();

    // --- LAN source-push：选择 peer、preview、per-target progress（含 unsupported）---
    await page.getByTestId('agent-hub-open-lan-push').click();
    await expect(page.getByTestId('lan-push-dialog')).toBeVisible();
    await expect(page.getByTestId('lan-push-plaintext-disclosure')).toBeVisible();
    await expect(page.getByTestId('lan-push-peer-peer-ok')).toBeVisible();
    await expect(page.getByTestId('lan-push-peer-peer-unsupported')).toBeVisible();
    await page.getByTestId('lan-push-peer-peer-ok').check();
    await page.getByTestId('lan-push-peer-peer-unsupported').check();
    await page.getByTestId('lan-push-mode-fullHub').check();
    await page.getByTestId('lan-push-preview-btn').click();
    await expect(page.getByTestId('lan-push-preview')).toBeVisible({ timeout: 5_000 });
    await page.getByTestId('lan-push-start-btn').click();
    await expect(page.getByTestId('lan-push-report')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByTestId('lan-push-target-peer-ok')).toContainText(/committed/i);
    await expect(page.getByTestId('lan-push-target-peer-unsupported')).toContainText(
      /failed|unsupported/i,
    );
    // 关闭 LAN dialog（ghost cancel）
    await page.getByRole('button', { name: /cancel|取消/i }).first().click();
    await expect(page.getByTestId('lan-push-dialog')).toHaveCount(0, { timeout: 5_000 });

    // --- Git lane inspect / credential disclosure / mapping / confirm ---
    await page.getByTestId('agent-hub-open-git-import').click();
    await expect(page.getByTestId('git-import-drawer')).toBeVisible();
    await expect(page.getByTestId('git-import-plaintext-disclosure')).toBeVisible();
    await page.getByTestId('git-import-inspect-btn').click();
    await expect(page.getByTestId('git-import-lane-list')).toBeVisible({ timeout: 5_000 });
    await page.getByTestId('git-import-lane-device-remote-1').click();
    await page.getByTestId('git-import-preview-btn').click();
    await expect(page.getByTestId('git-import-preview')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByTestId('git-import-asset-asset-mcp-1')).toBeVisible();
    // map one project; leave hub-unmapped unmapped
    await page.getByTestId('git-import-map-hub-mapped').fill('wb-local-1');
    await page.getByTestId('git-import-map-confirm-hub-mapped').click();
    await expect(page.getByTestId('git-import-last-mapping')).toBeVisible({ timeout: 5_000 });
    await page.getByTestId('git-import-confirm-btn').click();
    await expect(page.getByTestId('git-import-outcome')).toBeVisible({ timeout: 5_000 });

    // stale preview：覆盖 confirm 返回 reject
    backendHarness.command('agent_hub_confirm_git_import', {
      kind: 'reject',
      error: { message: 'previewStale: snapshot hash changed', code: 'conflict' },
    });
    await page.getByTestId('git-import-confirm-btn').click();
    await expect(page.getByTestId('git-import-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByTestId('git-import-error')).toContainText(/stale|conflict|hash/i);

    // Attention deep link 仍可达（Gate A/C 导航-only）
    await page.goto(
      `/agent-hub?assetId=${encodeURIComponent(ASSET_ID)}&conflictId=${encodeURIComponent(CONFLICT_ID)}`,
    );
    await expect(page.getByTestId('agent-hub-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('agent-hub-conflict-drawer')).toBeVisible({
      timeout: 10_000,
    });
  });
});
