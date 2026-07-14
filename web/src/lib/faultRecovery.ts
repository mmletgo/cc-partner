/**
 * 传输/权限/边界故障分类与恢复策略（纯函数 + 终端事件桥状态机）。
 *
 * Business Logic（为什么需要这个模块）:
 *   网络离线、超时、malformed DTO、权限拒绝、冲突与 LAN/跨站边界拒绝等故障
 *   必须在前端有一致的 cache/stale/retry/乐观回滚策略，避免半危险态或乐观发散；
 *   终端 disconnect/reconnect 还要精确回收 listener 且不重复入账输入。
 *
 * Code Logic（这个模块做什么）:
 *   提供 classifyTransportFault / planFaultRecovery 与 createTerminalFaultBridge；
 *   不依赖测试全局或生产环境变量；FaultProfile 字符串与 harness 对齐。
 */

/** 与 backendHarness.FaultProfile 对齐的故障剖面（生产模块自声明，避免依赖 tests/）。 */
export type FaultProfile =
  | 'networkOffline'
  | 'timeout'
  | 'malformedJson'
  | 'permissionDenied'
  | 'conflict'
  | 'dbBusy'
  | 'lanBoundaryRejected'
  | 'crossSiteRejected';

/** 缓存策略：保留并标 stale / 清空 / 不写不改。 */
export type FaultCachePolicy = 'keepStale' | 'clear' | 'none';

/** 故障分类结果（typed，可供 UI/store 消费）。 */
export interface FaultClassification {
  /** 故障剖面 kind；无法识别时为 unknown。 */
  kind: FaultProfile | 'unknown';
  /** 稳定错误码（如 NETWORK_OFFLINE）。 */
  code: string;
  /** 是否适合自动/手动重试。 */
  retryable: boolean;
  /** 是否向用户展示 Retry 控件。 */
  showRetry: boolean;
  /** 缓存策略。 */
  cachePolicy: FaultCachePolicy;
  /**
   * 错误路径上禁止把乐观态提交为权威态。
   * 恒为 false，防止乐观发散。
   */
  allowOptimisticCommit: false;
  /** 可选表面提示（不包含 payload）。 */
  surfaceHint?: string;
}

/** Error-like 输入（含 harness createFaultError 挂的 code）。 */
export interface FaultErrorLike {
  name?: string;
  message?: string;
  code?: string;
}

/** classifyTransportFault 可接受的输入。 */
export type ClassifyTransportFaultInput = FaultProfile | FaultErrorLike | unknown;

/** planFaultRecovery 输入。 */
export interface PlanFaultRecoveryInput {
  classification: FaultClassification;
  hasCache: boolean;
  optimisticApplied: boolean;
}

/** 恢复计划：缓存/重试/乐观回滚决策。 */
export interface FaultRecoveryPlan {
  /** 是否保留已有缓存内容。 */
  keepCache: boolean;
  /** 是否将保留的缓存标记为 stale。 */
  markStale: boolean;
  /** 是否清空缓存（fail-closed）。 */
  clearCache: boolean;
  /** 是否展示 Retry。 */
  showRetry: boolean;
  /** 是否回滚已应用的乐观态（optimisticApplied 时恒 true）。 */
  rollbackOptimistic: boolean;
  /** 错误路径禁止乐观提交，恒 false。 */
  allowOptimisticCommit: false;
  /** 守卫：不得出现乐观发散。 */
  noOptimisticDivergence: true;
}

