/**
 * crossAgentPresentation pure helper tests.
 *
 * Business Logic（为什么需要）:
 *   选择性适配页的目标排除、peer 门闩、preview-before-apply 与 DTO 解析不得漂移。
 *
 * Code Logic（做什么）:
 *   直接断言 pure function；无 React / API。
 */

import { describe, expect, test } from 'vitest';
import {
  adaptModeTone,
  canRunCrossAgentApply,
  canRunCrossAgentFullApply,
  canRunCrossAgentFullPreview,
  canRunCrossAgentPreview,
  canSelectDestination,
  countApplicableDestinations,
  countApplicableFullItems,
  defaultDestinationsForSource,
  defaultFullDestination,
  destinationCandidates,
  fullPlanHasAllKinds,
  isPeerContextBlocked,
  normalizeAdaptMode,
  parseCrossAgentApplyResults,
  parseCrossAgentFullPlan,
  parseCrossAgentPreview,
  sanitizeDestinations,
  toggleDestinationSelection,
  toggleFullPlanItemIncluded,
  type CrossAgentFullPlan,
  type CrossAgentPreviewReport,
} from './crossAgentPresentation';

function previewFixture(
  overrides: Partial<CrossAgentPreviewReport> = {},
): CrossAgentPreviewReport {
  return {
    source: 'claude',
    kind: 'instruction',
    needsAdaptation: false,
    planHash: 'plan-1',
    destinations: [
      {
        destination: 'codex',
        mode: 'shared',
        path: '/tmp/.codex/AGENTS.md',
        renderedHash: 'abc',
        unifiedDiff: 'diff',
        partialBlockers: [],
        canApply: false,
      },
    ],
    ...overrides,
  };
}

describe('destinationCandidates / sanitize', () => {
  test('excludes source from destination candidates', () => {
    expect(destinationCandidates('claude')).toEqual(['codex', 'opencode', 'grok', 'gemini', 'cursor', 'pi']);
    expect(destinationCandidates('codex')).toEqual(['claude', 'opencode', 'grok', 'gemini', 'cursor', 'pi']);
  });

  test('cannot select source as destination', () => {
    expect(canSelectDestination('claude', 'claude')).toBe(false);
    expect(canSelectDestination('claude', 'codex')).toBe(true);
  });

  test('sanitizeDestinations drops source and duplicates', () => {
    expect(sanitizeDestinations('claude', ['claude', 'codex', 'codex', 'opencode'])).toEqual([
      'codex',
      'opencode',
    ]);
  });

  test('toggleDestinationSelection ignores source target', () => {
    expect(toggleDestinationSelection('claude', ['codex'], 'claude')).toEqual(['codex']);
    expect(toggleDestinationSelection('claude', ['codex'], 'opencode')).toEqual([
      'codex',
      'opencode',
    ]);
    expect(toggleDestinationSelection('claude', ['codex', 'opencode'], 'codex')).toEqual([
      'opencode',
    ]);
  });

  test('defaultDestinationsForSource is all others', () => {
    expect(defaultDestinationsForSource('opencode')).toEqual(['claude', 'codex', 'grok', 'gemini', 'cursor', 'pi']);
  });
});

describe('peer and gates', () => {
  test('peer context blocked when deviceId set', () => {
    expect(isPeerContextBlocked(null)).toBe(false);
    expect(isPeerContextBlocked('')).toBe(false);
    expect(isPeerContextBlocked('peer-1')).toBe(true);
  });

  test('preview gate blocks peer, empty markdown, source-in-dest, unconfirmed scope', () => {
    const base = {
      deviceId: null as string | null,
      source: 'claude' as const,
      destinations: ['codex' as const],
      sourceMarkdown: 'Always run tests.',
      busy: false,
      scope: 'user' as const,
      projectKey: null as string | null,
      scopeConfirmed: true,
    };
    expect(canRunCrossAgentPreview(base).ok).toBe(true);

    expect(canRunCrossAgentPreview({ ...base, deviceId: 'peer-a' }).reason).toBe('peerBlocked');
    expect(canRunCrossAgentPreview({ ...base, sourceMarkdown: '  ' }).reason).toBe(
      'emptyMarkdown',
    );
    expect(canRunCrossAgentPreview({ ...base, destinations: [] }).reason).toBe(
      'emptyDestinations',
    );
    expect(
      canRunCrossAgentPreview({ ...base, destinations: ['claude', 'codex'] }).reason,
    ).toBe('sourceInDestinations');
    expect(canRunCrossAgentPreview({ ...base, scopeConfirmed: false }).reason).toBe(
      'scopeUnconfirmed',
    );
    expect(
      canRunCrossAgentPreview({
        ...base,
        scope: 'project',
        projectKey: null,
      }).reason,
    ).toBe('projectBlocked');
  });

  test('apply gate is always unavailable', () => {
    const preview = previewFixture();
    expect(
      canRunCrossAgentApply({ deviceId: null, preview, busy: false }).reason,
    ).toBe('applyUnavailable');
    expect(
      canRunCrossAgentApply({ deviceId: null, preview: null, busy: false }).reason,
    ).toBe('applyUnavailable');
    expect(
      canRunCrossAgentApply({ deviceId: 'peer', preview, busy: false }).reason,
    ).toBe('applyUnavailable');
  });
});

