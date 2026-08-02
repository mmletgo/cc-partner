/**
 * CcSwitchCliDependencyCard — Settings 依赖环境页的 cc-switch CLI 状态卡。
 *
 * Business Logic（为什么需要这个组件）:
 *   Provider Manager 依赖 cc-switch CLI 切换 provider。用户需要在「设置 → 依赖环境」看到
 *   CLI 是否已安装/版本/路径，并在缺失时一键安装（macOS 走 brew）。
 *   该卡是自包含的（与 LanFirewallDependencyCard 同构），自行管理状态/调用 API，
 *   因此 SettingsDependenciesPanel 只需放置一行，无需改 Settings controller/props。
 *
 * Code Logic（这个组件做什么）:
 *   mount 时调 provider_manager_status 读取 cli/gui；Recheck 复用同一加载函数；
 *   Install（缺失时）调 provider_manager_install_cli，成功后 force 重拉。
 */

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import { providerManagerApi } from '@/api/providerManager';
import type { ProviderManagerSummary } from '@/lib/types';
import styles from './CcSwitchCliDependencyCard.module.css';

export interface CcSwitchCliDependencyCardProps {
  className?: string;
}

/**
 * Business Logic: 自包含 CLI 依赖卡。
 * Code Logic: 本地 state 管理 loading/summary/error/installing；mount 加载一次。
 */
export function CcSwitchCliDependencyCard(props: CcSwitchCliDependencyCardProps): React.ReactElement {
  const { className } = props;
  const { t } = useTranslation(['providerManager', 'common']);
  const [summary, setSummary] = useState<ProviderManagerSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await providerManagerApi.status();
      setSummary(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('providerManager:cli.loadFailed', { error: String(err) }));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    let cancelled = false;
    void providerManagerApi
      .status()
      .then((next) => {
        if (!cancelled) setSummary(next);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : t('providerManager:cli.loadFailed', { error: String(err) }));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const install = useCallback(async () => {
    setInstalling(true);
    setError(null);
    try {
      await providerManagerApi.installCli();
      const next = await providerManagerApi.status();
      setSummary(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('providerManager:cli.installFailed', { error: String(err) }));
    } finally {
      setInstalling(false);
    }
  }, [t]);

  const cli = summary?.cli ?? null;
  const installed = cli?.available === true;
  const tone: 'success' | 'warn' | 'neutral' = loading
    ? 'neutral'
    : installed
      ? 'success'
      : 'warn';
  const statusKey = loading
    ? 'common:loading'
    : installed
      ? 'providerManager:cli.installed'
      : 'providerManager:cli.notInstalled';
  const mismatch = summary?.gui?.versionMismatch === true;

  return (
    <Card className={[styles.card, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <div>
            <h2 className={styles.title}>{t('providerManager:cli.label')}</h2>
            <p className={styles.subtitle}>{t('providerManager:cli.description')}</p>
          </div>
          <Pill tone={tone} dot>
            {t(statusKey as never)}
          </Pill>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.notice}>{t('providerManager:cli.coexistenceNote')}</p>

        {mismatch ? (
          <StatusMessage tone="warn">
            {t('providerManager:status.versionMismatch', {
              guiVersion: summary?.gui?.version ?? '?',
              cliVersion: summary?.cli.version ?? '?',
            })}
          </StatusMessage>
        ) : null}

        {installed && cli ? (
          <dl className={styles.metaGrid}>
            {cli.version ? (
              <div>
                <dt>{t('providerManager:cli.version')}</dt>
                <dd>{cli.version}</dd>
              </div>
            ) : null}
            {cli.path ? (
              <div className={styles.metaWide}>
                <dt>{t('providerManager:cli.path')}</dt>
                <dd className={styles.mono}>{cli.path}</dd>
              </div>
            ) : null}
          </dl>
        ) : null}

        {error ? <div className={styles.errorBox}>{error}</div> : null}

        <div className={styles.actions}>
          {!installed ? (
            <Button
              variant="secondary"
              size="sm"
              loading={installing}
              disabled={installing}
              onClick={install}
            >
              {installing ? t('providerManager:cli.installing') : t('providerManager:cli.install')}
            </Button>
          ) : null}
          <Button variant="ghost" size="sm" loading={loading} disabled={loading} onClick={load}>
            {t('providerManager:cli.recheck')}
          </Button>
        </div>
      </Card.Body>
    </Card>
  );
}
