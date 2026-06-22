/**
 * CLAUDE.md 编辑页
 *
 * Business Logic（为什么需要这个页面）:
 *   用户希望在 Claude Partner 内直接编辑 user 级全局指令文件（~/.claude/CLAUDE.md），
 *   避免每次手动开编辑器。编辑后保存即可写回磁盘，并能一键同步到局域网内其他设备，
 *   让多台机器共享同一份全局指令。
 *
 * Code Logic（这个页面做什么）:
 *   - 进页面调 get_claude_md 载入内容与元数据
 *   - textarea 实时编辑，"未保存"标记对比 text 与 savedText
 *   - 保存按钮调 update_claude_md 写回（内容未变时跳过）
 *   - 同步按钮调 trigger_sync 后重新拉取远端最新内容
 *   - 操作反馈用本地 toast state（setTimeout 自动清除）
 *   - hooks 全部无条件声明在渲染之前（项目规则 20）
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { ClaudeMdIcon, SyncIcon } from '@/lib/icons';
import { claudeMdApi } from '@/api/claudeMd';
import styles from './ClaudeMd.module.css';

export function ClaudeMd() {
  const { t } = useTranslation(['claudeMd', 'common']);
  const [text, setText] = useState('');
  const [savedText, setSavedText] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  // toast 自动清除的定时器引用，避免重复提示叠加
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** 设置一条操作反馈，3s 后自动清除（覆盖上一次未清除的提示） */
  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 3000);
  }, []);

  /** 进页面载入当前 CLAUDE.md 内容与元数据 */
  const load = useCallback(async () => {
    setLoading(true);
    try {
      const dto = await claudeMdApi.get();
      setText(dto.content);
      setSavedText(dto.content);
    } catch (err) {
      showToast(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [showToast]);

  useEffect(() => {
    // 挂载时拉取 CLAUDE.md：fetch 后 setState 是合法的 mount-load 模式，
    // set-state-in-effect 规则对此误报，局部豁免。
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load();
  }, [load]);

  // 卸载时清掉可能挂着的 toast 定时器，避免 setState on unmounted
  useEffect(() => {
    return () => {
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
  }, []);

  /** 保存：内容未变直接跳过，否则写回后端并刷新 savedText 基线 */
  const handleSave = useCallback(async () => {
    if (text === savedText) return;
    setSaving(true);
    try {
      const dto = await claudeMdApi.update(text);
      setSavedText(dto.content);
      setText(dto.content);
      showToast(t('claudeMd:saved'));
    } catch (err) {
      showToast(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }, [text, savedText, t, showToast]);

  /** 同步：触发 P2P 同步后重新拉取远端最新内容覆盖本地 */
  const handleSync = useCallback(async () => {
    setSyncing(true);
    try {
      await claudeMdApi.sync();
      const dto = await claudeMdApi.get();
      setText(dto.content);
      setSavedText(dto.content);
      showToast(t('claudeMd:synced'));
    } catch (err) {
      showToast(err instanceof Error ? err.message : String(err));
    } finally {
      setSyncing(false);
    }
  }, [t, showToast]);

  const dirty = text !== savedText;

  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>{t('claudeMd:eyebrow')}</span>
        <h1 className={styles.title}>{t('claudeMd:title')}</h1>
        <p className={styles.lead}>{t('claudeMd:desc')}</p>
      </header>

      {/* 工具栏 */}
      <div className={styles.toolbar}>
        <Button
          variant="primary"
          size="sm"
          icon={<ClaudeMdIcon />}
          onClick={handleSave}
          disabled={loading || saving || !dirty}
        >
          {saving ? t('claudeMd:saving') : t('claudeMd:save')}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          icon={<SyncIcon />}
          onClick={handleSync}
          disabled={loading || syncing}
        >
          {syncing ? t('claudeMd:syncing') : t('claudeMd:sync')}
        </Button>
        {dirty ? <span className={styles.unsaved}>{t('claudeMd:unsaved')}</span> : null}
        <span className={styles.charCount}>{t('claudeMd:charCount', { n: text.length })}</span>
      </div>

      {/* 编辑区 */}
      <textarea
        className={styles.editor}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder={t('claudeMd:placeholder')}
        disabled={loading}
        aria-label={t('claudeMd:title')}
      />

      {/* 操作反馈 */}
      {toast ? <div className={styles.toast}>{toast}</div> : null}
    </div>
  );
}
