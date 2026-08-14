/**
 * GameHubDialog — 侧栏 footer 打开的游戏大厅 / 记单词 / 插件三态 Dialog。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户要点版本号旁的 game 进大厅，再进记单词或插件游戏；游戏中点遮罩不能退出。
 *
 * Code Logic（这个组件做什么）:
 *   单 Dialog 三态；大厅 Escape/遮罩关闭；游戏态 closeOnBackdrop=false，
 *   Escape/返回回大厅。插件用全应用半透明 surface。遮罩用 backdropVariant=scrim。
 *   hooks 全在 early return 前。
 */

import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import { gamePluginsApi } from '@/api/gamePlugins';
import { wordgameApi } from '@/api/wordgame';
import { useBattery } from '@/hooks/useBattery';
import { useTheme } from '@/hooks/useTheme';
import { gamePluginSpecPrompt } from '@/lib/gamePluginSpecPrompt';
import type { GamePluginSummary } from '@/lib/types/gamePlugin';
import type { WordgameCard, WordgameHubStatus } from '@/lib/types/wordgame';
import { GamePluginPlayer } from '@/pages/GamePluginPlayer';
import { WordGame } from '@/pages/WordGame';
import styles from './GameHubDialog.module.css';

export interface GameHubDialogProps {
  open: boolean;
  onClose: () => void;
}

type HubView = 'hub' | 'play' | 'plugin';

/**
 * Business Logic（为什么需要这个函数）:
 *   预热状态要用人话解释：还差几个、正在生成哪个词、是否堵在失败词。
 *
 * Code Logic（这个函数做什么）:
 *   按 canEnter / preheatStatus 选文案；不拼接后端原文到标题。
 */
function describeReadiness(
  status: WordgameHubStatus,
  t: TFunction<'wordgame'>,
): string | null {
  if (status.canEnter) return null;
  if (status.preheatStatus === 'blocked' && status.preheatLemma) {
    return t('hub.blocked', { lemma: status.preheatLemma });
  }
  if (status.preheatStatus === 'generating' && status.preheatLemma) {
    return t('hub.generating', { lemma: status.preheatLemma });
  }
  if (status.preheatStatus === 'waiting_for_words') {
    return t('hub.waiting');
  }
  return t('hub.notReady', {
    cached: status.cachedUnfamiliarCount,
    required: status.requiredCached,
  });
}

/**
 * 渲染游戏大厅 Dialog。
 */
