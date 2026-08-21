/**
 * Attention 全局 Inbox 域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面/移动 Inbox 共享 snapshot 契约，需与 orchestrator/workbench 实现解耦，仅消费语义 target。
 *
 * Code Logic（这个模块做什么）:
 *   导出 Attention 分类、新鲜度、来源、跳转 target、条目、计数与快照 DTO。
 */

/**
 * Attention 分类。
 *
 * Business Logic（为什么需要这个类型）:
 *   全局 Inbox 按“需要决策 / 运行受阻 / 环境受阻”三档呈现，badge 与分组都依赖稳定字面量。
 *
 * Code Logic（这个类型做什么）:
 *   对齐 Rust AttentionCategory 序列化值：decision | blocked | environment。
 */
export type AttentionCategory = 'decision' | 'blocked' | 'environment';

/**
 * Attention 条目新鲜度。
 *
 * Business Logic（为什么需要这个类型）:
 *   远端 mirror 回退时用户需要区分 live 与 cached，避免把陈旧数据当实时状态。
 *
 * Code Logic（这个类型做什么）:
 *   对齐 Rust AttentionFreshness：live | cached。
 */
export type AttentionFreshness = 'live' | 'cached';

/**
 * Attention 条目来源类型。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端按 sourceKind 映射动作文案与导航语义，禁止页面散落业务判断。
 *
 * Code Logic（这个类型做什么）:
 *   对齐 Rust AttentionSourceKind 四个稳定字面量。
 */
export type AttentionSourceKind =
  | 'orchestratorHumanReview'
  | 'orchestratorBlocked'
  | 'remoteOutboxFailed'
  | 'workbenchDependency'
  | 'agentNeedsInput'
  | 'agentFailed'
  | 'experimentNeedsDecision'
  | 'agentHubConflict'
  | 'agentHubProjectionBlocked';

/**
 * Attention 语义化跳转目标。
 *
 * Business Logic（为什么需要这个类型）:
 *   后端只返回语义 target，由桌面/移动端各自映射导航，禁止携带后端 URL。
 *
 * Code Logic（这个类型做什么）:
 *   discriminated union：orchestratorTask / remoteOutbox / settings /
 *   agentSession（v2）/ experiment（v2）/ agentHubAsset（Agent Hub）。
 */
export type AttentionTarget =
  | { kind: 'orchestratorTask'; projectId: string; taskId: string }
  | { kind: 'remoteOutbox'; projectId: string; outboxId: string }
  | { kind: 'settings'; tab: 'dependencies' }
  | {
      kind: 'agentSession';
      projectId: string;
      worktreeId?: string | null;
      terminalSessionId: string;
      agentSessionId: string;
    }
  | { kind: 'experiment'; projectId: string; experimentId: string }
  | { kind: 'agentHubAsset'; assetId: string; conflictId?: string | null };

/**
 * 单条 Attention 条目 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   Inbox 列表、badge 与 Provider 都以条目为最小展示单元，字段契约必须跨端一致。
 *
 * Code Logic（字段说明）:
 *   camelCase 对齐 Rust AttentionItemDto；project/device/cachedAt 可空。
 */
export interface AttentionItem {
  id: string;
  category: AttentionCategory;
  sourceKind: AttentionSourceKind;
  title: string;
  summary: string;
  updatedAt: string;
  freshness: AttentionFreshness;
  cachedAt: string | null;
  project: { id: string; name: string; kind: 'local' | 'remote' } | null;
  device: { id: string; name: string } | null;
  target: AttentionTarget;
  /** 本设备视角的已读时间；未读时后端省略该字段。 */
  readAt?: string | null;
}

/**
 * Attention 分类计数。
 *
 * Business Logic（为什么需要这个类型）:
 *   列表分组空态看 total 与三类计数；导航 badge 由当天未读派生，不直接用 unreadTotal。
 *
 * Code Logic（字段说明）:
 *   total/decision/blocked/environment 含已读；unread_* 由本设备 read_set 派生。
 */
export interface AttentionCounts {
  total: number;
  decision: number;
  blocked: number;
  environment: number;
  unreadTotal: number;
  unreadDecision: number;
  unreadBlocked: number;
  unreadEnvironment: number;
}

/**
 * Attention 快照 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   一次聚合成功后才产出完整快照，失败不得返回部分列表；Provider 以 snapshot 为单位缓存。
 *
 * Code Logic（字段说明）:
 *   generatedAt + counts + items + myDeviceId，对齐 Rust AttentionSnapshotDto。
 */
export interface AttentionSnapshot {
  generatedAt: string;
  counts: AttentionCounts;
  items: AttentionItem[];
  myDeviceId: string;
}
