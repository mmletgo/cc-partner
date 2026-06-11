/**
 * 速记本页面
 *
 * Business Logic（为什么需要这个页面）:
 *   用户在日常工作中需要快速记录临时想法、片段文字、待办事项等。
 *   速记本提供一个无需保存的纯临时记事空间，退出应用时自动清空，
 *   降低"忘记保存"的认知负担。
 *
 * Code Logic（这个页面做什么）:
 *   - 纯前端内存状态，不调用后端 API
 *   - 实时字符计数显示
 *   - 复制全部：navigator.clipboard.writeText
 *   - 清空：二次确认 modal 后清空状态
 */

import { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card } from '@/components/primitives';
import { CopyIcon, TrashIcon, XIcon } from '@/lib/icons';
import styles from './Scratchpad.module.css';

export function Scratchpad() {
  const { t } = useTranslation(['scratchpad', 'common']);
  const [text, setText] = useState('');
  const [pendingClear, setPendingClear] = useState(false);

  const charCount = text.length;

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

  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>TOOLS</span>
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
        <span className={styles.charCount}>{t('scratchpad:charCount', { n: charCount })}</span>
      </div>

      {/* 编辑区 */}
      <textarea
        className={styles.editor}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder={t('scratchpad:placeholder')}
        aria-label={t('scratchpad:contentAriaLabel')}
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
