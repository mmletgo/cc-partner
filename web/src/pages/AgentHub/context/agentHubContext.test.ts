/**
 * Agent Hub URL 上下文 pure 模型测试。
 *
 * Business Logic: 锁定 agent/scope/device/project/tab/lane/adapt 深链往返与 legacy section 映射。
 * Code Logic: URLSearchParams 构造 + parse/write/mapLegacy 断言。
 */

import { describe, expect, test } from 'vitest';
import {
  AGENT_HUB_PORTABLE_USER_CAPABILITY,
  AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY,
  getAgentHubContextCapability,
  getAgentHubDraftIdentity,
  mapLegacySection,
  parseAgentHubContext,
  parseWorkbenchHostedAgentHubContext,
  peerAllowsUserInstructionThreePane,
  peerAllowsUserPortableInventory,
  portableInventoryTargetForHubContext,
  writeAgentHubContext,
  writeWorkbenchHostedAgentHubContext,
  type AgentHubContext,
} from './agentHubContext';

/** 默认上下文（空 query）。 */
const DEFAULT_CONTEXT: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  instructionLane: 'exclusive',
  assetLane: 'equipped',
  adaptView: false,
};

describe('parseAgentHubContext', () => {
  test('empty params yield defaults (claude / user / local / instructions / exclusive)', () => {
    const ctx = parseAgentHubContext(new URLSearchParams());
    expect(ctx).toEqual(DEFAULT_CONTEXT);
  });

  test('view=adapt sets adaptView true without altering other defaults', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('view=adapt'));
    expect(ctx.adaptView).toBe(true);
    expect(ctx.agent).toBe('claude');
    expect(ctx.scope).toBe('user');
    expect(ctx.tab).toBe('instructions');
    expect(ctx.instructionLane).toBe('exclusive');
    expect(ctx.deviceId).toBeNull();
    expect(ctx.projectKey).toBeNull();
  });

  test('adapt=1 alias also sets adaptView', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('adapt=1'));
    expect(ctx.adaptView).toBe(true);
  });

  test('explicit project navigation params round-trip as the selected owner', () => {
    const params = new URLSearchParams(
      'agent=opencode&scope=project&project=wb:proj-1&tab=mcp&view=adapt',
    );
    expect(parseAgentHubContext(params)).toEqual({
      agent: 'opencode',
      scope: 'project',
      deviceId: null,
      projectKey: 'wb:proj-1',
      tab: 'mcp',
      instructionLane: 'exclusive',
      assetLane: 'equipped',
      adaptView: true,
    });
  });

  test('lane=adapted on instructions is preserved', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('lane=adapted'));
    expect(ctx.tab).toBe('instructions');
    expect(ctx.instructionLane).toBe('adapted');
  });

  test('lane on non-instructions tab is forced back to default exclusive', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('tab=skill&lane=common'));
    expect(ctx.tab).toBe('skill');
    expect(ctx.instructionLane).toBe('exclusive');
  });

  test('assetLane=store on skill is preserved', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('tab=skill&assetLane=store'));
    expect(ctx.tab).toBe('skill');
    expect(ctx.assetLane).toBe('store');
  });

  test('assetLane on non-store tab is forced back to equipped', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('tab=mcp&assetLane=store'));
    expect(ctx.tab).toBe('mcp');
    expect(ctx.assetLane).toBe('equipped');
  });

  test('skill to command keeps store assetLane', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('tab=command&assetLane=store'));
    expect(ctx.tab).toBe('command');
    expect(ctx.assetLane).toBe('store');
  });

  test('store lane inspects all agents; equipped follows the current agent', () => {
    expect(
      portableInventoryTargetForHubContext({
        tab: 'skill',
        assetLane: 'store',
        agent: 'claude',
      }),
    ).toBe('all');
    expect(
      portableInventoryTargetForHubContext({
        tab: 'command',
        assetLane: 'store',
        agent: 'grok',
      }),
    ).toBe('all');
    expect(
      portableInventoryTargetForHubContext({
        tab: 'skill',
        assetLane: 'equipped',
        agent: 'grok',
      }),
    ).toBe('grok');
    expect(
      portableInventoryTargetForHubContext({
        tab: 'mcp',
        assetLane: 'store',
        agent: 'codex',
      }),
    ).toBe('codex');
  });

  test('peer user URL keeps the peer and ignores the project field', () => {
    const params = new URLSearchParams(
      'scope=user&deviceId=peer-abc&project=should-ignore',
    );
    const ctx = parseAgentHubContext(params);
    expect(ctx.scope).toBe('user');
    expect(ctx.deviceId).toBe('peer-abc');
    expect(ctx.projectKey).toBeNull();
  });

  test('project URL keeps the project and ignores deviceId', () => {
    const params = new URLSearchParams(
      'scope=project&project=remote:dev1:/path&deviceId=should-ignore',
    );
    const ctx = parseAgentHubContext(params);
    expect(ctx.scope).toBe('project');
    expect(ctx.projectKey).toBe('remote:dev1:/path');
    expect(ctx.deviceId).toBeNull();
  });

  test('invalid enum tokens fall back to defaults', () => {
    const params = new URLSearchParams(
      'agent=gpt&scope=everywhere&tab=settings&view=wizard&lane=nope',
    );
    expect(parseAgentHubContext(params)).toEqual(DEFAULT_CONTEXT);
  });

  test('legacy section=assets&target=codex&kind=skill yields agent=codex tab=skill', () => {
    const params = new URLSearchParams('section=assets&target=codex&kind=skill');
    const ctx = parseAgentHubContext(params);
    expect(ctx.agent).toBe('codex');
    expect(ctx.tab).toBe('skill');
    // assets 默认用户级库存
    expect(ctx.scope).toBe('user');
    expect(ctx.adaptView).toBe(false);
    expect(ctx.instructionLane).toBe('exclusive');
  });

  test('legacy section via mapLegacySection then merge still works with parse', () => {
    const params = new URLSearchParams('section=assets&target=codex&kind=skill');
    const legacy = mapLegacySection(params.get('section'));
    expect(legacy.tab).toBe('skill');
    const ctx = parseAgentHubContext(params);
    expect(ctx).toMatchObject({ agent: 'codex', tab: 'skill' });
  });

  test('explicit agent/tab win over legacy target/kind', () => {
    const params = new URLSearchParams(
      'section=assets&target=codex&kind=skill&agent=opencode&tab=plugin',
    );
    const ctx = parseAgentHubContext(params);
    expect(ctx.agent).toBe('opencode');
    expect(ctx.tab).toBe('plugin');
  });
});

