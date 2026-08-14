/**
 * 插件游戏全应用播放器。
 *
 * Business Logic（为什么需要这个组件）:
 *     用户点大厅里的插件后，要在半透明遮罩里玩，并把完成事件交给宿主入账。
 *
 * Code Logic（这个组件做什么）:
 *     沙箱 iframe 加载 gameplugin 协议；向游戏推 theme/battery；只接受该 iframe 的 complete/close。
 */

import { useCallback, useEffect, useRef, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import type { GamePluginSummary } from '@/lib/types/gamePlugin';
import styles from './GamePluginPlayer.module.css';

export interface GamePluginPlayerProps {
  game: GamePluginSummary;
  theme: 'light' | 'dark';
  batteryMode: 'charging' | 'unlimited';
  remainingMs: number;
  locale: 'zh' | 'en';
  onBack: () => void;
  onCredit: (sourceId?: string) => Promise<void>;
}

interface GameMessage {
  type?: unknown;
  action?: unknown;
  sourceId?: unknown;
}

/**
 * 拼 iframe src。
 */
function pluginSrc(game: GamePluginSummary): string {
  const entry = game.entry
    .split('/')
    .filter((part) => part.length > 0)
    .map((part) => encodeURIComponent(part))
    .join('/');
  return `gameplugin://localhost/${encodeURIComponent(game.id)}/${entry}`;
}

/**
 * 渲染插件游戏 iframe。
 */
export function GamePluginPlayer({
  game,
  theme,
  batteryMode,
  remainingMs,
  locale,
  onBack,
  onCredit,
}: GamePluginPlayerProps): ReactNode {
  const { t } = useTranslation('wordgame');
  const frameRef = useRef<HTMLIFrameElement | null>(null);

  const postHost = useCallback((): void => {
    const win = frameRef.current?.contentWindow;
    if (!win) return;
    win.postMessage(
      {
        type: 'cc-partner:host',
        version: 1,
        theme,
        batteryMode,
        remainingMs,
        locale,
      },
      '*',
    );
  }, [batteryMode, locale, remainingMs, theme]);

  useEffect(() => {
    postHost();
  }, [postHost]);

  useEffect(() => {
    const onMessage = (event: MessageEvent<GameMessage>): void => {
      if (event.source !== frameRef.current?.contentWindow) return;
      const data = event.data;
      if (!data || data.type !== 'cc-partner:game') return;
      if (data.action === 'close') {
        onBack();
        return;
      }
      if (data.action === 'complete') {
        const sourceId = typeof data.sourceId === 'string' ? data.sourceId : undefined;
        void onCredit(sourceId);
      }
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, [onBack, onCredit]);

  return (
    <div className={styles.player} data-testid="game-plugin-player">
      <header className={styles.toolbar}>
        <Button variant="ghost" size="sm" onClick={onBack}>
          {t('play.back')}
        </Button>
        <h2 id="game-hub-dialog-title" className={styles.title}>
          {game.name}
        </h2>
      </header>
      <iframe
        ref={frameRef}
        className={styles.frame}
        title={game.name}
        src={pluginSrc(game)}
        sandbox="allow-scripts allow-pointer-lock"
        onLoad={postHost}
      />
    </div>
  );
}
