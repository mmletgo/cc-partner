/**
 * CLAUDE.md 编辑页
 *
 * Business Logic（为什么需要这个页面）:
 *   用户希望在 cc-partner 内直接编辑 user 级全局指令文件（~/.claude/CLAUDE.md），
 *   避免每次手动开编辑器。编辑后保存即可写回磁盘，并能一键推送到局域网设备和 GitHub 云端，
 *   让多台机器共享同一份全局指令。
 *
 * Code Logic（这个页面做什么）:
 *   - 进页面调 get_claude_md 载入内容与元数据
 *   - textarea 实时编辑，"未保存"标记对比 text 与 savedText
 *   - 保存/推送使用 saveAttempt 合同：递增 editVersion，submit 捕获 attempt；
 *     success 更新 baseline，仅当 version 未变且 draft 仍等于 snapshot 时才回填
 *   - 推送按钮调 push_claude_md，把本机当前内容分发到局域网设备和 GitHub 云端
 *   - 操作反馈用 StatusMessage（success=role=status / danger=role=alert），定时自动清除
 *   - busy 按钮保持稳定 accessible name（loading 不改 children 文案）
 *   - hooks 全部无条件声明在渲染之前（项目规则 20）
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, StatusMessage, type StatusMessageTone } from '@/components/primitives';
import { ClaudeMdIcon, SyncIcon } from '@/lib/icons';
import { claudeMdApi } from '@/api/claudeMd';
import {
  createSaveAttempt,
  resolveSaveFailure,
  resolveSaveSuccess,
} from '@/lib/asyncState/saveAttempt';
import styles from './ClaudeMd.module.css';

/** 本地 toast：消息 + tone，驱动 StatusMessage */
type ClaudeMdToast = {
  message: string;
  tone: StatusMessageTone;
};

