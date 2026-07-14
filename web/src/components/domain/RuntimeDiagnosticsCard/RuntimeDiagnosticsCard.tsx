/**
 * 运行状态诊断卡片
 *
 * Business Logic（为什么需要这个组件）:
 *   Settings 依赖环境 tab 需要展示 sidecar owner 诊断，并支持刷新/打开日志/复制脱敏摘要。
 *
 * Code Logic（这个组件做什么）:
 *   挂载时拉取 get_runtime_diagnostics；渲染 counts/phases；复制前扫描敏感键。
 */

import { useCallback, useEffect, useState, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import {
  findForbiddenDiagnosticsKeys,
  formatDiagnosticsForCopy,
  runtimeDiagnosticsApi,
  type SanitizedRuntimeDiagnostics,
} from '@/api/runtimeDiagnostics';
import styles from './RuntimeDiagnosticsCard.module.css';

/**
 * 运行诊断卡片
 *
 * Business Logic（为什么需要这个组件）:
 *   依赖环境页需要独立运行状态分区，不把编排塞进 Settings controller。
 *
 * Code Logic（这个组件做什么）:
 *   useState/useEffect/useCallback 置顶；加载失败展示 retry；动作按钮刷新/日志/复制。
 *
 * @returns 诊断 Card
 */
export function RuntimeDiagnosticsCard(): ReactElement {
  const { t } = useTranslation(['settings', 'common']);
  const [diagnostics, setDiagnostics] = useState<SanitizedRuntimeDiagnostics | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copyFeedback, setCopyFeedback] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户进入依赖环境页或点刷新时需要最新 owner 诊断。
   *
   * Code Logic（这个函数做什么）:
   *   调 runtimeDiagnosticsApi.get；成功写 state，失败写 error。
   *
   * @param isRefresh 是否为用户主动刷新（控制 refreshing）
   */
  const load = useCallback(async (isRefresh = false) => {
    if (isRefresh) setRefreshing(true);
    else setLoading(true);
    setError(null);
    try {
      const next = await runtimeDiagnosticsApi.get();
      setDiagnostics(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings:runtime.loadFailed'));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, [t]);

  // 挂载首次加载：在 promise then/finally 中 setState，避免 effect 体内同步 setState 触发 lint。
  useEffect(() => {
    let cancelled = false;
    void runtimeDiagnosticsApi
      .get()
      .then((next) => {
        if (cancelled) return;
        setDiagnostics(next);
        setError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : t('settings:runtime.loadFailed'));
      })
      .finally(() => {
        if (cancelled) return;
        setLoading(false);
        setRefreshing(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要一键复制脱敏摘要用于反馈，且不得含 token/content。
   *
   * Code Logic（这个函数做什么）:
   *   format JSON → 敏感键扫描 → navigator.clipboard.writeText。
   */
  const handleCopy = useCallback(async () => {
    if (!diagnostics) return;
    const text = formatDiagnosticsForCopy(diagnostics);
    const forbidden = findForbiddenDiagnosticsKeys(text);
    if (forbidden.length > 0) {
      setCopyFeedback(t('settings:runtime.copyBlocked'));
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      setCopyFeedback(t('settings:runtime.copied'));
    } catch {
      setCopyFeedback(t('settings:runtime.copyFailed'));
    }
  }, [diagnostics, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开日志目录便于用户查看 backend.log。
   *
   * Code Logic（这个函数做什么）:
   *   调 open_backend_log_dir；失败写入 error。
   */
  const handleOpenLogs = useCallback(async () => {
    try {
      await runtimeDiagnosticsApi.openLogDir();
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings:runtime.openLogsFailed'));
    }
  }, [t]);

  return (
    <Card variant="flat" padding="md" data-testid="runtime-diagnostics-card">
      <Card.Header>
        <div className={styles.headerRow}>
          <h2 className={styles.title}>{t('settings:runtime.title')}</h2>
          {diagnostics ? (
            <Pill tone="neutral" dot>
              {t('settings:runtime.generationLabel', { generation: diagnostics.generation })}
            </Pill>
          ) : null}
        </div>
      </Card.Header>
      <Card.Body padding="md">
        {loading ? (
          <p className={styles.helper}>{t('settings:runtime.loading')}</p>
        ) : error && !diagnostics ? (
          <div className={styles.stack}>
            <p className={styles.helper} role="alert">
              {t('settings:runtime.loadFailedDetail', { error })}
            </p>
            <Button variant="secondary" size="sm" onClick={() => void load(true)} loading={refreshing}>
              {t('settings:runtime.refresh')}
            </Button>
          </div>
        ) : diagnostics ? (
          <div className={styles.stack}>
            {error ? (
              <p className={styles.helper} role="alert">
                {t('settings:runtime.loadFailedDetail', { error })}
              </p>
            ) : null}
            <dl className={styles.metaGrid} data-testid="runtime-diagnostics-meta">
              <div>
                <dt>{t('settings:runtime.owner')}</dt>
                <dd data-testid="runtime-owner">{diagnostics.ownerInstanceId}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.startedAt')}</dt>
                <dd>{diagnostics.startedAt}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.cloudSyncPhase')}</dt>
                <dd>{diagnostics.cloudSyncPhase}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.terminalSessions')}</dt>
                <dd>{diagnostics.terminalSessionCount}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.bridges')}</dt>
                <dd data-testid="runtime-bridge-count">{diagnostics.bridgeCount}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.orchestratorTick')}</dt>
                <dd>{diagnostics.orchestrator.latestTickAt ?? t('settings:runtime.emptyValue')}</dd>
              </div>
              <div>
                <dt>{t('settings:runtime.orchestratorError')}</dt>
                <dd>
                  {diagnostics.orchestrator.latestErrorClass ?? t('settings:runtime.emptyValue')}
                </dd>
              </div>
              <div>
                <dt>{t('settings:runtime.configFingerprint')}</dt>
                <dd className={styles.mono}>{diagnostics.configFingerprint}</dd>
              </div>
            </dl>
            {diagnostics.bridges.length > 0 ? (
              <ul className={styles.bridgeList} data-testid="runtime-bridge-list">
                {diagnostics.bridges.map((bridge, index) => (
                  <li key={`${bridge.phase}-${index}`}>
                    {t('settings:runtime.bridgeItem', {
                      phase: bridge.phase,
                      attempt: bridge.attempt,
                      error: bridge.lastErrorClass ?? t('settings:runtime.emptyValue'),
                    })}
                  </li>
                ))}
              </ul>
            ) : null}
            <div className={styles.actions}>
              <Button
                variant="secondary"
                size="sm"
                onClick={() => void load(true)}
                loading={refreshing}
                aria-busy={refreshing}
              >
                {t('settings:runtime.refresh')}
              </Button>
              <Button variant="secondary" size="sm" onClick={() => void handleOpenLogs()}>
                {t('settings:runtime.openLogs')}
              </Button>
              <Button
                variant="primary"
                size="sm"
                onClick={() => void handleCopy()}
                data-testid="runtime-copy-diagnostics"
              >
                {t('settings:runtime.copy')}
              </Button>
            </div>
            {copyFeedback ? (
              <p className={styles.helper} data-testid="runtime-copy-feedback">
                {copyFeedback}
              </p>
            ) : null}
          </div>
        ) : null}
      </Card.Body>
    </Card>
  );
}

RuntimeDiagnosticsCard.displayName = 'RuntimeDiagnosticsCard';
