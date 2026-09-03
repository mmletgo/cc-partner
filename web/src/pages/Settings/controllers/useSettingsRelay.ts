/**
 * Settings 中转访问（跳板）域 controller hook。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Settings 依赖环境页的「中转访问」卡片需要两类数据（局域网设备列表 + relay 配置）
 *   与三类写动作（添加/移除跳板、切换本机角色）；按 Settings controller/panel 拆分约定，
 *   这些 transport 调用必须留在 controller 侧，卡片保持 pure view。
 *
 * Code Logic（这个 hook 做什么）:
 *   挂载时并行 devicesApi.list + configApi.get（requestSeq 防 stale 写回）；
 *   写动作统一走 configApi.update({ relay }) patch 链路并维护本地 relay 配置快照；
 *   组装 RelayAccessCard 所需的 candidates（直连在线非本机）与 viaDevices（含影子清单）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { devicesApi } from '@/api/devices';
import type { RelayAccessCandidate } from '@/components/domain/RelayAccessCard';
import { buildRelayViaRows, filterRelayViaCandidates } from '@/lib/relayDevices';
import type { RelayViaRow } from '@/lib/relayDevices';
import { DEFAULT_RELAY_CONFIG } from '@/lib/types/settings';
import type { Device, RelayConfig } from '@/lib/types';

export interface UseSettingsRelayResult {
  /** 「添加跳板」候选（本机直连在线非本机设备） */
  candidates: RelayAccessCandidate[];
  /** 已配置跳板行（含影子清单与计数） */
  viaDevices: RelayViaRow[];
  /** 本机是否允许被用作跳板（relay.enabled） */
  allowEnabled: boolean;
  /** 设备/配置加载中 */
  loading: boolean;
  /** 保存中 */
  saving: boolean;
  /** 加载失败文案 */
  loadError: string | null;
  /** 保存失败文案 */
  saveError: string | null;
  /** 保存成功文案 */
  saveSuccess: string | null;
  handleAddViaDevice: (deviceId: string) => void;
  handleRemoveViaDevice: (deviceId: string) => void;
  handleToggleAllow: (enabled: boolean) => void;
  refresh: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   update_config 返回完整 AppConfig；新后端带 relay 段时以其为权威刷新本地快照，
 *   旧后端（并行 Rust 任务未就绪/mock）缺段时沿用本次提交值，避免 UI 回跳。
 *
 * Code Logic（这个函数做什么）:
 *   返回配置中的 relay 段；undefined 时回退传入的 submitted 快照。
 */
