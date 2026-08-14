/**
 * 记单词闪卡视图（不进路由，由 GameHubDialog 挂载）。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户一次只答一道题；选择即时判、填空提交后由后端比答案。
 *
 * Code Logic（这个组件做什么）:
 *   hooks 全在 early return 前；选择点选项即 submit；输入题等用户点提交。
 */

import { useCallback, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Input, Pill, StatusMessage } from '@/components/primitives';
import { wordgameApi } from '@/api/wordgame';
import type { WordgameCard, WordgameSubmitResult } from '@/lib/types/wordgame';
import styles from './WordGame.module.css';

export interface WordGameProps {
  initialCard: WordgameCard;
  onBack: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   invoke 失败要给用户可读文案，但不能把桌面不可用的内部错误直接贴出来。
 *
 * Code Logic（这个函数做什么）:
 *   Error.message 优先；含 invoke 字样时回落 fallback。
 */
function feedbackMessage(error: unknown, fallback: string): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/invoke|__tauri/i.test(message)) return fallback;
  return message || fallback;
}

/**
 * 渲染一局记单词闪卡。
 */
export function WordGame({ initialCard, onBack }: WordGameProps): ReactNode {
  const { t } = useTranslation(['wordgame']);
  const [card, setCard] = useState<WordgameCard>(initialCard);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<WordgameSubmitResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = useCallback(
    async (answer: string): Promise<void> => {
      const trimmed = answer.trim();
      if (!trimmed || busy) return;
      setBusy(true);
      setError(null);
      try {
        const next = await wordgameApi.submitAnswer(card.lemma, card.questionType, trimmed);
        setResult(next);
      } catch (reason) {
        setError(feedbackMessage(reason, t('wordgame:play.submitError')));
      } finally {
        setBusy(false);
      }
    },
    [busy, card.lemma, card.questionType, t],
  );

  const handleChoice = useCallback(
    (option: string): void => {
      if (result) return;
      void handleSubmit(option);
    },
    [handleSubmit, result],
  );

  const handleInputSubmit = useCallback(
    (event: { preventDefault: () => void }): void => {
      event.preventDefault();
      void handleSubmit(draft);
    },
    [draft, handleSubmit],
  );

  const handleAdvance = useCallback((): void => {
    if (!result?.next) return;
    setCard(result.next);
    setDraft('');
    setResult(null);
    setError(null);
  }, [result]);

  const locked = result !== null;
  const done = Boolean(result && (result.done || !result.next));

  return (
    <div className={styles.play} data-testid="wordgame-play">
      <header className={styles.playHeader}>
        <Button variant="ghost" size="sm" onClick={onBack}>
          {t('wordgame:play.back')}
        </Button>
        <h2 className={styles.playTitle}>{t('wordgame:play.title')}</h2>
        <Pill tone="accent">{t(`wordgame:types.${card.questionType}`)}</Pill>
      </header>
      <p className={styles.lemma} data-testid="wordgame-lemma">
        {card.lemma}
      </p>
      <p className={styles.prompt} data-testid="wordgame-prompt">
        {card.prompt}
      </p>
      {card.kind === 'choice' ? (
        <div className={styles.options} role="group" aria-label={t('wordgame:play.promptLabel')}>
          {card.options.map((option) => (
            <Button
              key={option}
              variant="secondary"
              disabled={busy || locked}
              onClick={() => handleChoice(option)}
            >
              {option}
            </Button>
          ))}
        </div>
      ) : (
        <form className={styles.inputRow} onSubmit={handleInputSubmit}>
          <Input
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={t('wordgame:play.answerLabel')}
            disabled={busy || locked}
            aria-label={t('wordgame:play.answerLabel')}
          />
          <Button type="submit" variant="primary" loading={busy} disabled={locked || !draft.trim()}>
            {t('wordgame:play.submit')}
          </Button>
        </form>
      )}
      {result ? (
        <StatusMessage
          tone={result.correct ? 'success' : 'danger'}
          action={
            done ? undefined : (
              <Button variant="primary" size="sm" onClick={handleAdvance}>
                {t('wordgame:play.next')}
              </Button>
            )
          }
        >
          {result.correct ? t('wordgame:play.correct') : t('wordgame:play.incorrect', { expected: result.expected })}
          {result.familiar ? ` ${t('wordgame:play.familiar')}` : ''}
          {done ? ` ${t('wordgame:play.done')}` : ''}
        </StatusMessage>
      ) : null}
      {error ? (
        <StatusMessage tone="danger">{error}</StatusMessage>
      ) : null}
    </div>
  );
}
