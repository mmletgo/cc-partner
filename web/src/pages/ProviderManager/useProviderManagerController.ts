/**
 * useProviderManagerController — Provider Manager 页 controller。
 *
 * Business Logic（为什么需要这个 hook）:
 *   页面需要加载 cc-switch 整体状态、切换 provider、安装 CLI 并维护 stale/error 态；
 *   还要支持把 summary/switch 的作用目标切换到局域网内其他 cc-partner 设备；
 *   按 controller/view 拆分约定，所有 `@/api` 调用集中在本 hook，view 只消费投影。
 *
 * Code Logic（这个 hook 做什么）:
 *   - `useVisibilityPolling` 周期性拉取 `provider_manager_status`（按 deviceId 分流本机/远端）
 *     与 `list_devices`（过滤 online，供目标设备选择器）。
 *   - `onSwitch` 调 `provider_manager_switch`（携带当前 deviceId），成功后用返回的
 *     `AppProviders` 原地替换该 app。
 *   - `onInstall` 调 `provider_manager_install_cli`（携带当前 deviceId：本机安装 / 对端安装），
 *     成功后 force 重拉状态；返回 `ok=false` 且带 message（如非 macOS 平台的人工指引，
 *     或 brew 失败原因）时把 message（有 url 附上）写入 installError 展示——本机与远端同语义。
 *   - `onSelectDevice` 同步更新 deviceId（state + ref），并 force 重拉；
 *     switching 在途时忽略，避免竞态。
 *   - deviceId 存 ref 一份：load 身份稳定，选择设备后立即触发的刷新也能读到最新目标。
 *   - hooks 全部在返回之前（项目规则 20）。
 */

import { useCallback, useRef, useState } from 'react';
import { devicesApi } from '@/api/devices';
import { providerManagerApi } from '@/api/providerManager';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type { AgentApp, AppProviders, Device, ProviderManagerSummary } from '@/lib/types';