describe('mapLegacySection', () => {
  test('maps userInstructions to user + instructions', () => {
    expect(mapLegacySection('userInstructions')).toEqual({
      scope: 'user',
      tab: 'instructions',
    });
  });

  test('maps projectInstructions to project + instructions', () => {
    expect(mapLegacySection('projectInstructions')).toEqual({
      scope: 'project',
      tab: 'instructions',
    });
  });

  test('maps assets and portableAssets to skill tab default', () => {
    expect(mapLegacySection('assets')).toEqual({ tab: 'skill' });
    expect(mapLegacySection('portableAssets')).toEqual({ tab: 'skill' });
  });

  test('unknown or null section yields empty patch', () => {
    expect(mapLegacySection(null)).toEqual({});
    expect(mapLegacySection('')).toEqual({});
    expect(mapLegacySection('not-a-section')).toEqual({});
  });
});

describe('writeAgentHubContext', () => {
  test('defaults omit noise keys from empty params', () => {
    const next = writeAgentHubContext(new URLSearchParams(), DEFAULT_CONTEXT);
    expect(next.toString()).toBe('');
  });

  test('write preserves peer owner fields and safe navigation', () => {
    const original: AgentHubContext = {
      agent: 'codex',
      scope: 'user',
      deviceId: 'peer-42',
      projectKey: null,
      tab: 'command',
      instructionLane: 'exclusive',
      assetLane: 'equipped',
      adaptView: true,
    };
    const written = writeAgentHubContext(new URLSearchParams('conflictId=c1'), original);
    // 保留无关 deep link
    expect(written.get('conflictId')).toBe('c1');
    expect(written.get('deviceId')).toBe('peer-42');
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('lane=common on instructions is written and round-trips', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      instructionLane: 'common',
    };
    const written = writeAgentHubContext(new URLSearchParams(), original);
    expect(written.get('lane')).toBe('common');
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('default exclusive lane is omitted from empty params', () => {
    const written = writeAgentHubContext(new URLSearchParams(), DEFAULT_CONTEXT);
    expect(written.get('lane')).toBeNull();
    expect(parseAgentHubContext(written).instructionLane).toBe('exclusive');
  });

  test('lane is stripped when tab is not instructions', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      tab: 'skill',
      instructionLane: 'exclusive',
    };
    const withStaleLane = new URLSearchParams('lane=adapted&tab=skill');
    const written = writeAgentHubContext(withStaleLane, original);
    expect(written.get('lane')).toBeNull();
    expect(parseAgentHubContext(written).instructionLane).toBe('exclusive');
  });

  test('assetLane=store on skill is written and round-trips', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      tab: 'skill',
      assetLane: 'store',
    };
    const written = writeAgentHubContext(new URLSearchParams(), original);
    expect(written.get('assetLane')).toBe('store');
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('default equipped assetLane is omitted from empty params', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      tab: 'skill',
    };
    const written = writeAgentHubContext(new URLSearchParams(), original);
    expect(written.get('assetLane')).toBeNull();
    expect(parseAgentHubContext(written).assetLane).toBe('equipped');
  });

  test('assetLane is stripped when tab is not skill or command', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      tab: 'mcp',
      assetLane: 'equipped',
    };
    const withStale = new URLSearchParams('assetLane=store&tab=mcp');
    const written = writeAgentHubContext(withStale, original);
    expect(written.get('assetLane')).toBeNull();
    expect(parseAgentHubContext(written).assetLane).toBe('equipped');
  });

  test('project scope round-trips with its project identity', () => {
    const original: AgentHubContext = {
      agent: 'claude',
      scope: 'project',
      deviceId: null,
      projectKey: 'local:proj-x',
      tab: 'instructions',
      instructionLane: 'exclusive',
      assetLane: 'equipped',
      adaptView: false,
    };
    const written = writeAgentHubContext(new URLSearchParams(), original);
    expect(written.get('scope')).toBe('project');
    expect(written.get('project')).toBe('local:proj-x');
    expect(written.get('deviceId')).toBeNull();
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('writing modern context strips legacy section/target/kind so parse stays stable', () => {
    const legacy = new URLSearchParams('section=assets&target=codex&kind=skill&bridge=/tmp');
    const written = writeAgentHubContext(legacy, DEFAULT_CONTEXT);
    expect(written.get('section')).toBeNull();
    expect(written.get('target')).toBeNull();
    expect(written.get('kind')).toBeNull();
    expect(written.get('bridge')).toBe('/tmp');
    expect(parseAgentHubContext(written)).toEqual(DEFAULT_CONTEXT);
  });
});

