/**
 * Workbench「继续工作」启动摘要状态机。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户进入 `/workbench` 且已有项目但尚未选中时，需要看到可独立失败/刷新的最近项目、
 *   会话、Orchestrator 任务、传输与设备摘要，而不能因为某一 section 失败整页崩溃，
 *   也不能编造空指标。active 项目路径不消费该状态；零项目路径也不请求。
 *
 * Code Logic（这个模块做什么）:
 *   在 wire DTO（`@/lib/types`）之上定义前端 resource 状态、初始 loading 态、
 *   wire→resource 归约，以及整次 invoke 失败时的 stale 保留逻辑。纯函数，无 React / 网络副作用。
 */

import type {
  WorkbenchLaunchDevice,
  WorkbenchLaunchProject,
  WorkbenchLaunchSession,
  WorkbenchLaunchSectionWire,
  WorkbenchLaunchSummaryWire,
  WorkbenchLaunchTask,
  WorkbenchLaunchTransfer,
} from '@/lib/types';

export type {
  WorkbenchLaunchDevice,
  WorkbenchLaunchProject,
  WorkbenchLaunchSession,
  WorkbenchLaunchSectionWire,
  WorkbenchLaunchSummaryWire,
  WorkbenchLaunchTask,
  WorkbenchLaunchTransfer,
};

/**
 * 前端 section 资源态：loading / ready(+stale) / error(+可选 cached)。
 * empty 数组是真实空态，不是假指标。
 */
export type WorkbenchLaunchResource<T> =
  | { kind: 'loading' }
  | { kind: 'ready'; value: T; stale: boolean }
  | { kind: 'error'; message: string; cached?: T };

/** 前端持有的五 section + generatedAt 状态。 */
export interface WorkbenchLaunchSummaryState {
  projects: WorkbenchLaunchResource<WorkbenchLaunchProject[]>;
  sessions: WorkbenchLaunchResource<WorkbenchLaunchSession[]>;
  tasks: WorkbenchLaunchResource<WorkbenchLaunchTask[]>;
  transfers: WorkbenchLaunchResource<WorkbenchLaunchTransfer[]>;
  devices: WorkbenchLaunchResource<WorkbenchLaunchDevice[]>;
  generatedAt: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   进入「有项目未选中」模式时，五个 section 应同时显示 loading，避免残留上一轮数据。
 *
 * Code Logic（这个函数做什么）:
 *   返回全部 section 为 loading、generatedAt 为 null 的初始态。
 */
export function createInitialLaunchSummaryState(): WorkbenchLaunchSummaryState {
  return {
    projects: { kind: 'loading' },
    sessions: { kind: 'loading' },
    tasks: { kind: 'loading' },
    transfers: { kind: 'loading' },
    devices: { kind: 'loading' },
    generatedAt: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从已 ready / error 资源中提取可保留缓存，供失败时 stale 展示。
 *
 * Code Logic（这个函数做什么）:
 *   ready → value；error 带 cached → cached；其余 undefined。
 */
export function getLaunchResourceCachedValue<T>(
  resource: WorkbenchLaunchResource<T>,
): T | undefined {
  if (resource.kind === 'ready') return resource.value;
  if (resource.kind === 'error') return resource.cached;
  return undefined;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   单 section wire 成功写入 ready；失败时保留先前缓存并进入 error，互不影响。
 *
 * Code Logic（这个函数做什么）:
 *   ready wire → {kind:'ready', stale:false}；error wire → {kind:'error', cached?}。
 */
export function mapLaunchSectionWireToResource<T>(
  wire: WorkbenchLaunchSectionWire<T>,
  previous: WorkbenchLaunchResource<T[]>,
): WorkbenchLaunchResource<T[]> {
  if (wire.kind === 'ready') {
    return { kind: 'ready', value: wire.value, stale: false };
  }
  const cached = getLaunchResourceCachedValue(previous);
  return cached === undefined
    ? { kind: 'error', message: wire.message }
    : { kind: 'error', message: wire.message, cached };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   整次 summary 成功返回时，按 section 独立归约；一个 section error 不抹掉其它 ready。
 *
 * Code Logic（这个函数做什么）:
 *   对五个 section 调用 mapLaunchSectionWireToResource，并写入 generatedAt。
 */
export function reduceWorkbenchLaunchResults(
  previous: WorkbenchLaunchSummaryState,
  wire: WorkbenchLaunchSummaryWire,
): WorkbenchLaunchSummaryState {
  return {
    projects: mapLaunchSectionWireToResource(wire.projects, previous.projects),
    sessions: mapLaunchSectionWireToResource(wire.sessions, previous.sessions),
    tasks: mapLaunchSectionWireToResource(wire.tasks, previous.tasks),
    transfers: mapLaunchSectionWireToResource(wire.transfers, previous.transfers),
    devices: mapLaunchSectionWireToResource(wire.devices, previous.devices),
    generatedAt: wire.generatedAt,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   整次 invoke 失败时，已 ready 的 section 应标 stale 保留缓存；尚未成功的 loading
 *   转为 error；已有 error 更新 message 并保留 cached。
 *
 * Code Logic（这个函数做什么）:
 *   逐 section 映射；不改写 generatedAt（仍指向上次成功时间）。
 */
export function markLaunchSummaryStaleOnFailure(
  state: WorkbenchLaunchSummaryState,
  message: string,
): WorkbenchLaunchSummaryState {
  const mark = <T,>(resource: WorkbenchLaunchResource<T>): WorkbenchLaunchResource<T> => {
    if (resource.kind === 'ready') {
      return { kind: 'ready', value: resource.value, stale: true };
    }
    if (resource.kind === 'error') {
      return resource.cached === undefined
        ? { kind: 'error', message }
        : { kind: 'error', message, cached: resource.cached };
    }
    return { kind: 'error', message };
  };

  return {
    projects: mark(state.projects),
    sessions: mark(state.sessions),
    tasks: mark(state.tasks),
    transfers: mark(state.transfers),
    devices: mark(state.devices),
    generatedAt: state.generatedAt,
  };
}