/** 切换在途标识：`${app}:${providerId}`。 */
function switchKey(app: AgentApp, providerId: string): string {
  return `${app}:${providerId}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   安装结果 `ok=false` 时（非 macOS 平台返回人工指引、或 brew 失败原因），后端 message
 *   常附带官方安装链接；用户需要看到完整指引才能继续，本机与远端同语义。
 *
 * Code Logic（这个函数做什么）:
 *   有 url 时拼成 `message（url）`，否则原样返回 message。
 */
function composeManualGuidance(message: string, url: string | null): string {
  return url ? `${message}（${url}）` : message;
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
  /** 在线远端设备（供目标设备选择器渲染；不含本机）。 */
  devices: Device[];
  /** 当前目标设备 id；null = 本机。 */
  deviceId: string | null;
  /** 当前是否作用于远端设备（deviceId 非空）。 */
  isRemote: boolean;
  onSwitch: (app: AgentApp, providerId: string) => Promise<void>;
  /** 安装 cc-switch CLI：按当前 deviceId 分流本机安装 / 对端安装。 */
  onInstall: () => Promise<void>;
  onRecheck: () => Promise<void>;
  /** 选择目标设备（null = 本机）；switching 在途时忽略。 */
  onSelectDevice: (deviceId: string | null) => void;
}

/**
 * Business Logic: 加载状态 + 切换 + 安装 + 目标设备选择的单一编排入口。
 * Code Logic: useVisibilityPolling 驱动后台刷新（status + devices）；mutation 后 force 刷新或原地替换。
 */
export function useProviderManagerController(): UseProviderManagerControllerResult {
  const [summary, setSummary] = useState<ProviderManagerSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [switchingKey, setSwitchingKey] = useState<string | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [deviceId, setDeviceId] = useState<string | null>(null);

  // deviceId 的同步镜像：让 load/onSwitch 身份稳定（不随选择变化），
  // 且 onSelectDevice 后立即触发的刷新能读到最新目标（不等 effect 更新 taskRef）。
  const deviceIdRef = useRef<string | null>(null);

  /**
   * Business Logic: 用户切换目标设备后，summary/switch 都要落到该设备；
   * 设备离线/对端不支持时后端返回中文错误，透传给现有 error 展示，选择保留。
   *
   * Code Logic: deviceIdRef 非空 → status(deviceId)；否则 status()。
   * 设备列表拉取失败静默保留旧列表（不打断 summary 主流程）。
   */
  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const target = deviceIdRef.current;
      const next = await providerManagerApi.status(target);
      setSummary(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Business Logic: 设备选择器需要「本机 + 在线远端设备」；
   * mDNS capabilities 有 220 字节截断不可靠，不按能力过滤，
   * 对端是否支持 provider 管理由后端调用时校验并返回明确错误。
   *
   * Code Logic: devicesApi.list() → 过滤 status === 'online' → setDevices。
   */
  const loadDevices = useCallback(async () => {
    try {
      const all = await devicesApi.list();
      setDevices(all.filter((device) => device.status === 'online'));
    } catch {
      // 设备列表失败不影响 summary；保留上一次列表供选择器降级展示。
    }
  }, []);

  const { runNow } = useVisibilityPolling(
    useCallback(async () => {
      await Promise.all([load(), loadDevices()]);
    }, [load, loadDevices]),
    { intervalMs: 30_000 },
  );

  const onRecheck = useCallback(async () => {
    await runNow({ force: true });
  }, [runNow]);

  const onSwitch = useCallback(
    async (app: AgentApp, providerId: string) => {
      const key = switchKey(app, providerId);
      setSwitchingKey(key);
      setSwitchError(null);
      try {
        const updated: AppProviders = await providerManagerApi.switch(app, providerId, deviceIdRef.current);
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

  /**
   * Business Logic: 安装按当前目标设备分流（deviceIdRef 非空 = 对端安装）；安装后 force 重拉
   *   捕获新版本/路径。返回 `ok=false` 且带 message（非 macOS 平台的人工指引、brew 失败原因）
   *   不是 invoke 异常，必须显式把 message（有 url 附上）写入 installError 展示，
   *   本机与远端同语义。
   *
   * Code Logic: providerManagerApi.installCli(deviceIdRef.current) → ok/message/url 判定 →
   *   composeManualGuidance 组装展示文案；异常路径照旧透传 err.message。
   */
  const onInstall = useCallback(async () => {
    setInstalling(true);
    setInstallError(null);
    try {
      const result = await providerManagerApi.installCli(deviceIdRef.current);
      // 安装后强制重拉，捕获新版本/路径（manual 指引时重拉无害，状态不变）。
      await runNow({ force: true });
      if (!result.ok && result.message) {
        setInstallError(composeManualGuidance(result.message, result.url));
      }
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
    } finally {
      setInstalling(false);
    }
  }, [runNow]);

  /**
   * Business Logic: 用户点选目标设备 chip 后要立即看到该设备的 provider；
   * switching 在途时忽略选择，避免「切换请求已按旧设备发出、状态却被新设备刷新覆盖」的竞态。
   *
   * Code Logic: 同步写 deviceIdRef + setDeviceId，清空上次 switchError，
   * 再 runNow({ force: true }) 立即重拉（in-flight 时排队补跑一轮）。
   */
  const onSelectDevice = useCallback(
    (id: string | null) => {
      if (switchingKey !== null) {
        return;
      }
      if (deviceIdRef.current === id) {
        return;
      }
      deviceIdRef.current = id;
      setDeviceId(id);
      setSwitchError(null);
      void runNow({ force: true });
    },
    [switchingKey, runNow],
  );

  return {
    summary,
    loading,
    error,
    switchingKey,
    switchError,
    installing,
    installError,
    devices,
    deviceId,
    isRemote: deviceId !== null,
    onSwitch,
    onInstall,
    onRecheck,
    onSelectDevice,
  };
}