export function ClaudeMd() {
  const { t } = useTranslation(['claudeMd', 'common']);
  const [text, setText] = useState('');
  const [savedText, setSavedText] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [pushing, setPushing] = useState(false);
  const [toast, setToast] = useState<ClaudeMdToast | null>(null);

  // toast 自动清除的定时器引用，避免重复提示叠加
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** 用户每次编辑递增；submit 捕获 version 以判定响应期间是否有新输入 */
  const editVersionRef = useRef(0);
  /** 每次 save/push 提交递增；旧 seq 的 success/error 不改当前态 */
  const requestSeqRef = useRef(0);
  /** 最新 draft / baseline 的同步快照，供 await 后读取，避免闭包陈旧；仅在事件/async 路径写入 */
  const textRef = useRef('');
  const savedTextRef = useRef('');

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户编辑 CLAUDE.md 时需要即时更新草稿并标记版本，保存响应才能识别并发输入。
   *
   * Code Logic（这个函数做什么）:
   *   写入 text/textRef，并递增 editVersion。
   */
  const handleTextChange = useCallback((value: string) => {
    textRef.current = value;
    editVersionRef.current += 1;
    setText(value);
  }, []);

  /**
   * 设置一条操作反馈，3s 后自动清除（覆盖上一次未清除的提示）
   *
   * Business Logic（为什么需要这个函数）:
   *   保存/推送结果需短暂可读提示，且读屏按 tone 选择 status/alert。
   *
   * Code Logic（这个函数做什么）:
   *   写入 {message,tone}，重置 3s 定时器后清空。
   */
  const showToast = useCallback((msg: string, tone: StatusMessageTone = 'info') => {
    setToast({ message: msg, tone });
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 3000);
  }, []);

  /** 进页面载入当前 CLAUDE.md 内容与元数据 */
  const load = useCallback(async () => {
    setLoading(true);
    try {
      const dto = await claudeMdApi.get();
      textRef.current = dto.content;
      savedTextRef.current = dto.content;
      setText(dto.content);
      setSavedText(dto.content);
    } catch (err) {
      showToast(err instanceof Error ? err.message : String(err), 'danger');
    } finally {
      setLoading(false);
    }
  }, [showToast]);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect,setState 在 await 后异步执行 */
  useEffect(() => {
    // 挂载时拉取 CLAUDE.md：fetch 后 setState 是合法的 mount-load 模式，
    // set-state-in-effect 规则对此误报，局部豁免。
    void load();
  }, [load]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // 卸载时清掉可能挂着的 toast 定时器，避免 setState on unmounted
  useEffect(() => {
    return () => {
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   保存必须写回磁盘，且不得用响应覆盖保存期间的新输入。
   *
   * Code Logic（这个函数做什么）:
   *   捕获 SaveAttempt → update_claude_md → resolveSaveSuccess/Failure；
   *   仅 applied 时更新 baseline/draft 与 toast。
   */
  const handleSave = useCallback(async () => {
    if (textRef.current === savedTextRef.current) return;
    const attempt = createSaveAttempt(
      ++requestSeqRef.current,
      textRef.current,
      editVersionRef.current,
    );
    setSaving(true);
    try {
      const dto = await claudeMdApi.update(attempt.submittedSnapshot);
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: requestSeqRef.current,
        currentDraft: textRef.current,
        currentEditVersion: editVersionRef.current,
        serverValue: dto.content,
        currentBaseline: savedTextRef.current,
      });
      if (!resolution.applied) return;
      savedTextRef.current = resolution.baseline;
      textRef.current = resolution.draft;
      setSavedText(resolution.baseline);
      setText(resolution.draft);
      showToast(t('claudeMd:saved'), 'success');
    } catch (err) {
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: requestSeqRef.current,
        currentDraft: textRef.current,
        currentBaseline: savedTextRef.current,
      });
      if (!failure.applied) return;
      showToast(err instanceof Error ? err.message : String(err), 'danger');
    } finally {
      if (attempt.requestSeq === requestSeqRef.current) {
        setSaving(false);
      }
    }
  }, [t, showToast]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   推送会把提交瞬间内容分发到局域网/云端；推送期间的新编辑仍需保留。
   *
   * Code Logic（这个函数做什么）:
   *   捕获 SaveAttempt → push_claude_md → 以 submittedSnapshot 作为成功 baseline
   *   走 resolveSaveSuccess；失败走 resolveSaveFailure 保留 draft。
   */
  const handlePush = useCallback(async () => {
    const attempt = createSaveAttempt(
      ++requestSeqRef.current,
      textRef.current,
      editVersionRef.current,
    );
    setPushing(true);
    try {
      const result = await claudeMdApi.push(attempt.submittedSnapshot);
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: requestSeqRef.current,
        currentDraft: textRef.current,
        currentEditVersion: editVersionRef.current,
        serverValue: attempt.submittedSnapshot,
        currentBaseline: savedTextRef.current,
      });
      if (!resolution.applied) return;
      savedTextRef.current = resolution.baseline;
      textRef.current = resolution.draft;
      setSavedText(resolution.baseline);
      setText(resolution.draft);
      showToast(result.note || t('claudeMd:pushed'), 'success');
    } catch (err) {
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: requestSeqRef.current,
        currentDraft: textRef.current,
        currentBaseline: savedTextRef.current,
      });
      if (!failure.applied) return;
      showToast(err instanceof Error ? err.message : String(err), 'danger');
    } finally {
      if (attempt.requestSeq === requestSeqRef.current) {
        setPushing(false);
      }
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
          disabled={loading || saving || pushing || !dirty}
          loading={saving}
          aria-busy={saving || undefined}
        >
          {t('claudeMd:save')}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          icon={<SyncIcon />}
          onClick={handlePush}
          disabled={loading || saving || pushing}
          loading={pushing}
          aria-busy={pushing || undefined}
        >
          {t('claudeMd:push')}
        </Button>
        {dirty ? <span className={styles.unsaved}>{t('claudeMd:unsaved')}</span> : null}
        <span className={styles.charCount}>{t('claudeMd:charCount', { n: text.length })}</span>
      </div>

      {/* 编辑区 */}
      <textarea
        className={styles.editor}
        value={text}
        onChange={(e) => handleTextChange(e.target.value)}
        placeholder={t('claudeMd:placeholder')}
        disabled={loading}
        aria-label={t('claudeMd:title')}
      />

      {/* 操作反馈：success=status / danger=alert */}
      {toast ? (
        <StatusMessage tone={toast.tone} className={styles.toast}>
          {toast.message}
        </StatusMessage>
      ) : null}
    </div>
  );
}
