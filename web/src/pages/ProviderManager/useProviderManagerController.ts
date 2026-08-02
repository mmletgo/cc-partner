/**
 * useProviderManagerController — Provider Manager 页 controller。
 *
 * Business Logic（为什么需要这个 hook）:
 *   页面需要加载 cc-switch 整体状态、切换 provider、安装 CLI 并维护 stale/error 态；
 *   按 controller/view 拆分约定，所有 `@/api` 调用集中在本 hook，view 只消费投影。
 *
 * Code Logic（这个 hook 做什么）:
 *   - `useVisibilityPolling` 周期性拉取 `provider_manager_status`（可见时刷新，隐藏停轮询）。
 *   - `onSwitch` 调 `provider_manager_switch`，成功后用返回的 `AppProviders` 原地替换该 app。
 *   - `onInstall` 调 `provider_manager_install_cli`，成功后 force 重拉状态。
 *   - hooks 全部在返回之前（项目规则 20）。
 */

import { useCallback, useState } from 'react';
import { providerManagerApi } from '@/api/providerManager';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type { AgentApp, AppProviders, ProviderManagerSummary } from '@/lib/types';

/** 切换在途标识：`${app}:${providerId}`。 */
function switchKey(app: AgentApp, providerId: string): string {
  return `${app}:${providerId}`;
}

/** controller 返回值（view props 契约）。 */
export interface UseProviderManagerControllerResult {
  summary: ProviderManagerSummary | null;
  loading: boolean;
  error: string | null;
  switchingKey: string | null;
  switchError: string | null;
  installing: boolean;
  installError: string | null;
  onSwitch: (app: AgentApp, providerId: string) => Promise<void>;
  onInstall: () => Promise<void>;
  onRecheck: () => Promise<void>;
}

/**
 * Business Logic: 加载状态 + 切换 + 安装的单一编排入口。
 * Code Logic: useVisibilityPolling 驱动后台刷新；mutation 后 force 刷新或原地替换。
 */
export function useProviderManagerController(): UseProviderManagerControllerResult {
  const [summary, setSummary] = useState<ProviderManagerSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [switchingKey, setSwitchingKey] = useState<string | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await providerManagerApi.status();
      setSummary(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const { runNow } = useVisibilityPolling(load, { intervalMs: 30_000 });

  const onRecheck = useCallback(async () => {
    await runNow({ force: true });
  }, [runNow]);

  const onSwitch = useCallback(
    async (app: AgentApp, providerId: string) => {
      const key = switchKey(app, providerId);
      setSwitchingKey(key);
      setSwitchError(null);
      try {
        const updated: AppProviders = await providerManagerApi.switch(app, providerId);
        setSummary((prev: ProviderManagerSummary | null) => {
          if (!prev) return prev;
          const apps = prev.apps.map((a: AppProviders) => (a.app === app ? updated : a));
          return { ...prev, apps };
        });
      } catch (err) {
        setSwitchError(err instanceof Error ? err.message : String(err));
      } finally {
        setSwitchingKey(null);
      }
    },
    [],
  );

  const onInstall = useCallback(async () => {
    setInstalling(true);
    setInstallError(null);
    try {
      await providerManagerApi.installCli();
      // 安装后强制重拉，捕获新版本/路径。
      await runNow({ force: true });
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
    } finally {
      setInstalling(false);
    }
  }, [runNow]);

  return {
    summary,
    loading,
    error,
    switchingKey,
    switchError,
    installing,
    installError,
    onSwitch,
    onInstall,
    onRecheck,
  };
}