function pickRelayConfig(relay: RelayConfig | undefined, submitted: RelayConfig): RelayConfig {
  return relay ?? submitted;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   Settings 依赖环境页需要一个独立于三域 hook 的 relay 资源/表单编排，
 *   保持 useSettingsController thin composer 角色不膨胀。
 *
 * Code Logic（这个 hook 做什么）:
 *   state：devices/selfDeviceId/relayConfig/loading/loadError/saving/saveError/saveSuccess；
 *   effect：挂载加载一次（requestSeq 守卫）；
 *   动作：add/remove via 与 toggle allow 都以最新配置为基线 patch relay 段。
 *
 * @returns RelayAccessCard 所需的完整 props bundle
 */
export function useSettingsRelay(): UseSettingsRelayResult {
  const { t } = useTranslation(['settings']);
  const [devices, setDevices] = useState<Device[]>([]);
  const [selfDeviceId, setSelfDeviceId] = useState('');
  const [relayConfig, setRelayConfig] = useState<RelayConfig>(DEFAULT_RELAY_CONFIG);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState<string | null>(null);
  const loadSeqRef = useRef(0);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入依赖环境页或点击刷新时需要最新设备表（含影子条目）与 relay 配置；
   *   并行请求任一成功也不得被另一次刷新的慢响应覆盖。
   *
   * Code Logic（这个函数做什么）:
   *   抬 requestSeq；并行 devicesApi.list 与 configApi.get，仅最新请求写回 state；
   *   任一失败置 loadError（保留已成功部分数据供卡片渲染）。
   */
  const refresh = useCallback(async (): Promise<void> => {
    const seq = loadSeqRef.current + 1;
    loadSeqRef.current = seq;
    setLoading(true);
    setLoadError(null);
    try {
      const [list, config] = await Promise.all([devicesApi.list(), configApi.get()]);
      if (loadSeqRef.current !== seq) return;
      setDevices(list);
      setSelfDeviceId(config.deviceId);
      setRelayConfig(config.relay ?? DEFAULT_RELAY_CONFIG);
    } catch (error) {
      if (loadSeqRef.current !== seq) return;
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      if (loadSeqRef.current === seq) setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- mount load entry（同步 setLoading 属首次加载门闩，与 InternalClaudeProviderCard 先例一致）
    void refresh();
    return () => {
      loadSeqRef.current += 1;
    };
  }, [refresh]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   添加/移除跳板与开关都是同一条 update_config({ relay }) patch 链路；
   *   需要统一的 saving 门闩、成功/失败反馈与本地快照同步。
   *
   * Code Logic（这个函数做什么）:
   *   以当前 relayConfig 为基线生成 next；configApi.update 成功后按返回值刷新快照
   *   并置 saveSuccess，失败置 saveError（不回滚用户操作意图，下次动作重试）。
   */
  const patchRelay = useCallback(
    async (next: RelayConfig): Promise<void> => {
      setSaving(true);
      setSaveError(null);
      setSaveSuccess(null);
      try {
        const config = await configApi.update({ relay: next });
        setRelayConfig(pickRelayConfig(config.relay, next));
        setSaveSuccess(t('settings:relay.saveSuccess'));
      } catch (error) {
        setSaveError(
          error instanceof Error && error.message
            ? error.message
            : t('settings:relay.saveFailed'),
        );
      } finally {
        setSaving(false);
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在候选下拉选中设备并确认添加，即把该设备写入 relay.viaDeviceIds。
   *
   * Code Logic（这个函数做什么）:
   *   去重后追加 deviceId 并 patch。
   */
  const handleAddViaDevice = useCallback(
    (deviceId: string): void => {
      if (relayConfig.viaDeviceIds.includes(deviceId)) return;
      void patchRelay({
        ...relayConfig,
        viaDeviceIds: [...relayConfig.viaDeviceIds, deviceId],
      });
    },
    [patchRelay, relayConfig],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   跳板设备不再使用或已失联时，用户需要把它从信任列表移除。
   *
   * Code Logic（这个函数做什么）:
   *   过滤目标 deviceId 并 patch。
   */
  const handleRemoveViaDevice = useCallback(
    (deviceId: string): void => {
      void patchRelay({
        ...relayConfig,
        viaDeviceIds: relayConfig.viaDeviceIds.filter((id) => id !== deviceId),
      });
    },
    [patchRelay, relayConfig],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   本机操作者可以拒绝本机被邻居设备当作跳板（B 侧角色开关）。
   *
   * Code Logic（这个函数做什么）:
   *   更新 relay.enabled 并 patch。
   */
  const handleToggleAllow = useCallback(
    (enabled: boolean): void => {
      void patchRelay({ ...relayConfig, enabled });
    },
    [patchRelay, relayConfig],
  );

  const candidates = useMemo<RelayAccessCandidate[]>(
    () =>
      filterRelayViaCandidates(devices, selfDeviceId)
        .filter((device) => !relayConfig.viaDeviceIds.includes(device.id))
        .map((device) => ({ id: device.id, name: device.name, address: device.address })),
    [devices, relayConfig.viaDeviceIds, selfDeviceId],
  );

  const viaDevices = useMemo<RelayViaRow[]>(
    () => buildRelayViaRows(devices, relayConfig.viaDeviceIds),
    [devices, relayConfig.viaDeviceIds],
  );

  return {
    candidates,
    viaDevices,
    allowEnabled: relayConfig.enabled,
    loading,
    saving,
    loadError,
    saveError,
    saveSuccess,
    handleAddViaDevice,
    handleRemoveViaDevice,
    handleToggleAllow,
    refresh,
  };
}
