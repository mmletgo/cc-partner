/**
 * Session 运行时长叶子文本 —— 自持 1 Hz 时钟，不驱动 Workbench 根重渲染。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在状态卡看到当前会话运行时长；若在 Workbench 根每秒 setState，会连带整页
 *   controllers / 终端层 1 Hz 重渲染。将时钟隔离到本叶子后，仅文本子树更新。
 *
 * Code Logic（这个组件做什么）:
 *   接收不可变 startedAt/endedAt/running/visible/emptyValue；仅当 running 且所属表面可见
 *   且 document 可见时订阅 1 Hz 外部 store；stopped 时用 endedAt 冻结最终时长；
 *   unmount / 条件失效时退订。格式化复用 formatRuntime。document 可见性与时钟均用
 *   useSyncExternalStore，避免 effect 内同步 setState 触发级联渲染告警。
 */

import { useSyncExternalStore, type ReactElement } from 'react';

import { formatRuntime } from './workbenchPageHelpers';

/**
 * SessionRuntimeText 输入 props。
 *
 * Business Logic: 页面/状态卡只传会话时间与可见性语义，不传已格式化字符串，避免根持有时钟。
 */
export interface SessionRuntimeTextProps {
  /** 会话启动时间 ISO；null 时展示 emptyValue。 */
  startedAt: string | null;
  /** 会话结束时间 ISO；stopped 时用于冻结最终时长。 */
  endedAt: string | null;
  /** 会话是否仍在 running；false 时不启 interval。 */
  running: boolean;
  /** 所属 Workbench 表面（inspector/workspace）是否视为可见；由父组件从既有状态派生。 */
  visible: boolean;
  /** 无 startedAt 或非法时间时的占位文案。 */
  emptyValue: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   SSR/测试环境可能无 document；interval 启停需稳定读取当前 tab 可见性。
 *
 * Code Logic（这个函数做什么）:
 *   document 不存在时视为可见；否则读 visibilityState === 'visible'。
 */
function readDocumentVisible(): boolean {
  if (typeof document === 'undefined') return true;
  return document.visibilityState === 'visible';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   document 可见性变化时需要让叶子组件重新评估是否应 tick。
 *
 * Code Logic（这个函数做什么）:
 *   订阅 visibilitychange，store 变更时通知 React。
 */
function subscribeDocumentVisibility(onStoreChange: () => void): () => void {
  if (typeof document === 'undefined') {
    return () => undefined;
  }
  const handler = (): void => {
    onStoreChange();
  };
  document.addEventListener('visibilitychange', handler);
  return () => {
    document.removeEventListener('visibilitychange', handler);
  };
}

/** 1 Hz 时钟快照：仅在有订阅者时推进，保证 getSnapshot 在无事件时稳定。 */
let secondClockNowMs = Date.now();
const secondClockListeners = new Set<() => void>();
let secondClockTimer: number | null = null;

/**
 * Business Logic（为什么需要这个函数）:
 *   多个 SessionRuntimeText 可能同时可见；共享 1 Hz 时钟避免每实例独立 timer。
 *
 * Code Logic（这个函数做什么）:
 *   首个订阅者启动 window.setInterval(1000)，末个退订时 clear；每次 tick 更新快照并通知。
 *   订阅瞬间对齐 Date.now()，保证 hidden→visible resume 后立刻刷新。
 */
function subscribeSecondClock(onStoreChange: () => void): () => void {
  secondClockListeners.add(onStoreChange);
  secondClockNowMs = Date.now();
  if (secondClockTimer == null && typeof window !== 'undefined') {
    secondClockTimer = window.setInterval(() => {
      secondClockNowMs = Date.now();
      for (const listener of secondClockListeners) {
        listener();
      }
    }, 1000);
  }
  // 订阅后立即通知一次，让已挂载组件读到对齐后的快照
  onStoreChange();
  return () => {
    secondClockListeners.delete(onStoreChange);
    if (secondClockListeners.size === 0 && secondClockTimer != null && typeof window !== 'undefined') {
      window.clearInterval(secondClockTimer);
      secondClockTimer = null;
    }
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   useSyncExternalStore 要求无变更时 getSnapshot 返回稳定值；退订后快照冻结在最后一次 tick。
 *
 * Code Logic（这个函数做什么）:
 *   返回模块级 secondClockNowMs。
 */
function getSecondClockSnapshot(): number {
  return secondClockNowMs;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   不可见/已停止时不应订阅 1 Hz 时钟。
 *
 * Code Logic（这个函数做什么）:
 *   立即返回空 cleanup，不注册 timer。
 */
function subscribeNoop(): () => void {
  return () => undefined;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   状态卡「运行时长」行需要每秒刷新的紧凑文本，但不能拖累 Workbench 根。
 *
 * Code Logic（这个组件做什么）:
 *   documentVisible 与 nowMs 均经 useSyncExternalStore；
 *   shouldTick = running && visible && documentVisible 时才订阅 1 Hz 时钟；
 *   不 tick 时 getSnapshot 仍读最后冻结值（endedAt 优先时无关）；
 *   渲染 formatRuntime 结果到带 data-testid 的 span。
 */
export function SessionRuntimeText(props: SessionRuntimeTextProps): ReactElement {
  const { startedAt, endedAt, running, visible, emptyValue } = props;

  const documentVisible = useSyncExternalStore(
    subscribeDocumentVisibility,
    readDocumentVisible,
    () => true,
  );

  const shouldTick = running && visible && documentVisible;

  const nowMs = useSyncExternalStore(
    shouldTick ? subscribeSecondClock : subscribeNoop,
    getSecondClockSnapshot,
    getSecondClockSnapshot,
  );

  const text = formatRuntime(startedAt, endedAt, nowMs, emptyValue);
  return <span data-testid="session-runtime-text">{text}</span>;
}
