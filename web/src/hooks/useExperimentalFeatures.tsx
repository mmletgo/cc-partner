/**
 * useExperimentalFeatures — 内测功能 opt-in 开关的全局投影。
 *
 * Business Logic（为什么需要这个模块）:
 *   充电模式、游戏大厅、网页浏览、项目自动化与云端同步默认关闭；侧栏、工作台、设置页
 *   必须读同一份权威开关，避免入口与调度分叉。
 *
 * Code Logic（这个模块做什么）:
 *   Provider 挂载时经 configApi.get 读取 experimentalFeatures；setFeature 提交整表覆盖后刷新。
 *   无 Provider 时 hook 回落全关（fail-closed），便于单测不包 Provider。
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from 'react';
import { configApi } from '@/api/config';
import { experimentalFeaturesDecoder } from '@/lib/schemas/config';
import {
  DEFAULT_EXPERIMENTAL_FEATURES,
  type ExperimentalFeaturesConfig,
} from '@/lib/types/settings';

export type ExperimentalFeatureId = keyof ExperimentalFeaturesConfig;

export interface ExperimentalFeaturesContextValue {
  features: ExperimentalFeaturesConfig;
  loaded: boolean;
  setFeature: (id: ExperimentalFeatureId, enabled: boolean) => Promise<void>;
  refresh: () => Promise<void>;
}

const ExperimentalFeaturesContext = createContext<ExperimentalFeaturesContextValue | null>(
  null,
);

const FAIL_CLOSED: ExperimentalFeaturesContextValue = {
  features: DEFAULT_EXPERIMENTAL_FEATURES,
  loaded: true,
  setFeature: async () => undefined,
  refresh: async () => undefined,
};

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面走 get_config；手机浏览器没有 Tauri invoke，改读 GET /api/orchestrator/config 的 sibling。
 *
 * Code Logic（这个函数做什么）:
 *   先 configApi.get；失败则 HTTP 解码 experimentalFeatures，缺字段 fail-closed。
 */
async function loadExperimentalFeatures(): Promise<ExperimentalFeaturesConfig> {
  try {
    const cfg = await configApi.get();
    return cfg.experimentalFeatures ?? DEFAULT_EXPERIMENTAL_FEATURES;
  } catch {
    // 桌面有 Tauri internals 时 get_config 失败应 fail-closed，不要再打同源 HTTP
    // （E2E/缺命令时浏览器会把 404 记成 console.error）。
    if (typeof window !== 'undefined') {
      const internals = (window as Window & { __TAURI_INTERNALS__?: { invoke?: unknown } })
        .__TAURI_INTERNALS__;
      if (typeof internals?.invoke === 'function') {
        return DEFAULT_EXPERIMENTAL_FEATURES;
      }
    }
    try {
      const { getJson } = await import('@/api/workbenchHttp');
      const resp = await getJson<{ experimentalFeatures?: unknown }>(
        '/api/orchestrator/config',
      );
      if (resp.experimentalFeatures === undefined) {
        return DEFAULT_EXPERIMENTAL_FEATURES;
      }
      return experimentalFeaturesDecoder.decode(resp.experimentalFeatures);
    } catch {
      return DEFAULT_EXPERIMENTAL_FEATURES;
    }
  }
}

export interface ExperimentalFeaturesProviderProps {
  children: ReactNode;
  /** 测试可注入初始值，跳过 get_config。 */
  initialFeatures?: ExperimentalFeaturesConfig;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   AppShell / Workbench / Settings 需要共享同一份内测开关，设置页切换后侧栏立即隐藏入口。
 *
 * Code Logic（这个组件做什么）:
 *   读 get_config；setFeature 走 update_config 整表覆盖 experimentalFeatures。
 *   进行中的 refresh 不得覆盖用户刚提交的开关。
 */
export function ExperimentalFeaturesProvider({
  children,
  initialFeatures,
}: ExperimentalFeaturesProviderProps): ReactElement {
  const [features, setFeatures] = useState<ExperimentalFeaturesConfig>(
    initialFeatures ?? DEFAULT_EXPERIMENTAL_FEATURES,
  );
  const [loaded, setLoaded] = useState(initialFeatures !== undefined);
  const loadSeqRef = useRef(0);

  const refresh = useCallback(async (): Promise<void> => {
    const seq = loadSeqRef.current + 1;
    loadSeqRef.current = seq;
    try {
      const next = await loadExperimentalFeatures();
      if (loadSeqRef.current !== seq) return;
      setFeatures(next);
    } catch {
      if (loadSeqRef.current !== seq) return;
      setFeatures(DEFAULT_EXPERIMENTAL_FEATURES);
    } finally {
      if (loadSeqRef.current === seq) setLoaded(true);
    }
  }, []);

  useEffect(() => {
    if (initialFeatures !== undefined) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- 无注入 initialFeatures 时挂载拉取 get_config
    void refresh();
  }, [initialFeatures, refresh]);

  const setFeature = useCallback(
    async (id: ExperimentalFeatureId, enabled: boolean): Promise<void> => {
      const next: ExperimentalFeaturesConfig = { ...features, [id]: enabled };
      loadSeqRef.current += 1;
      setFeatures(next);
      try {
        const updated = await configApi.update({ experimentalFeatures: next });
        setFeatures(updated.experimentalFeatures ?? next);
      } catch (error) {
        setFeatures(features);
        throw error;
      }
    },
    [features],
  );

  const value = useMemo<ExperimentalFeaturesContextValue>(
    () => ({ features, loaded, setFeature, refresh }),
    [features, loaded, setFeature, refresh],
  );

  return (
    <ExperimentalFeaturesContext.Provider value={value}>
      {children}
    </ExperimentalFeaturesContext.Provider>
  );
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   入口显隐与设置开关必须读同一 Context；测试未包 Provider 时 fail-closed。
 *
 * Code Logic（这个 hook 做什么）:
 *   有 Context 则返回；否则返回全关 no-op。
 */
// eslint-disable-next-line react-refresh/only-export-components -- Provider 与 hook 同入口，避免每个消费方再加一行 import
export function useExperimentalFeatures(): ExperimentalFeaturesContextValue {
  return useContext(ExperimentalFeaturesContext) ?? FAIL_CLOSED;
}
