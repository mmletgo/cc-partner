/**
 * Settings 移动推送中转 controller。
 *
 * Business Logic（为什么需要这个 hook）:
 *   依赖环境页需要读写推送中转 URL/凭据；卡片保持 pure view。
 *
 * Code Logic（这个 hook 做什么）:
 *   挂载时 get_mobile_push_config；保存走 update_mobile_push_config。
 */
import { useCallback, useEffect, useState } from 'react';
import { mobilePushApi } from '@/api/mobilePush';

export interface UseSettingsMobilePushResult {
  relayUrl: string;
  relayToken: string;
  tokenConfigured: boolean;
  loading: boolean;
  saving: boolean;
  loadError: string | null;
  saveError: string | null;
  saveSuccess: string | null;
  setRelayUrl: (value: string) => void;
  setRelayToken: (value: string) => void;
  save: () => Promise<void>;
  refresh: () => Promise<void>;
}

export function useSettingsMobilePush(): UseSettingsMobilePushResult {
  const [relayUrl, setRelayUrl] = useState('');
  const [relayToken, setRelayToken] = useState('');
  const [tokenConfigured, setTokenConfigured] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const cfg = await mobilePushApi.get();
      setRelayUrl(cfg.relayUrl);
      setTokenConfigured(cfg.relayTokenConfigured);
      setRelayToken('');
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- 初始网络加载复用手动刷新入口，与 useSettingsRelay 的挂载合同一致。
    void refresh();
  }, [refresh]);

  const save = useCallback(async () => {
    setSaving(true);
    setSaveError(null);
    setSaveSuccess(null);
    try {
      const cfg = await mobilePushApi.update(relayUrl, relayToken.trim() || undefined);
      setRelayUrl(cfg.relayUrl);
      setTokenConfigured(cfg.relayTokenConfigured);
      setRelayToken('');
      setSaveSuccess('saved');
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }, [relayUrl, relayToken]);

  return {
    relayUrl,
    relayToken,
    tokenConfigured,
    loading,
    saving,
    loadError,
    saveError,
    saveSuccess,
    setRelayUrl,
    setRelayToken,
    save,
    refresh,
  };
}