export function GameHubDialog({ open, onClose }: GameHubDialogProps): ReactNode {
  const { t, i18n } = useTranslation('wordgame');
  const { theme } = useTheme();
  const { snapshot } = useBattery();
  const [view, setView] = useState<HubView>('hub');
  const [status, setStatus] = useState<WordgameHubStatus | null>(null);
  const [card, setCard] = useState<WordgameCard | null>(null);
  const [plugins, setPlugins] = useState<GamePluginSummary[]>([]);
  const [pluginDir, setPluginDir] = useState<string>('');
  const [activePlugin, setActivePlugin] = useState<GamePluginSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [starting, setStarting] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const loadStatus = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const [hub, list] = await Promise.all([
        wordgameApi.getHubStatus(),
        gamePluginsApi.list().catch(() => ({ dir: '', games: [] as GamePluginSummary[] })),
      ]);
      setStatus(hub);
      setPluginDir(list.dir);
      setPlugins(list.games);
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message || t('hub.loadError'));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    if (!open) {
      setView('hub');
      setCard(null);
      setActivePlugin(null);
      setError(null);
      setCopied(false);
      return;
    }
    void loadStatus();
  }, [open, loadStatus]);

  const handleDialogClose = useCallback((): void => {
    if (view === 'play') {
      setView('hub');
      setCard(null);
      void wordgameApi.abandonRound().catch(() => undefined);
      void loadStatus();
      return;
    }
    if (view === 'plugin') {
      setView('hub');
      setActivePlugin(null);
      return;
    }
    onClose();
  }, [loadStatus, onClose, view]);

  const handleEnter = useCallback(async (): Promise<void> => {
    setStarting(true);
    setError(null);
    try {
      const first = await wordgameApi.startRound();
      setCard(first);
      setView('play');
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message || t('play.startError'));
      await loadStatus();
    } finally {
      setStarting(false);
    }
  }, [loadStatus, t]);

  const handleRetry = useCallback(async (): Promise<void> => {
    setRetrying(true);
    setError(null);
    try {
      setStatus(await wordgameApi.retryPreheat());
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message || t('hub.loadError'));
    } finally {
      setRetrying(false);
    }
  }, [t]);

  const handleCopySpec = useCallback(async (): Promise<void> => {
    const lang = i18n.language.startsWith('zh') ? 'zh' : 'en';
    try {
      await navigator.clipboard.writeText(gamePluginSpecPrompt(lang));
      setCopied(true);
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message || t('hub.specCopyError'));
    }
  }, [i18n.language, t]);

  const handleEnterPlugin = useCallback((game: GamePluginSummary): void => {
    setActivePlugin(game);
    setView('plugin');
  }, []);

  const handlePluginCredit = useCallback(
    async (sourceId?: string): Promise<void> => {
      if (!activePlugin) return;
      try {
        await gamePluginsApi.credit(activePlugin.id, sourceId);
      } catch (reason) {
        const message = reason instanceof Error ? reason.message : String(reason);
        setError(message || t('hub.pluginLoadError'));
      }
    },
    [activePlugin, t],
  );

  const pluginReason = (game: GamePluginSummary): string | null => {
    if (game.playable) return null;
    if (game.reason === 'missing_entry') return t('hub.pluginMissingEntry');
    return t('hub.pluginInvalid');
  };

  const readiness = status ? describeReadiness(status, t) : null;
  const title =
    view === 'play' ? t('play.title') : view === 'plugin' ? (activePlugin?.name ?? t('hub.title')) : t('hub.title');
  const specLang = i18n.language.startsWith('zh') ? 'zh' : 'en';

  return (
    <Dialog
      open={open}
      titleId="game-hub-dialog-title"
      onClose={handleDialogClose}
      closeOnBackdrop={view === 'hub'}
      backdropVariant="scrim"
      className={view === 'plugin' ? styles.playDialog : styles.dialog}
    >
      <div className={styles.body} data-testid="game-hub-dialog">
        {view === 'hub' ? (
          <>
            <header className={styles.header}>
              <div>
                <h2 id="game-hub-dialog-title" className={styles.title}>
                  {title}
                </h2>
                <p className={styles.subtitle}>{t('hub.subtitle')}</p>
              </div>
              <Button variant="ghost" size="sm" onClick={onClose}>
                {t('hub.close')}
              </Button>
            </header>
            <article className={styles.gameRow}>
              <div>
                <h3 className={styles.gameTitle}>{t('hub.wordgameTitle')}</h3>
                <p className={styles.gameDesc}>{t('hub.wordgameDesc')}</p>
                {readiness ? <p className={styles.reason}>{readiness}</p> : null}
              </div>
              <Button
                variant="primary"
                disabled={!status?.canEnter || loading}
                loading={starting}
                onClick={() => {
                  void handleEnter();
                }}
              >
                {t('hub.enter')}
              </Button>
            </article>
            {status?.preheatStatus === 'blocked' ? (
              <Button
                variant="secondary"
                size="sm"
                loading={retrying}
                onClick={() => {
                  void handleRetry();
                }}
              >
                {t('hub.retry')}
              </Button>
            ) : null}
            {status?.remoteHint ? (
              <StatusMessage tone="warn">{t('remoteHint')}</StatusMessage>
            ) : null}
            {status?.preheatError && status.preheatStatus === 'blocked' ? (
              <StatusMessage tone="danger">{status.preheatError}</StatusMessage>
            ) : null}
            {plugins.map((game) => (
              <article key={game.id} className={styles.gameRow}>
                <div>
                  <h3 className={styles.gameTitle}>{game.name}</h3>
                  <p className={styles.gameDesc}>{game.description || t('hub.pluginFallbackDesc')}</p>
                  {pluginReason(game) ? <p className={styles.reason}>{pluginReason(game)}</p> : null}
                </div>
                <Button
                  variant="primary"
                  disabled={!game.playable || loading}
                  onClick={() => handleEnterPlugin(game)}
                >
                  {t('hub.enter')}
                </Button>
              </article>
            ))}
            {plugins.length === 0 ? (
              <p className={styles.reason}>
                {t('hub.pluginEmpty', { dir: pluginDir || t('hub.pluginDirUnknown') })}
              </p>
            ) : null}
            <section className={styles.spec} data-testid="game-spec-prompt">
              <div className={styles.specHeader}>
                <h3 className={styles.gameTitle}>{t('hub.specTitle')}</h3>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    void handleCopySpec();
                  }}
                >
                  {copied ? t('hub.specCopied') : t('hub.specCopy')}
                </Button>
              </div>
              <pre className={styles.specBody}>{gamePluginSpecPrompt(specLang)}</pre>
            </section>
            {error ? <StatusMessage tone="danger">{error}</StatusMessage> : null}
          </>
        ) : view === 'plugin' && activePlugin ? (
          <GamePluginPlayer
            game={activePlugin}
            theme={theme}
            batteryMode={snapshot?.mode === 'unlimited' ? 'unlimited' : 'charging'}
            remainingMs={snapshot?.remainingMs ?? 0}
            locale={specLang}
            onBack={handleDialogClose}
            onCredit={handlePluginCredit}
          />
        ) : card ? (
          <WordGame initialCard={card} onBack={handleDialogClose} />
        ) : (
          <StatusMessage tone="warn">{t('play.empty')}</StatusMessage>
        )}
      </div>
    </Dialog>
  );
}