describe('parse preview/apply', () => {
  test('parses strict preview-only report and rejects unknown/writable rows', () => {
    const parsed = parseCrossAgentPreview({
      source: 'claude',
      kind: 'instruction',
      needsAdaptation: true,
      planHash: 'plan-1',
      destinations: [
        {
          destination: 'codex',
          mode: 'shared',
          path: '/a',
          canApply: false,
          partialBlockers: ['x'],
        },
        {
          destination: 'opencode',
          mode: 'residual',
          path: '/b',
          canApply: false,
          partialBlockers: ['CROSS_AGENT_PREVIEW_ONLY'],
        },
      ],
    });
    expect(parsed?.needsAdaptation).toBe(true);
    expect(parsed?.destinations[0]?.mode).toBe('shared');
    expect(parsed?.destinations[1]?.mode).toBe('residual');
    expect(
      parseCrossAgentPreview({
        source: 'claude',
        kind: 'instruction',
        needsAdaptation: false,
        planHash: 'plan-2',
        destinations: [
          {
            destination: 'codex',
            mode: 'weird',
            path: '/a',
            canApply: false,
            partialBlockers: [],
          },
        ],
      }),
    ).toBeNull();
    expect(
      parseCrossAgentPreview({
        source: 'claude',
        kind: 'instruction',
        needsAdaptation: false,
        planHash: 'plan-3',
        destinations: [
          {
            destination: 'codex',
            mode: 'shared',
            path: '/a',
            canApply: true,
            partialBlockers: [],
          },
        ],
      }),
    ).toBeNull();
    expect(normalizeAdaptMode('adapted')).toBe('adapted');
  });

  test('parseCrossAgentApplyResults tolerates bad rows', () => {
    const rows = parseCrossAgentApplyResults([
      { destination: 'codex', status: 'applied', path: '/a' },
      { destination: 'nope', path: '/b' },
      null,
    ]);
    expect(rows).toEqual([
      { destination: 'codex', status: 'applied', path: '/a', errorCode: null },
    ]);
  });

  test('countApplicable and mode tones', () => {
    const preview = previewFixture({
      destinations: [
        {
          destination: 'codex',
          mode: 'shared',
          path: '/a',
          partialBlockers: [],
          canApply: false,
        },
        {
          destination: 'opencode',
          mode: 'residual',
          path: '/b',
          partialBlockers: ['r'],
          canApply: false,
        },
      ],
    });
    expect(countApplicableDestinations(preview)).toBe(0);
    expect(adaptModeTone('shared')).toBe('success');
    expect(adaptModeTone('residual')).toBe('danger');
  });

  test('full mode: single destination gates and plan parse/toggle', () => {
    expect(defaultFullDestination('claude')).toBe('codex');
    expect(
      canRunCrossAgentFullPreview({
        deviceId: null,
        source: 'claude',
        destination: 'codex',
        sourceMarkdown: 'hi',
        busy: false,
        scope: 'user',
        projectKey: null,
        scopeConfirmed: true,
      }).ok,
    ).toBe(false);
    expect(
      canRunCrossAgentFullPreview({
        deviceId: null,
        source: 'claude',
        destination: null,
        sourceMarkdown: 'hi',
        busy: false,
        scope: 'user',
        projectKey: null,
        scopeConfirmed: true,
      }).reason,
    ).toBe('fullUnavailable');
    expect(
      canRunCrossAgentFullPreview({
        deviceId: 'peer',
        source: 'claude',
        destination: 'codex',
        sourceMarkdown: 'hi',
        busy: false,
        scope: 'user',
        projectKey: null,
        scopeConfirmed: true,
      }).reason,
    ).toBe('fullUnavailable');

    const plan = parseCrossAgentFullPlan({
      source: 'claude',
      destination: 'codex',
      scope: 'user',
      planHash: 'abc',
      generator: 'stub',
      items: [
        {
          kind: 'instruction',
          logicalKey: 'instruction:user',
          action: 'create',
          path: '/tmp/AGENTS.md',
          included: true,
        },
        {
          kind: 'skill',
          logicalKey: 'skill:demo',
          action: 'skip',
          path: '/tmp/skill',
          residualReason: 'stub',
          included: true,
        },
        {
          kind: 'command',
          logicalKey: 'inventory:empty:command',
          action: 'skip',
          path: '',
          residualReason: 'none',
          included: false,
        },
        {
          kind: 'mcp',
          logicalKey: 'inventory:empty:mcp',
          action: 'skip',
          path: '',
          residualReason: 'none',
          included: false,
        },
        {
          kind: 'plugin',
          logicalKey: 'inventory:empty:plugin',
          action: 'skip',
          path: '',
          residualReason: 'none',
          included: false,
        },
      ],
    }) as CrossAgentFullPlan;
    expect(fullPlanHasAllKinds(plan)).toBe(true);
    expect(countApplicableFullItems(plan)).toBe(1);
    expect(canRunCrossAgentFullApply({ deviceId: null, plan, busy: false }).ok).toBe(false);
    expect(
      canRunCrossAgentFullApply({ deviceId: null, plan: null, busy: false }).reason,
    ).toBe('fullUnavailable');

    const toggled = toggleFullPlanItemIncluded(plan, 'instruction:user');
    expect(toggled.items[0]?.included).toBe(false);
    expect(
      canRunCrossAgentFullApply({ deviceId: null, plan: toggled, busy: false }).reason,
    ).toBe('fullUnavailable');
  });
});
