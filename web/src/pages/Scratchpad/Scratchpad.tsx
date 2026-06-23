/**
 * 速记本页面
 *
 * Business Logic（为什么需要这个页面）:
 *   用户在日常工作中需要快速记录临时想法、片段文字、待办事项等。
 *   速记本会自动保存到本机，关闭软件后再次打开仍能继续编辑，
 *   降低"忘记保存"和意外丢失内容的认知负担。
 *
 * Code Logic（这个页面做什么）:
 *   - 从 Rust/SQLite 初始化单例内容，并在内容变化后 debounce 自动保存
 *   - 实时字符计数显示
 *   - 复制全部：navigator.clipboard.writeText
 *   - 清空：二次确认 modal 后写入空内容
 *   - 局域网同步：调用 scratchpadApi.syncLan 后刷新内容
 *   - GitHub 同步：调用 configApi.triggerCloudSync 后刷新内容
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { scratchpadApi } from '@/api/scratchpad';
import { Button, Card } from '@/components/primitives';
import { CopyIcon, SyncIcon, TrashIcon, UploadIcon, XIcon } from '@/lib/icons';
import styles from './Scratchpad.module.css';

const AUTOSAVE_DELAY_MS = 500;

/**
 * Business Logic（为什么需要）:
 *   用户需要一个可随手记录、自动保留内容的本机速记空间。
 *
 * Code Logic（做什么）:
 *   管理速记文本、加载/保存/同步状态、字符计数、复制和清空确认；
 *   所有持久化与同步都通过 Tauri invoke 交给 Rust 后端。
 */
