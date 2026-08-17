/**
 * Plugin package pure helper 合同。
 *
 * Business Logic: mixed package / delete preview / partial blockers 必须精确。
 * Code Logic: vitest 覆盖 blockers 与 delete 分组。
 */

import { describe, expect, test } from 'vitest';
import { allHubTargets } from '@/lib/agentCatalog';
import type { PluginPackageReport } from '@/lib/types/agentHub';
import {
  groupDeletePreview,
  isPluginFullySynced,
  listPluginPartialBlockers,
  orderedComponentTargets,
  pluginAggregateTone,
  summarizeDeletePreview,
} from './pluginPackagePresentation';

function mixedReport(): PluginPackageReport {
  return {
    packageAssetId: 'pkg-1',
    packageDisplayName: 'demo-plugin',
    sourceTarget: 'claude',
    destinationTarget: 'codex',
    aggregateStatus: 'partial',
    activationState: 'planned',
    diagnostics: ['hook_mapping_absent'],
    partialBlockers: [],
    components: [
      {
        kind: 'skill',
        assetId: 'c-skill',
        displayName: 'Skill A',
        canonicalRevisionId: 'rev-skill-1',
        ownership: 'packageOwned',
        sourceTarget: 'claude',
        targets: [
          {
            target: 'claude',
            status: 'verified',
            reasons: [],
            projectedPaths: ['skills/a/SKILL.md'],
            materializedAlias: 'skill-a',
          },
          {
            target: 'codex',
            status: 'partial',
            reasons: ['portable_partial'],
            projectedPaths: [],
          },
          {
            target: 'opencode',
            status: 'sourceOnly',
            reasons: ['not_requested'],
            projectedPaths: [],
          },
        ],
      },
      {
        kind: 'hook',
        assetId: 'c-hook',
        displayName: 'Hook B',
        canonicalRevisionId: 'rev-hook-1',
        ownership: 'shared',
        sourceTarget: 'claude',
        residualReason: 'targetOnly_no_mapping',
        targets: [
          {
            target: 'claude',
            status: 'verified',
            reasons: [],
            projectedPaths: ['hooks/b.json'],
          },
          {
            target: 'codex',
            status: 'blocked',
            reasons: ['hook_mapping_absent'],
            projectedPaths: [],
          },
          {
            target: 'opencode',
            status: 'blocked',
            reasons: ['hook_mapping_absent'],
            projectedPaths: [],
          },
        ],
      },
    ],
    residuals: [
      {
        residualTarget: 'claude',
        residualKind: 'runtime',
        treeManifestHash: 'abc',
        included: true,
        reasons: [],
      },
      {
        residualTarget: 'codex',
        residualKind: 'npm',
        treeManifestHash: 'def',
        included: false,
        reasons: ['cross_runtime_omitted'],
      },
    ],
    deletePreview: {
      packageAssetId: 'pkg-1',
      packageDisplayName: 'demo-plugin',
      components: [
        {
          assetId: 'c-skill',
          displayName: 'Skill A',
          kind: 'skill',
          ownership: 'packageOwned',
          decision: 'tombstoneOwned',
        },
        {
          assetId: 'c-hook',
          displayName: 'Hook B',
          kind: 'hook',
          ownership: 'shared',
          decision: 'preserveShared',
        },
        {
          assetId: 'c-standalone',
          displayName: 'Standalone C',
          kind: 'agent',
          ownership: 'standalone',
          decision: 'preserveStandalone',
        },
      ],
    },
  };
}

describe('pluginPackagePresentation', () => {
  test('each component has its own target matrix', () => {
    const report = mixedReport();
    for (const component of report.components) {
      const ordered = orderedComponentTargets(component);
      expect(ordered.map((o) => o.target)).toEqual(allHubTargets());
      expect(ordered.filter((o) => o.cell != null)).toHaveLength(3);
      expect(
        new Set(ordered.filter((o) => o.cell != null).map((o) => o.cell!.status)).size,
      ).toBeGreaterThan(1);
    }
  });

  test('partial blockers name exact component/target reasons', () => {
    const blockers = listPluginPartialBlockers(mixedReport());
    expect(blockers).toContain('Skill A@codex:portable_partial');
    expect(blockers).toContain('Hook B@codex:hook_mapping_absent');
    expect(blockers).toContain('residual@codex:cross_runtime_omitted');
    expect(blockers.some((b) => b.includes('Skill A@claude'))).toBe(false);
  });

  test('delete preview groups tombstone vs preserve', () => {
    const groups = groupDeletePreview(mixedReport().deletePreview);
    expect(groups.tombstone.map((c) => c.assetId)).toEqual(['c-skill']);
    expect(groups.preserve.map((c) => c.assetId).sort()).toEqual(['c-hook', 'c-standalone']);
  });

  test('never treats partial as fully synced green', () => {
    expect(isPluginFullySynced('partial')).toBe(false);
    expect(isPluginFullySynced('full')).toBe(true);
    expect(pluginAggregateTone('partial')).toBe('warn');
    expect(pluginAggregateTone('full')).toBe('success');
  });

  test('prefers explicit partialBlockers when present', () => {
    const report = mixedReport();
    report.partialBlockers = ['exact:blocker'];
    expect(listPluginPartialBlockers(report)).toEqual(['exact:blocker']);
  });

  test('summarizeDeletePreview counts tombstone and preserve for portable details', () => {
    const summary = summarizeDeletePreview(mixedReport().deletePreview);
    expect(summary).toEqual({
      tombstoneCount: 1,
      preserveCount: 2,
      total: 3,
    });
  });
});