/** 终端事件桥对外合同（unit 可测）。 */
export interface TerminalFaultBridge {
  /** 当前已注册 listener 数。 */
  readonly listenerCount: number;
  /** 已入账输入次数（disconnect 期间不增加；幂等 key 不重复）。 */
  readonly inputCount: number;
  /** 是否处于已连接可入账状态。 */
  readonly connected: boolean;
  /** disconnect 前快照的 baseline listener 数。 */
  readonly baselineListenerCount: number;
  /**
   * Business Logic（为什么需要这个方法）:
   *   终端输出/状态需要订阅事件；disconnect 后应可精确回收。
   *
   * Code Logic（这个方法做什么）:
   *   注册 handler，返回 unlisten；connected=false 时仍允许注册到 pending 池（不计入 active）。
   */
  listen(handler: (payload: unknown) => void): () => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   连接断开时必须归零 listener，避免泄漏与重复投递。
   *
   * Code Logic（这个方法做什么）:
   *   保存 baseline 与 handlers，清空 active listener，停止输入入账。
   */
  disconnect(): void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   恢复连接后 listener 数必须精确回到 disconnect 前 baseline。
   *
   * Code Logic（这个方法做什么）:
   *   按保存的 handlers 重建 active 集合；不自动重放输入。
   */
  reconnect(): void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   终端输入在断线期间不得入账，重连后 replay 不得重复计数。
   *
   * Code Logic（这个方法做什么）:
   *   connected 且 key 未见过时记一笔并返回 true；否则 false。
   */
  acceptInput(key: string, payload?: string): boolean;
  /**
   * Business Logic（为什么需要这个方法）:
   *   调用方可只“记账”而不关心返回值（与 acceptInput 同幂等语义）。
   *
   * Code Logic（这个方法做什么）:
   *   委托 acceptInput(key, payload)。
   */
  noteInput(key: string, payload?: string): void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   测试与桥接层需要向当前 active listeners 投递事件。
   *
   * Code Logic（这个方法做什么）:
   *   仅当 connected 时同步调用全部 active handler。
   */
  emit(payload: unknown): void;
}

/** profile → 稳定 code。 */
const PROFILE_CODES: Readonly<Record<FaultProfile, string>> = {
  networkOffline: 'NETWORK_OFFLINE',
  timeout: 'TIMEOUT',
  malformedJson: 'MALFORMED_JSON',
  permissionDenied: 'PERMISSION_DENIED',
  conflict: 'CONFLICT',
  dbBusy: 'DB_BUSY',
  lanBoundaryRejected: 'LAN_BOUNDARY_REJECTED',
  crossSiteRejected: 'CROSS_SITE_REJECTED',
};

/** code → profile 反查。 */
const CODE_TO_PROFILE: Readonly<Record<string, FaultProfile>> = {
  NETWORK_OFFLINE: 'networkOffline',
  TIMEOUT: 'timeout',
  MALFORMED_JSON: 'malformedJson',
  PERMISSION_DENIED: 'permissionDenied',
  CONFLICT: 'conflict',
  DB_BUSY: 'dbBusy',
  LAN_BOUNDARY_REJECTED: 'lanBoundaryRejected',
  CROSS_SITE_REJECTED: 'crossSiteRejected',
};

