/**
 * TokenStats - Token 统计页壳层
 *
 * Business Logic（为什么需要这个组件):
 *   把 Token 统计页作为新的系统组顶层入口（路由 `/token-stats`），由 settings /
 *   agent ledger drawer 之外的独立页面承担多维筛选 + 趋势图 + 三维拆分的复合视图。
 *   壳层只负责组合 controller + view，并按 CLAUDE.md §5.8 / 规则 20 在 early return
 *   之前调用所有 hooks；view 是 pure 渲染，不直接 import `@/api/*`。
 *
 * Code Logic（这个组件做什么):
 *   - 调用 `useTokenStatsController()` 获得单一权威 state 与回调。
 *   - 当首屏失败（refreshError === 'error' && 无数据）显示错误重试面板；
 *     保留 stale 数据时让 view 内置的 stale 横幅提示。
 *   - 正常情况下渲染 `<TokenStatsView {...controller} />`。
 */

import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { useTokenStatsController } from './useTokenStatsController';
import { TokenStatsView } from './TokenStatsView';
import styles from './TokenStats.module.css';

export function TokenStats() {
  // **hooks 必须在所有 early return 之前**——CLAUDE.md / AGENTS.md §5.8 规则 20。
  const controller = useTokenStatsController();
  const { t } = useTranslation(['tokenStats', 'common']);

  // 首屏错误 + 无任何缓存数据 → 渲染错误面板
  if (controller.refreshError === 'error' && controller.summary == null) {
    return (
      <div className={styles.page}>
        <div className={styles.container}>
          <div className={styles.errorPanel} role="alert" data-testid="token-stats-error">
            <p className={styles.errorText}>{t('tokenStats:errors.loadFailed')}</p>
            <Button variant="secondary" size="md" onClick={controller.onRefresh}>
              {t('common:action.retry')}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  return <TokenStatsView {...controller} />;
}

TokenStats.displayName = 'TokenStats';
