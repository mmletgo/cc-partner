/**
 * 工作台顶栏标语 hook：owning device SQLite + 本机 localStorage 一次性灌入。
 *
 * Business Logic（为什么需要这个模块）:
 *   标语必须落在 owning device；本机空行才偷看 legacy localStorage。
 *
 * Code Logic（这个模块做什么）:
 *   get/save 走 workbenchApi.banner；离线只读；成功灌入后 clear seed。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { workbenchApi } from '@/api/workbench';
import { clampBannerMarkdown, clearLegacyBannerSeed, peekLegacyBannerSeed } from './workbenchBanner';

export interface UseWorkbenchBannerParams {
  deviceId?: string;
  remoteWriteDisabled?: boolean;
}

export interface UseWorkbenchBannerResult {
  markdown: string;
  persist: (next: string) => Promise<void>;
  readOnly: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   顶栏叶子只关心正文与保存，不该自己拼 invoke。
 *
 * Code Logic（这个函数做什么）:
 *   按 deviceId 加载；本机空行灌入 seed；persist 写 owning device。
 */
export function useWorkbenchBanner(params: UseWorkbenchBannerParams): UseWorkbenchBannerResult {
  const { deviceId, remoteWriteDisabled = false } = params;
  const [markdown, setMarkdown] = useState('');
  const loadedKeyRef = useRef<string | null>(null);

  useEffect(() => {
    const key = deviceId ?? 'local';
    let cancelled = false;
    loadedKeyRef.current = key;
    void workbenchApi.banner
      .get(deviceId)
      .then(async (row) => {
        if (cancelled || loadedKeyRef.current !== key) return;
        if (!deviceId && row.markdown.trim().length === 0) {
          const seed = peekLegacyBannerSeed();
          if (seed.length > 0) {
            const saved = await workbenchApi.banner.save(seed, deviceId);
            if (cancelled || loadedKeyRef.current !== key) return;
            clearLegacyBannerSeed();
            setMarkdown(saved.markdown);
            return;
          }
        }
        setMarkdown(row.markdown);
      })
      .catch(() => {
        if (cancelled || loadedKeyRef.current !== key) return;
        setMarkdown('');
      });
    return () => {
      cancelled = true;
    };
  }, [deviceId]);

  const persist = useCallback(
    async (next: string) => {
      if (remoteWriteDisabled) return;
      const clamped = clampBannerMarkdown(next);
      const saved = await workbenchApi.banner.save(clamped, deviceId);
      setMarkdown(saved.markdown);
    },
    [deviceId, remoteWriteDisabled],
  );

  return {
    markdown,
    persist,
    readOnly: remoteWriteDisabled,
  };
}
