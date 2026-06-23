/**
 * Prompt 优化页面
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要把原始编程任务需求整理成更适合 Claude Code 执行的结构化 Prompt，
 *   并同时获得中文版本和等价英文版本，方便复制使用。
 *
 * Code Logic（这个页面做什么）:
 *   管理输入、调用后端 `optimize_prompt`、展示两个只读结果框和复制状态；
 *   不保存历史、不入库、不跨设备同步，也不在前端缓存结果。
 */

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { promptOptimizerApi } from '@/api/promptOptimizer';
import { Button, Card } from '@/components/primitives';
import { CopyIcon, EditIcon } from '@/lib/icons';
import styles from './PromptOptimizer.module.css';

type CopiedTarget = 'zh' | 'en' | null;

/**
 * 渲染 Prompt 优化主页面。
 *
 * Business Logic（为什么需要）:
 *   给用户一个一次性转换原始需求的工作台，优化结果只用于当前复制使用。
 *
 * Code Logic（做什么）:
 *   使用 React state 管理输入、加载、错误和复制状态；所有用户可见文案走 i18n；
 *   hooks 全部位于 return 前，避免 React hooks 调用顺序问题。
 */
export function PromptOptimizer() {
  const { t } = useTranslation(['promptOptimizer', 'common']);
  const [input, setInput] = useState('');
  const [optimizedZh, setOptimizedZh] = useState('');
  const [optimizedEn, setOptimizedEn] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copiedTarget, setCopiedTarget] = useState<CopiedTarget>(null);
  const canOptimize = input.trim().length > 0 && !loading;

  useEffect(() => {
    if (!copiedTarget) return undefined;
    const timer = window.setTimeout(() => setCopiedTarget(null), 1500);
    return () => window.clearTimeout(timer);
  }, [copiedTarget]);

  const handleOptimize = useCallback(async () => {
    if (!input.trim()) return;
    setLoading(true);
    setError(null);
    setCopiedTarget(null);
    try {
      const result = await promptOptimizerApi.optimize(input);
      setOptimizedZh(result.optimizedZh);
      setOptimizedEn(result.optimizedEn);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('promptOptimizer:errorFallback'));
    } finally {
      setLoading(false);
    }
  }, [input, t]);

  const handleCopy = useCallback(
    async (target: Exclude<CopiedTarget, null>) => {
      const text = target === 'zh' ? optimizedZh : optimizedEn;
      if (!text) return;
      try {
        await navigator.clipboard.writeText(text);
        setCopiedTarget(target);
        setError(null);
      } catch {
        setError(t('promptOptimizer:copyFailed'));
      }
    },
    [optimizedEn, optimizedZh, t],
  );

  return (
    <div className={styles.page}>
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>{t('promptOptimizer:eyebrow')}</span>
        <h1 className={styles.title}>{t('promptOptimizer:title')}</h1>
        <p className={styles.lead}>{t('promptOptimizer:description')}</p>
      </header>

      <section className={styles.inputSection} aria-labelledby="prompt-optimizer-input-title">
        <div className={styles.sectionHeader}>
          <h2 id="prompt-optimizer-input-title" className={styles.sectionTitle}>
            {t('promptOptimizer:inputTitle')}
          </h2>
          <Button
            variant="primary"
            size="sm"
            icon={<EditIcon />}
            loading={loading}
            disabled={!canOptimize}
            onClick={handleOptimize}
          >
            {loading ? t('promptOptimizer:optimizing') : t('promptOptimizer:optimize')}
          </Button>
        </div>
        <textarea
          className={styles.input}
          value={input}
          onChange={(event) => setInput(event.target.value)}
          placeholder={t('promptOptimizer:inputPlaceholder')}
          aria-label={t('promptOptimizer:inputAriaLabel')}
          disabled={loading}
        />
      </section>

      <div className={styles.statusRow} aria-live="polite">
        {error ? <span className={styles.error}>{error}</span> : null}
      </div>

      <section className={styles.resultGrid} aria-label={t('promptOptimizer:resultsAriaLabel')}>
        <Card className={styles.resultCard}>
          <div className={styles.resultHeader}>
            <h2 className={styles.resultTitle}>{t('promptOptimizer:zhTitle')}</h2>
            <Button
              variant="secondary"
              size="sm"
              icon={<CopyIcon />}
              disabled={!optimizedZh}
              onClick={() => void handleCopy('zh')}
            >
              {copiedTarget === 'zh' ? t('promptOptimizer:copied') : t('common:action.copy')}
            </Button>
          </div>
          <textarea
            className={styles.resultText}
            value={optimizedZh}
            readOnly
            placeholder={t('promptOptimizer:resultPlaceholder')}
            aria-label={t('promptOptimizer:zhAriaLabel')}
          />
        </Card>

        <Card className={styles.resultCard}>
          <div className={styles.resultHeader}>
            <h2 className={styles.resultTitle}>{t('promptOptimizer:enTitle')}</h2>
            <Button
              variant="secondary"
              size="sm"
              icon={<CopyIcon />}
              disabled={!optimizedEn}
              onClick={() => void handleCopy('en')}
            >
              {copiedTarget === 'en' ? t('promptOptimizer:copied') : t('common:action.copy')}
            </Button>
          </div>
          <textarea
            className={styles.resultText}
            value={optimizedEn}
            readOnly
            placeholder={t('promptOptimizer:resultPlaceholder')}
            aria-label={t('promptOptimizer:enAriaLabel')}
          />
        </Card>
      </section>
    </div>
  );
}
