/**
 * Attention Inbox 快照运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面/移动 Inbox 在状态写入前必须拒绝错误 enum、缺字段与坏 target，
 *   避免部分危险列表进入 Provider。
 *
 * Code Logic（这个模块做什么）:
 *   解码 AttentionSnapshot / Item / Target / Counts，对齐 lib/types/attention。
 */

import type {
  AttentionCategory,
  AttentionCounts,
  AttentionFreshness,
  AttentionItem,
  AttentionSnapshot,
  AttentionSourceKind,
  AttentionTarget,
} from '../types/attention';
import {
  arrayDecoder,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  unionDecoder,
  type Decoder,
} from '../runtimeSchema';

const attentionCategoryDecoder: Decoder<AttentionCategory> = enumDecoder('AttentionCategory', [
  'decision',
  'blocked',
  'environment',
] as const);

const attentionFreshnessDecoder: Decoder<AttentionFreshness> = enumDecoder('AttentionFreshness', [
  'live',
  'cached',
] as const);

const attentionSourceKindDecoder: Decoder<AttentionSourceKind> = enumDecoder('AttentionSourceKind', [
  'orchestratorHumanReview',
  'orchestratorBlocked',
  'remoteOutboxFailed',
  'workbenchDependency',
  'agentNeedsInput',
  'agentFailed',
  'experimentNeedsDecision',
  'agentHubConflict',
  'agentHubProjectionBlocked',
] as const);

const projectKindDecoder = enumDecoder('AttentionProjectKind', ['local', 'remote'] as const);

const attentionProjectDecoder = nullableDecoder(
  objectDecoder('AttentionProject', {
    id: stringDecoder,
    name: stringDecoder,
    kind: projectKindDecoder,
  }),
);

const attentionDeviceDecoder = nullableDecoder(
  objectDecoder('AttentionDevice', {
    id: stringDecoder,
    name: stringDecoder,
  }),
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   target 是导航真源，错误 kind 不得渲染可点行。
 *
 * Code Logic（这个 decoder 做什么）:
 *   按 kind 判别联合解码 orchestratorTask/remoteOutbox/settings。
 */
export const attentionTargetDecoder: Decoder<AttentionTarget> = unionDecoder<AttentionTarget>(
  'AttentionTarget',
  [
    objectDecoder('AttentionTargetOrchestratorTask', {
      kind: literalDecoder('orchestratorTask'),
      projectId: stringDecoder,
      taskId: stringDecoder,
    }),
    objectDecoder('AttentionTargetRemoteOutbox', {
      kind: literalDecoder('remoteOutbox'),
      projectId: stringDecoder,
      outboxId: stringDecoder,
    }),
    objectDecoder('AttentionTargetSettings', {
      kind: literalDecoder('settings'),
      tab: literalDecoder('dependencies'),
    }),
    objectDecoder('AttentionTargetAgentSession', {
      kind: literalDecoder('agentSession'),
      projectId: stringDecoder,
      worktreeId: optionalDecoder(nullableDecoder(stringDecoder)),
      terminalSessionId: stringDecoder,
      agentSessionId: stringDecoder,
    }),
    objectDecoder('AttentionTargetExperiment', {
      kind: literalDecoder('experiment'),
      projectId: stringDecoder,
      experimentId: stringDecoder,
    }),
    objectDecoder('AttentionTargetAgentHubAsset', {
      kind: literalDecoder('agentHubAsset'),
      assetId: stringDecoder,
      conflictId: optionalDecoder(nullableDecoder(stringDecoder)),
    }),
  ],
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   单条 Inbox 条目是列表最小单元。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格校验 category/sourceKind/freshness/target 与可空 project/device/cachedAt。
 */
export const attentionItemDecoder: Decoder<AttentionItem> = objectDecoder('AttentionItem', {
  id: stringDecoder,
  category: attentionCategoryDecoder,
  sourceKind: attentionSourceKindDecoder,
  title: stringDecoder,
  summary: stringDecoder,
  updatedAt: stringDecoder,
  freshness: attentionFreshnessDecoder,
  cachedAt: nullableDecoder(stringDecoder),
  project: attentionProjectDecoder,
  device: attentionDeviceDecoder,
  target: attentionTargetDecoder,
  readAt: optionalDecoder(nullableDecoder(stringDecoder)),
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   badge 用 unreadTotal；旧后端缺 unread_* 时回落 0，避免 fail-closed 抹掉 Inbox。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 total/decision/blocked/environment 与四类 unread；缺失 unread 注入 0。
 */
export const attentionCountsDecoder: Decoder<AttentionCounts> = objectDecoder<AttentionCounts>(
  'AttentionCounts',
  {
    total: numberDecoder,
    decision: numberDecoder,
    blocked: numberDecoder,
    environment: numberDecoder,
    unreadTotal: numberDecoder,
    unreadDecision: numberDecoder,
    unreadBlocked: numberDecoder,
    unreadEnvironment: numberDecoder,
  },
  {
    defaults: {
      unreadTotal: 0,
      unreadDecision: 0,
      unreadBlocked: 0,
      unreadEnvironment: 0,
    },
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   Provider 以完整 snapshot 为单位缓存，部分列表不得进入状态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 generatedAt + counts + items[] + myDeviceId（旧后端缺省空串）。
 */
export const attentionSnapshotDecoder: Decoder<AttentionSnapshot> = objectDecoder<AttentionSnapshot>(
  'AttentionSnapshot',
  {
    generatedAt: stringDecoder,
    counts: attentionCountsDecoder,
    items: arrayDecoder(attentionItemDecoder),
    myDeviceId: stringDecoder,
  },
  {
    defaults: {
      myDeviceId: '',
    },
  },
);
