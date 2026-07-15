/**
 * Session 运行时长叶子文本 —— 自持 1 Hz 时钟，不驱动 Workbench 根重渲染。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在状态卡看到当前会话运行时长；若在 Workbench 根每秒 setState，会连带整页
 *   controllers / 终端层 1 Hz 重渲染。将时钟隔离到本叶子后，仅文本子树更新。
 *
 * Code Logic（这个组件做什么）:
 *   接收不可变 startedAt/endedAt/running/visible/emptyValue；仅当 running 且所属表面可见
 *   且 document 可见时 setInterval(1000) 更新 nowMs；stopped 时用 endedAt 冻结最终时长；
 *   unmount / 条件失效时 clearInterval。格式化复用 formatRuntime。
 */

import { useEffect, useState, type ReactElement } from 'react';

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
 * Business Logic（为什么需要这个组件）:
 *   状态卡「运行时长」行需要每秒刷新的紧凑文本，但不能拖累 Workbench 根。
 *
 * Code Logic（这个组件做什么）:
 *   局部 nowMs + documentVisible state；shouldTick = running && visible && documentVisible
 *   时启动 1s interval；渲染 formatRuntime 结果到带 data-testid 的 span。
 */
export function SessionRuntimeText(props: SessionRuntimeTextProps): ReactElement {
  const { startedAt, endedAt, running, visible, emptyValue } = props;
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  const [documentVisible, setDocumentVisible] = useState<boolean>(() => readDocumentVisible());

  useEffect(() => {
    /**
     * Business Logic（为什么需要这个 effect）:
     *   用户切到其它浏览器 tab 时不应继续 1 Hz 计时唤醒，回到本 tab 后再续。
     *
     * Code Logic（这个 effect 做什么）:
     *   监听 visibilitychange，把 document.visibilityState 写入 documentVisible。
     */
    const onVisibilityChange = (): void => {
      setDocumentVisible(readDocumentVisible());
    };
    document.addEventListener('visibilitychange', onVisibilityChange);
    setDocumentVisible(readDocumentVisible());
    return () => {
      document.removeEventListener('visibilitychange', onVisibilityChange);
    };
  }, []);

  const shouldTick = running && visible && documentVisible;

  useEffect(() => {
    /**
     * Business Logic（为什么需要这个 effect）:
     *   仅在会话运行且表面/文档可见时需要实时时长；否则冻结展示值省 CPU。
     *
     * Code Logic（这个 effect 做什么）:
     *   shouldTick 时立刻对齐 Date.now 并 setInterval(1000)；否则不建 timer；cleanup clearInterval。
     */
    if (!shouldTick) return undefined;
    setNowMs(Date.now());
    const timer = window.setInterval(() => {
      setNowMs(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  }, [shouldTick]);

  const text = formatRuntime(startedAt, endedAt, nowMs, emptyValue);
  return <span data-testid="session-runtime-text">{text}</span>;
}
