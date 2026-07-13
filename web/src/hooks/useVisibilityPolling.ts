import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * useVisibilityPolling 配置项。
 *
 * Business Logic（为什么需要这个接口）:
 *   各业务页共享同一套可见性轮询语义，需要统一 interval/启停/首刷/可见恢复开关。
 *
 * Code Logic（这个接口做什么）:
 *   描述轮询周期与可选行为开关；缺省时 enabled/runImmediately/refreshOnVisible 均为 true。
 */
export interface UseVisibilityPollingOptions {
  intervalMs: number;
  enabled?: boolean;
  runImmediately?: boolean;
  refreshOnVisible?: boolean;
}

/**
 * useVisibilityPolling 返回值。
 *
 * Business Logic（为什么需要这个接口）:
 *   mutation 成功后需要立即刷新，UI 也需要知道当前是否仍有 in-flight 请求。
 *
 * Code Logic（这个接口做什么）:
 *   暴露 single-flight 的 runNow 与 inFlight 布尔状态。
 */
export interface UseVisibilityPollingResult {
  runNow: () => Promise<void>;
  inFlight: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Transfer/Devices/Health 等页面需要在可见时定时拉取后端权威状态，
 *   后台标签页不得空转网络，恢复可见要立即对齐，且同一 poll 最多一个 in-flight。
 *
 * Code Logic（这个函数做什么）:
 *   用 taskRef 跟踪最新 task（身份变化不重置 interval），用 inFlightPromiseRef 做 single-flight；
 *   仅在 document.visibilityState === 'visible' 时执行 interval tick；
 *   refreshOnVisible 时在 hidden→visible 立即 runNow；卸载后不写 React state；
 *   runNow 不吞错误，interval 路径附加 .catch(() => undefined) 防止 unhandled rejection。
 */
export function useVisibilityPolling(
  task: () => Promise<void>,
  options: UseVisibilityPollingOptions,
): UseVisibilityPollingResult {
  const {
    intervalMs,
    enabled = true,
    runImmediately = true,
    refreshOnVisible = true,
  } = options;

  const taskRef = useRef(task);
  const mountedRef = useRef(true);
  const inFlightPromiseRef = useRef<Promise<void> | null>(null);
  const [inFlight, setInFlight] = useState(false);

  // 保持 taskRef 指向最新 task，避免 interval effect 因 task 身份变化而重置计时器。
  useEffect(() => {
    taskRef.current = task;
  }, [task]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   业务 mutation 成功后需要立即刷新列表，且不能与 interval tick 并发双请求。
   *
   * Code Logic（这个函数做什么）:
   *   若已有 in-flight Promise 则直接返回它；否则执行最新 taskRef，维护 inFlight 状态，
   *   并在 finally 中清理 ref；卸载后不 setState；错误原样向上抛出。
   */
  const runNow = useCallback((): Promise<void> => {
    if (inFlightPromiseRef.current) {
      return inFlightPromiseRef.current;
    }

    const promise = (async () => {
      try {
        await taskRef.current();
      } finally {
        inFlightPromiseRef.current = null;
        if (mountedRef.current) {
          setInFlight(false);
        }
      }
    })();

    inFlightPromiseRef.current = promise;
    if (mountedRef.current) {
      setInFlight(true);
    }
    return promise;
  }, []);

  useEffect(() => {
    if (!enabled) {
      return;
    }

    let intervalId: ReturnType<typeof setInterval> | null = null;

    /**
     * Business Logic（为什么需要这个函数）:
     *   interval 与可见恢复路径需要统一触发 single-flight 刷新，且隐藏页不得发请求。
     *
     * Code Logic（这个函数做什么）:
     *   检查 visibilityState，可见时调用 runNow，并用 catch 吞掉 rejection 避免 unhandled。
     */
    const safeRun = () => {
      if (typeof document !== 'undefined' && document.visibilityState !== 'visible') {
        return;
      }
      void runNow().catch(() => undefined);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   仅在页面可见时保留定时轮询，隐藏时释放 interval 避免后台空转。
     *
     * Code Logic（这个函数做什么）:
     *   可见且未启动时 setInterval；不可见时 clearInterval。
     */
    const startInterval = () => {
      if (intervalId !== null) {
        return;
      }
      intervalId = setInterval(safeRun, intervalMs);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   页面隐藏或 effect 清理时必须停掉定时器。
     *
     * Code Logic（这个函数做什么）:
     *   clearInterval 并重置 intervalId。
     */
    const stopInterval = () => {
      if (intervalId === null) {
        return;
      }
      clearInterval(intervalId);
      intervalId = null;
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   根据当前可见性启停 interval，并在恢复可见时可选立即刷新。
     *
     * Code Logic（这个函数做什么）:
     *   visible：可选 refreshOnVisible 后 startInterval；hidden：stopInterval。
     */
    const handleVisibility = () => {
      if (typeof document === 'undefined') {
        return;
      }
      if (document.visibilityState === 'visible') {
        if (refreshOnVisible) {
          safeRun();
        }
        startInterval();
      } else {
        stopInterval();
      }
    };

    if (runImmediately) {
      safeRun();
    }

    if (typeof document === 'undefined' || document.visibilityState === 'visible') {
      startInterval();
    }

    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', handleVisibility);
    }

    return () => {
      stopInterval();
      if (typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', handleVisibility);
      }
    };
  }, [enabled, intervalMs, runImmediately, refreshOnVisible, runNow]);

  return { runNow, inFlight };
}
