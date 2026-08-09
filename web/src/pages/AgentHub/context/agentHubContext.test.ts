/**
 * Agent Hub URL 上下文 pure 模型测试。
 *
 * Business Logic: 锁定 agent/scope/device/project/tab/lane/adapt 深链往返与 legacy section 映射。
 * Code Logic: URLSearchParams 构造 + parse/write/mapLegacy 断言。
 */

import { describe, expect, test } from 'vitest';
import {
  mapLegacySection,
  parseAgentHubContext,
  writeAgentHubContext,
  type AgentHubContext,
} from './agentHubContext';

/** 默认上下文（空 query）。 */
const DEFAULT_CONTEXT: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  instructionLane: 'common',
  adaptView: false,
};

describe('parseAgentHubContext', () => {
  test('empty params yield defaults (claude / user / local / instructions / common)', () => {
    const ctx = parseAgentHubContext(new URLSearchParams());
    expect(ctx).toEqual(DEFAULT_CONTEXT);
  });

  test('view=adapt sets adaptView true without altering other defaults', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('view=adapt'));
    expect(ctx.adaptView).toBe(true);
    expect(ctx.agent).toBe('claude');
    expect(ctx.scope).toBe('user');
    expect(ctx.tab).toBe('instructions');
    expect(ctx.instructionLane).toBe('common');
    expect(ctx.deviceId).toBeNull();
    expect(ctx.projectKey).toBeNull();
  });

  test('explicit new params override defaults', () => {
    const params = new URLSearchParams(
      'agent=opencode&scope=project&project=wb:proj-1&tab=mcp&view=adapt',
    );
    expect(parseAgentHubContext(params)).toEqual({
      agent: 'opencode',
      scope: 'project',
      deviceId: null,
      projectKey: 'wb:proj-1',
      tab: 'mcp',
      instructionLane: 'common',
      adaptView: true,
    });
  });

  test('lane=adapted on instructions is preserved', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('lane=adapted'));
    expect(ctx.tab).toBe('instructions');
    expect(ctx.instructionLane).toBe('adapted');
  });

  test('lane on non-instructions tab is forced back to common', () => {
    const ctx = parseAgentHubContext(new URLSearchParams('tab=skill&lane=exclusive'));
    expect(ctx.tab).toBe('skill');
    expect(ctx.instructionLane).toBe('common');
  });

  test('user scope keeps deviceId and clears projectKey', () => {
    const params = new URLSearchParams(
      'scope=user&deviceId=peer-abc&project=should-ignore',
    );
    const ctx = parseAgentHubContext(params);
    expect(ctx.scope).toBe('user');
    expect(ctx.deviceId).toBe('peer-abc');
    expect(ctx.projectKey).toBeNull();
  });

  test('project scope keeps projectKey and clears deviceId', () => {
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
    expect(ctx.instructionLane).toBe('common');
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

  test('round-trip write/parse preserves non-default context', () => {
    const original: AgentHubContext = {
      agent: 'codex',
      scope: 'user',
      deviceId: 'peer-42',
      projectKey: null,
      tab: 'command',
      instructionLane: 'common',
      adaptView: true,
    };
    const written = writeAgentHubContext(new URLSearchParams('conflictId=c1'), original);
    // 保留无关 deep link
    expect(written.get('conflictId')).toBe('c1');
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('lane=exclusive on instructions is written and round-trips', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      instructionLane: 'exclusive',
    };
    const written = writeAgentHubContext(new URLSearchParams(), original);
    expect(written.get('lane')).toBe('exclusive');
    expect(parseAgentHubContext(written)).toEqual(original);
  });

  test('lane is stripped when tab is not instructions', () => {
    const original: AgentHubContext = {
      ...DEFAULT_CONTEXT,
      tab: 'skill',
      instructionLane: 'common',
    };
    const withStaleLane = new URLSearchParams('lane=adapted&tab=skill');
    const written = writeAgentHubContext(withStaleLane, original);
    expect(written.get('lane')).toBeNull();
    expect(parseAgentHubContext(written).instructionLane).toBe('common');
  });

  test('project scope round-trip', () => {
    const original: AgentHubContext = {
      agent: 'claude',
      scope: 'project',
      deviceId: null,
      projectKey: 'local:proj-x',
      tab: 'instructions',
      instructionLane: 'common',
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