const ALL_PROFILES: readonly FaultProfile[] = [
  'networkOffline',
  'timeout',
  'malformedJson',
  'permissionDenied',
  'conflict',
  'dbBusy',
  'lanBoundaryRejected',
  'crossSiteRejected',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   策略表是分类与恢复的单一事实来源，避免 switch 分散。
 *
 * Code Logic（这个函数做什么）:
 *   按 FaultProfile 返回完整 FaultClassification（allowOptimisticCommit 恒 false）。
 */
function classificationForProfile(profile: FaultProfile): FaultClassification {
  switch (profile) {
    case 'networkOffline':
      return {
        kind: 'networkOffline',
        code: PROFILE_CODES.networkOffline,
        retryable: true,
        showRetry: true,
        cachePolicy: 'keepStale',
        allowOptimisticCommit: false,
        surfaceHint: 'offline',
      };
    case 'timeout':
      return {
        kind: 'timeout',
        code: PROFILE_CODES.timeout,
        retryable: true,
        showRetry: true,
        cachePolicy: 'keepStale',
        allowOptimisticCommit: false,
        surfaceHint: 'timeout',
      };
    case 'malformedJson':
      return {
        kind: 'malformedJson',
        code: PROFILE_CODES.malformedJson,
        retryable: true,
        showRetry: true,
        cachePolicy: 'clear',
        allowOptimisticCommit: false,
        surfaceHint: 'malformed',
      };
    case 'permissionDenied':
      return {
        kind: 'permissionDenied',
        code: PROFILE_CODES.permissionDenied,
        retryable: true,
        showRetry: true,
        cachePolicy: 'none',
        allowOptimisticCommit: false,
        surfaceHint: 'permission',
      };
    case 'conflict':
      return {
        kind: 'conflict',
        code: PROFILE_CODES.conflict,
        retryable: true,
        showRetry: true,
        cachePolicy: 'none',
        allowOptimisticCommit: false,
        surfaceHint: 'conflict',
      };
    case 'dbBusy':
      return {
        kind: 'dbBusy',
        code: PROFILE_CODES.dbBusy,
        retryable: true,
        showRetry: true,
        cachePolicy: 'keepStale',
        allowOptimisticCommit: false,
        surfaceHint: 'db-busy',
      };
    case 'lanBoundaryRejected':
      return {
        kind: 'lanBoundaryRejected',
        code: PROFILE_CODES.lanBoundaryRejected,
        retryable: false,
        showRetry: false,
        cachePolicy: 'none',
        allowOptimisticCommit: false,
        surfaceHint: 'lan-boundary',
      };
    case 'crossSiteRejected':
      return {
        kind: 'crossSiteRejected',
        code: PROFILE_CODES.crossSiteRejected,
        retryable: false,
        showRetry: false,
        cachePolicy: 'none',
        allowOptimisticCommit: false,
        surfaceHint: 'cross-site',
      };
    default: {
      const _exhaustive: never = profile;
      throw new Error(`Unhandled fault profile: ${String(_exhaustive)}`);
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断字符串是否为已知 FaultProfile。
 *
 * Code Logic（这个函数做什么）:
 *   线性查找 ALL_PROFILES。
 */
function isFaultProfile(value: string): value is FaultProfile {
  return (ALL_PROFILES as readonly string[]).includes(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从未知输入尽量取出 name/message/code，兼容 Error 与普通对象。
 *
 * Code Logic（这个函数做什么）:
 *   返回规范化的 FaultErrorLike；无法解析时字段为空。
 */
function asErrorLike(input: unknown): FaultErrorLike {
  if (!input || typeof input !== 'object') {
    if (typeof input === 'string') {
      return { message: input };
    }
    return {};
  }
  const record = input as Record<string, unknown>;
  const name = typeof record.name === 'string' ? record.name : undefined;
  const message = typeof record.message === 'string' ? record.message : undefined;
  const code = typeof record.code === 'string' ? record.code : undefined;
  return { name, message, code };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI/store 需要把 transport/harness 错误统一成 typed 分类，再决定 cache/retry。
 *
 * Code Logic（这个函数做什么）:
 *   接受 FaultProfile 字符串、带 code 的 Error-like、AbortError/NetworkError/SyntaxError 等；
 *   映射到 FaultClassification；无法识别时 kind=unknown 且 fail-closed（clear + showRetry）。
 */
export function classifyTransportFault(input: ClassifyTransportFaultInput): FaultClassification {
  if (typeof input === 'string' && isFaultProfile(input)) {
    return classificationForProfile(input);
  }

  const errorLike = asErrorLike(input);
  if (errorLike.code) {
    const fromCode = CODE_TO_PROFILE[errorLike.code];
    if (fromCode) {
      return classificationForProfile(fromCode);
    }
  }

  const name = (errorLike.name ?? '').toLowerCase();
  const message = (errorLike.message ?? '').toLowerCase();

  if (name === 'aborterror' || message.includes('aborted') || message.includes('timeout')) {
    return classificationForProfile('timeout');
  }
  if (
    name === 'networkerror' ||
    message.includes('network offline') ||
    message.includes('failed to fetch') ||
    message.includes('networkerror')
  ) {
    return classificationForProfile('networkOffline');
  }
  if (
    name === 'syntaxerror' ||
    message.includes('malformed json') ||
    message.includes('unexpected token')
  ) {
    return classificationForProfile('malformedJson');
  }
  if (message.includes('permission denied') || message.includes('not authorized')) {
    return classificationForProfile('permissionDenied');
  }
  if (message.includes('conflict')) {
    return classificationForProfile('conflict');
  }
  if (message.includes('database is busy') || message.includes('db busy')) {
    return classificationForProfile('dbBusy');
  }
  if (message.includes('lan boundary')) {
    return classificationForProfile('lanBoundaryRejected');
  }
  if (message.includes('cross-site') || message.includes('cross site')) {
    return classificationForProfile('crossSiteRejected');
  }

  return {
    kind: 'unknown',
    code: 'UNKNOWN_FAULT',
    retryable: true,
    showRetry: true,
    cachePolicy: 'clear',
    allowOptimisticCommit: false,
    surfaceHint: 'unknown',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   分类之后，页面/store 需要明确的 cache/stale/retry/乐观回滚动作，禁止乐观发散。
 *
 * Code Logic（这个函数做什么）:
 *   结合 classification.cachePolicy 与 hasCache/optimisticApplied 产出 FaultRecoveryPlan；
 *   optimisticApplied 时 rollbackOptimistic 恒 true；allowOptimisticCommit 恒 false。
 */
export function planFaultRecovery(input: PlanFaultRecoveryInput): FaultRecoveryPlan {
  const { classification, hasCache, optimisticApplied } = input;
  const policy = classification.cachePolicy;

  // none：不写新 cache，也不强制 clear；不把现有数据标 stale 作“权威恢复”
  const keepCache = policy === 'clear' ? false : hasCache;
  const markStale = policy === 'keepStale' ? hasCache : false;
  const clearCache = policy === 'clear';

  return {
    keepCache,
    markStale,
    clearCache,
    showRetry: classification.showRetry,
    rollbackOptimistic: optimisticApplied,
    allowOptimisticCommit: false,
    noOptimisticDivergence: true,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端事件在 disconnect/reconnect 时必须精确回收 listener，且断线输入不入账、重放不重复。
 *
 * Code Logic（这个函数做什么）:
 *   返回内存状态机：listen/unlisten、disconnect（归零并记 baseline）、
 *   reconnect（精确恢复 baseline handlers）、acceptInput/noteInput（幂等 key + connected 门闩）。
 */
export function createTerminalFaultBridge(): TerminalFaultBridge {
  type Handler = (payload: unknown) => void;

  let connected = true;
  let activeHandlers: Handler[] = [];
  let savedHandlers: Handler[] = [];
  let baselineListenerCount = 0;
  let inputCount = 0;
  const seenInputKeys = new Set<string>();

  /**
   * Business Logic（为什么需要这个函数）:
   *   unlisten 必须幂等，且只移除仍在 active 列表中的同一引用。
   *
   * Code Logic（这个函数做什么）:
   *   从 activeHandlers 删除 handler 一次。
   */
  const removeActive = (handler: Handler): void => {
    const index = activeHandlers.indexOf(handler);
    if (index >= 0) {
      activeHandlers.splice(index, 1);
    }
  };

  const bridge: TerminalFaultBridge = {
    get listenerCount(): number {
      return activeHandlers.length;
    },
    get inputCount(): number {
      return inputCount;
    },
    get connected(): boolean {
      return connected;
    },
    get baselineListenerCount(): number {
      return baselineListenerCount;
    },
    listen(handler: Handler): () => void {
      if (connected) {
        activeHandlers.push(handler);
      } else {
        // 断线期间注册记入 saved，reconnect 时一并恢复，避免“半挂”计数
        savedHandlers.push(handler);
        baselineListenerCount = savedHandlers.length;
      }
      let active = true;
      return () => {
        if (!active) {
          return;
        }
        active = false;
        removeActive(handler);
        const savedIndex = savedHandlers.indexOf(handler);
        if (savedIndex >= 0) {
          savedHandlers.splice(savedIndex, 1);
          if (!connected) {
            baselineListenerCount = savedHandlers.length;
          }
        }
      };
    },
    disconnect(): void {
      if (!connected) {
        return;
      }
      savedHandlers = [...activeHandlers];
      baselineListenerCount = savedHandlers.length;
      activeHandlers = [];
      connected = false;
    },
    reconnect(): void {
      if (connected) {
        return;
      }
      activeHandlers = [...savedHandlers];
      connected = true;
      // 不自动重放输入；seenInputKeys 保留以保证 replay 幂等
    },
    acceptInput(key: string, payload?: string): boolean {
      void payload;
      if (!connected) {
        return false;
      }
      if (seenInputKeys.has(key)) {
        return false;
      }
      seenInputKeys.add(key);
      inputCount += 1;
      return true;
    },
    noteInput(key: string, payload?: string): void {
      bridge.acceptInput(key, payload);
    },
    emit(payload: unknown): void {
      if (!connected) {
        return;
      }
      for (const handler of [...activeHandlers]) {
        handler(payload);
      }
    },
  };

  return bridge;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与调用方需要 profile→code 的稳定只读表，与 harness FAULT_PROFILE_CODES 对齐。
 *
 * Code Logic（这个函数做什么）:
 *   返回 PROFILE_CODES 的浅拷贝。
 */
export function faultProfileCodes(): Readonly<Record<FaultProfile, string>> {
  return { ...PROFILE_CODES };
}