describe('Agent Hub context capability', () => {
  test('local user, peer user, and local project expose distinct capabilities', () => {
    expect(getAgentHubContextCapability(DEFAULT_CONTEXT)).toBe('direct');
    expect(
      getAgentHubContextCapability({ ...DEFAULT_CONTEXT, deviceId: 'peer-a' }),
    ).toBe('remote');
    expect(
      getAgentHubContextCapability({
        ...DEFAULT_CONTEXT,
        scope: 'project',
        projectKey: 'local:p1',
      }),
    ).toBe('project');
    expect(
      getAgentHubContextCapability({ ...DEFAULT_CONTEXT, projectKey: 'mixed' }),
    ).toBe('unsupported');
  });

  test('peerAllowsUserInstructionThreePane requires online plus capability token', () => {
    expect(peerAllowsUserInstructionThreePane(null)).toBe(false);
    expect(
      peerAllowsUserInstructionThreePane({
        online: false,
        capabilities: [AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY],
      }),
    ).toBe(false);
    expect(peerAllowsUserInstructionThreePane({ online: true, capabilities: [] })).toBe(false);
    expect(
      peerAllowsUserInstructionThreePane({
        online: true,
        capabilities: [AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY],
      }),
    ).toBe(true);
  });

  test('peerAllowsUserPortableInventory requires online plus capability token', () => {
    expect(peerAllowsUserPortableInventory(null)).toBe(false);
    expect(
      peerAllowsUserPortableInventory({
        online: false,
        capabilities: [AGENT_HUB_PORTABLE_USER_CAPABILITY],
      }),
    ).toBe(false);
    expect(peerAllowsUserPortableInventory({ online: true, capabilities: [] })).toBe(false);
    expect(
      peerAllowsUserPortableInventory({
        online: true,
        capabilities: [AGENT_HUB_PORTABLE_USER_CAPABILITY],
      }),
    ).toBe(true);
  });

  test('draft identity includes owner and agent but excludes transient tab/lane/view', () => {
    expect(
      getAgentHubDraftIdentity({
        ...DEFAULT_CONTEXT,
        agent: 'codex',
        deviceId: 'peer-a',
        tab: 'plugin',
        instructionLane: 'exclusive',
        adaptView: true,
      }),
    ).toEqual({
      scope: 'user',
      deviceId: 'peer-a',
      projectKey: null,
      agent: 'codex',
    });
  });
});

describe('workbench hosted agent hub context', () => {
  test('keeps view=projectAgent and projectId while freezing project identity', () => {
    const written = writeWorkbenchHostedAgentHubContext(
      new URLSearchParams('projectId=local-1&worktreeId=w1'),
      {
        ...DEFAULT_CONTEXT,
        scope: 'project',
        projectKey: 'local-1',
        tab: 'skill',
        adaptView: true,
      },
    );
    expect(written.get('view')).toBe('projectAgent');
    expect(written.get('adapt')).toBe('1');
    expect(written.get('projectId')).toBe('local-1');
    expect(written.get('worktreeId')).toBe('w1');
    expect(written.get('tab')).toBe('skill');
    expect(written.get('scope')).toBeNull();
    expect(written.get('project')).toBeNull();
    expect(written.get('deviceId')).toBeNull();

    const parsed = parseWorkbenchHostedAgentHubContext(written, 'local-1');
    expect(parsed.scope).toBe('project');
    expect(parsed.projectKey).toBe('local-1');
    expect(parsed.deviceId).toBeNull();
    expect(parsed.tab).toBe('skill');
    expect(parsed.adaptView).toBe(true);
  });
});
