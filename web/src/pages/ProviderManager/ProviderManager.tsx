/**
 * ProviderManager 页 — 列出各 agent 已配置的 provider 并切换当前 provider。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户希望在 cc-partner 内直接切换 cc-switch 已配置好的 provider，而无需打开 cc-switch GUI。
 *   读 cc-switch 数据库展示 provider；切换委托 cc-switch CLI（不自行写活配置文件）。
 *
 * Code Logic（这个模块做什么）:
 *   - `ProviderManager`（entry）实例化 controller 并展开为 view props。
 *   - `ProviderManagerView`（pure）只消费 controller 投影，不 import `@/api`。
 *   - `ProviderCard` / `AppSection` 为 pure 子组件，按 props 渲染。
 */

import type { ReactElement, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { AgentApp, AppProviders, ProviderEntry } from '@/lib/types';
import type { UseProviderManagerControllerResult } from './useProviderManagerController';
import { useProviderManagerController } from './useProviderManagerController';
import styles from './ProviderManager.module.css';

export type ProviderManagerViewProps = UseProviderManagerControllerResult;

/** 单个 provider 卡片（pure）。 */
function ProviderCard(props: {
  provider: ProviderEntry;
  switching: boolean;
  onSwitch: () => void;
}): ReactElement {
  const { provider, switching, onSwitch } = props;
  const { t } = useTranslation(['providerManager', 'common']);
  return (
    <Card variant="outlined" padding="sm" className={styles.providerCard}>
      <div className={styles.providerHead}>
        <span className={styles.providerName}>{provider.name}</span>
        {provider.category ? (
          <span className={styles.providerCategory}>{provider.category}</span>
        ) : null}
      </div>
      <div className={styles.providerAction}>
        {provider.isCurrent ? (
          <Pill tone="success" dot>
            {t('providerManager:status.current')}
          </Pill>
        ) : (
          <Button
            variant="secondary"
            size="sm"
            loading={switching}
            disabled={switching}
            onClick={onSwitch}
          >
            {t('providerManager:actions.switch')}
          </Button>
        )}
      </div>
    </Card>
  );
}

/** 某 agent 的 provider 列表区块（pure）。 */
function AppSection(props: {
  appProviders: AppProviders;
  switchingKey: string | null;
  onSwitch: (app: AgentApp, providerId: string) => void;
}): ReactElement {
  const { appProviders, switchingKey, onSwitch } = props;
  const { t } = useTranslation(['providerManager', 'common']);
  const current = appProviders.providers.find((p) => p.id === appProviders.currentProviderId);
  return (
    <section className={styles.appSection}>
      <header className={styles.appHeader}>
        <h2 className={styles.appTitle}>{t(`providerManager:apps.${appProviders.app}`)}</h2>
        {current ? (
          <span className={styles.currentLine}>
            <span className={styles.currentLabel}>{t('providerManager:status.current')}</span>
            <span className={styles.currentName}>{current.name}</span>
          </span>
        ) : null}
      </header>
      <div className={styles.providerGrid}>
        {appProviders.providers.map((provider) => (
          <ProviderCard
            key={provider.id}
            provider={provider}
            switching={switchingKey === `${appProviders.app}:${provider.id}`}
            onSwitch={() => {
              onSwitch(appProviders.app, provider.id);
            }}
          />
        ))}
      </div>
    </section>
  );
}

/** 纯 view（消费 controller 投影）。 */
export function ProviderManagerView(props: ProviderManagerViewProps): ReactElement {
  const {
    summary,
    loading,
    error,
    switchingKey,
    switchError,
    installing,
    installError,
    onSwitch,
    onInstall,
    onRecheck,
  } = props;
  const { t } = useTranslation(['providerManager', 'common']);

  const cliMissing = summary !== null && !summary.cli.available;
  const dbMissing = summary !== null && !summary.ccSwitchDbPresent;
  const guiMismatch = summary?.gui?.versionMismatch === true;
  const hasApps = (summary?.apps.length ?? 0) > 0;

  const installAction: ReactNode = cliMissing ? (
    <Button variant="secondary" size="sm" loading={installing} disabled={installing} onClick={onInstall}>
      {installing ? t('providerManager:actions.installing') : t('providerManager:actions.install')}
    </Button>
  ) : null;

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div>
            <h1 className={styles.title}>{t('providerManager:title')}</h1>
            <p className={styles.subtitle}>{t('providerManager:subtitle')}</p>
          </div>
          <Button variant="ghost" size="sm" loading={loading} onClick={onRecheck}>
            {t('providerManager:actions.recheck')}
          </Button>
        </header>

        {error ? (
          <StatusMessage tone="danger" action={
            <Button variant="secondary" size="sm" loading={loading} onClick={onRecheck}>
              {t('providerManager:actions.recheck')}
            </Button>
          }>
            {t('providerManager:loadFailed', { error })}
          </StatusMessage>
        ) : null}

        {cliMissing ? (
          <StatusMessage tone="warn" action={installAction}>
            {t('providerManager:status.cliMissing')}
            <span className={styles.hint}>{t('providerManager:status.cliMissingHint')}</span>
          </StatusMessage>
        ) : null}

        {installError ? <StatusMessage tone="danger">{installError}</StatusMessage> : null}

        {dbMissing ? <StatusMessage tone="info">{t('providerManager:status.dbMissing')}</StatusMessage> : null}

        {guiMismatch ? (
          <StatusMessage tone="warn">
            {t('providerManager:status.versionMismatch', {
              guiVersion: summary?.gui?.version ?? '?',
              cliVersion: summary?.cli.version ?? '?',
            })}
          </StatusMessage>
        ) : null}

        {switchError ? (
          <StatusMessage tone="danger">{t('providerManager:status.switchFailed', { error: switchError })}</StatusMessage>
        ) : null}

        {!loading && !error && summary && !hasApps ? (
          <StatusMessage tone="info">{t('providerManager:noProviders')}</StatusMessage>
        ) : null}

        {summary?.apps.map((appProviders) => (
          <AppSection
            key={appProviders.app}
            appProviders={appProviders}
            switchingKey={switchingKey}
            onSwitch={onSwitch}
          />
        ))}
      </div>
    </div>
  );
}

/** 页面入口：实例化 controller 并展开为 view props。 */
export function ProviderManager(): ReactElement {
  const controller = useProviderManagerController();
  return <ProviderManagerView {...controller} />;
}