export function Scratchpad() {
  const { t } = useTranslation(['scratchpad', 'common']);
  const [text, setText] = useState('');
  const [pendingClear, setPendingClear] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [lanSyncing, setLanSyncing] = useState(false);
  const [cloudSyncing, setCloudSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const loadedRef = useRef(false);
  const skipNextSaveRef = useRef(false);
  const saveTimerRef = useRef<number | null>(null);

  const charCount = text.length;

  const applyServerContent = useCallback((content: string) => {
    setText((current) => {
      if (current !== content) {
        skipNextSaveRef.current = true;
      }
      return content;
    });
    loadedRef.current = true;
  }, []);

  const loadScratchpad = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const scratchpad = await scratchpadApi.get();
      applyServerContent(scratchpad.content);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('scratchpad:loadFailed'));
    } finally {
      setLoading(false);
    }
  }, [applyServerContent, t]);

  useEffect(() => {
    const id = window.setTimeout(() => {
      void loadScratchpad();
    }, 0);
    return () => window.clearTimeout(id);
  }, [loadScratchpad]);

  useEffect(() => {
    if (!loadedRef.current) return undefined;
    if (skipNextSaveRef.current) {
      skipNextSaveRef.current = false;
      return undefined;
    }
    if (saveTimerRef.current !== null) {
      window.clearTimeout(saveTimerRef.current);
    }

    setSaving(true);
    setStatus(null);
    saveTimerRef.current = window.setTimeout(() => {
      void scratchpadApi
        .update(text)
        .then((scratchpad) => {
          setStatus(t('scratchpad:savedAt', { time: new Date(scratchpad.updatedAt).toLocaleTimeString() }));
        })
        .catch((err) => {
          setError(err instanceof Error ? err.message : t('scratchpad:saveFailed'));
        })
        .finally(() => {
          setSaving(false);
          saveTimerRef.current = null;
        });
    }, AUTOSAVE_DELAY_MS);

    return () => {
      if (saveTimerRef.current !== null) {
        window.clearTimeout(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
  }, [text, t]);

  const savePendingNow = useCallback(async () => {
    if (!loadedRef.current || saveTimerRef.current === null) return;
    window.clearTimeout(saveTimerRef.current);
    saveTimerRef.current = null;
    setSaving(true);
    try {
      const scratchpad = await scratchpadApi.update(text);
      setStatus(
        t('scratchpad:savedAt', {
          time: new Date(scratchpad.updatedAt).toLocaleTimeString(),
        }),
      );
    } finally {
      setSaving(false);
    }
  }, [text, t]);

  const handleCopyAll = useCallback(async () => {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // 静默失败
    }
  }, [text]);

  const handleClearRequest = useCallback(() => {
    if (!text) return;
    setPendingClear(true);
  }, [text]);

  const confirmClear = useCallback(() => {
    setText('');
    setPendingClear(false);
  }, []);

  const cancelClear = useCallback(() => {
    setPendingClear(false);
  }, []);

  const refreshAfterSync = useCallback(async () => {
    const scratchpad = await scratchpadApi.get();
    applyServerContent(scratchpad.content);
  }, [applyServerContent]);

  const handleLanSync = useCallback(async () => {
    setLanSyncing(true);
    setError(null);
    setStatus(null);
    try {
      await savePendingNow();
      const result = await scratchpadApi.syncLan();
      await refreshAfterSync();
      setStatus(t('scratchpad:lanSyncDone', { count: result.synced }));
    } catch (err) {
      setError(err instanceof Error ? err.message : t('scratchpad:lanSyncFailed'));
    } finally {
      setLanSyncing(false);
    }
  }, [refreshAfterSync, savePendingNow, t]);

  const handleCloudSync = useCallback(async () => {
    setCloudSyncing(true);
    setError(null);
    setStatus(null);
    try {
      await savePendingNow();
      const result = await configApi.triggerCloudSync();
      await refreshAfterSync();
      setStatus(result.ok ? t('scratchpad:cloudSyncDone', { note: result.note }) : result.note);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('scratchpad:cloudSyncFailed'));
    } finally {
      setCloudSyncing(false);
    }
  }, [refreshAfterSync, savePendingNow, t]);

  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>{t('scratchpad:eyebrow')}</span>
        <h1 className={styles.title}>{t('scratchpad:title')}</h1>
        <p className={styles.lead}>{t('scratchpad:desc')}</p>
      </header>

      {/* 工具栏 */}
      <div className={styles.toolbar}>
        <Button variant="primary" size="sm" icon={<CopyIcon />} onClick={handleCopyAll}>
          {t('scratchpad:copyAll')}
        </Button>
        <Button variant="secondary" size="sm" icon={<TrashIcon />} onClick={handleClearRequest}>
          {t('scratchpad:clear')}
        </Button>
        <Button variant="secondary" size="sm" icon={<SyncIcon />} loading={lanSyncing} onClick={handleLanSync}>
          {lanSyncing ? t('scratchpad:lanSyncing') : t('scratchpad:syncLan')}
        </Button>
        <Button variant="secondary" size="sm" icon={<UploadIcon />} loading={cloudSyncing} onClick={handleCloudSync}>
          {cloudSyncing ? t('scratchpad:cloudSyncing') : t('scratchpad:syncCloud')}
        </Button>
        <span className={styles.charCount}>{t('scratchpad:charCount', { n: charCount })}</span>
      </div>

      <div className={styles.statusRow} aria-live="polite">
        {loading ? <span>{t('scratchpad:loading')}</span> : null}
        {!loading && saving ? <span>{t('scratchpad:saving')}</span> : null}
        {!loading && !saving && status ? <span>{status}</span> : null}
        {error ? <span className={styles.error}>{error}</span> : null}
      </div>

      {/* 编辑区 */}
      <textarea
        className={styles.editor}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder={t('scratchpad:placeholder')}
        aria-label={t('scratchpad:contentAriaLabel')}
        disabled={loading}
      />

      {/* 清空确认弹层 */}
      {pendingClear ? (
        <div className={styles.modalMask} role="dialog" aria-modal="true" aria-labelledby="clear-title">
          <Card variant="elevated" className={styles.modal}>
            <h3 id="clear-title" className={styles.modalTitle}>
              {t('scratchpad:clearConfirmTitle')}
            </h3>
            <p className={styles.modalText}>{t('scratchpad:clearConfirmText')}</p>
            <div className={styles.modalActions}>
              <Button variant="secondary" size="sm" icon={<XIcon />} onClick={cancelClear}>
                {t('common:action.cancel')}
              </Button>
              <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={confirmClear}>
                {t('scratchpad:clear')}
              </Button>
            </div>
          </Card>
        </div>
      ) : null}
    </div>
  );
}
