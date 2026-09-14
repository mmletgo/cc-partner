/**
 * ProviderManager 页 — 列出各 agent 已配置的 provider 并切换当前 provider。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户希望在 cc-partner 内直接切换 cc-switch 已配置好的 provider，而无需打开 cc-switch GUI。
 *   读 cc-switch 数据库展示 provider；切换委托 cc-switch CLI（不自行写活配置文件）。
 *   目标既可以是本机，也可以是局域网内其他 cc-partner 设备（顶部设备选择器切换）。
 *
 * Code Logic（这个模块做什么）:
 *   - `ProviderManager`（entry）实例化 controller 并展开为 view props。
 *   - `ProviderManagerView`（pure）只消费 controller 投影，不 import `@/api`。
 *   - `DeviceTargetBar`（pure）渲染「本机 + 在线远端设备」chip 选择器。
 *   - `ProviderCard` / `AppSection` 为 pure 子组件，按 props 渲染。
 */

import type { ReactElement, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { AgentApp, AppProviders, Device, ProviderEntry } from '@/lib/types';
import type { UseProviderManagerControllerResult } from './useProviderManagerController';
import { useProviderManagerController } from './useProviderManagerController';
import styles from './ProviderManager.module.css';

export type ProviderManagerViewProps = UseProviderManagerControllerResult;

/** 单个 provider 卡片（pure）。
 *  使用 Card.Header / Card.Body / Card.Footer 复合子组件，
 *  让 Card 的 padding 机制真正生效，并保证网格内卡片高度对齐、按钮统一贴底。 */
function ProviderCard(props: {
  provider: ProviderEntry;
  switching: boolean;
  onSwitch: () => void;
}): ReactElement {
  const { provider, switching, onSwitch } = props;
  const { t } = useTranslation(['providerManager', 'common']);
  return (
    <Card variant="outlined" padding="sm" className={styles.providerCard}>
      <Card.Body className={styles.providerBody}>
        <div className={styles.providerHead}>
          <span className={styles.providerName} title={provider.name}>{provider.name}</span>
          {provider.category ? (
            <span className={styles.providerCategory}>{provider.category}</span>
          ) : null}
        </div>
      </Card.Body>
      <Card.Footer className={styles.providerFooter}>
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
      </Card.Footer>
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

/** 目标设备选择器（pure）：「本机」+ 在线远端设备 chip 行。
 *
 * Business Logic（为什么需要这个组件）:
 *   Provider 管理现在可作用于局域网内其他 cc-partner 设备，用户需要一眼看到可选目标并切换。
 *
 * Code Logic（这个组件做什么）:
 *   role=group + aria-label 的 chip 行；选中 chip 用 secondary + aria-pressed，
 *   未选中用 ghost；switching 在途时整行禁用，避免切换竞态。键盘原生可达。
 */
function DeviceTargetBar(props: {
  devices: Device[];
  deviceId: string | null;
  switching: boolean;
  onSelect: (deviceId: string | null) => void;
}): ReactElement {
  const { devices, deviceId, switching, onSelect } = props;
  const { t } = useTranslation(['providerManager', 'common']);

  /**
   * Business Logic（为什么需要这个函数）:
   *   经跳板可见的影子设备与直连设备可能重名，需要展示访问链路后缀（如「xxx（经 yyy）」）。
   *
   * Code Logic（这个函数做什么）:
   *   有 viaDeviceName 时用 i18n viaSuffix 模板拼接，否则直接返回设备名。
   */
  const labelFor = (device: Device): string =>
    device.viaDeviceName
      ? t('providerManager:device.viaSuffix', { name: device.name, via: device.viaDeviceName })
      : device.name;

  return (
    <div className={styles.deviceBar} role="group" aria-label={t('providerManager:device.selectorLabel')}>
      <span className={styles.deviceLabel}>{t('providerManager:device.selectorLabel')}</span>
      <Button
        variant={deviceId === null ? 'secondary' : 'ghost'}
        size="sm"
        aria-pressed={deviceId === null}
        disabled={switching}
        onClick={() => {
          onSelect(null);
        }}
      >
        {t('providerManager:device.local')}
      </Button>
      {devices.map((device) => (
        <Button
          key={device.id}
          variant={deviceId === device.id ? 'secondary' : 'ghost'}
          size="sm"
          aria-pressed={deviceId === device.id}
          disabled={switching}
          title={labelFor(device)}
          onClick={() => {
            onSelect(device.id);
          }}
        >
          {labelFor(device)}
        </Button>
      ))}
    </div>
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
    devices,
    deviceId,
    isRemote,
    onSwitch,
    onInstall,
    onRecheck,
    onSelectDevice,
  } = props;
  const { t } = useTranslation(['providerManager', 'common']);

  const switching = switchingKey !== null;
  const cliMissing = summary !== null && !summary.cli.available;
  const dbMissing = summary !== null && !summary.ccSwitchDbPresent;
  const guiMismatch = summary?.gui?.versionMismatch === true;
  const hasApps = (summary?.apps.length ?? 0) > 0;

  // 当前目标设备名：优先取设备列表里的名称，设备刚下线被裁掉时兜底显示 id。
  const targetDeviceName = devices.find((device) => device.id === deviceId)?.name ?? deviceId;

  // 安装 CLI 是本机动作；远端模式下隐藏入口，仅保留远端版 warn 文案。
  const installAction: ReactNode = cliMissing && !isRemote ? (
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
            {isRemote ? (
              <p className={styles.remoteTarget}>
                {t('providerManager:device.remoteTarget', { device: targetDeviceName ?? '' })}
              </p>
            ) : null}
          </div>
          <Button variant="ghost" size="sm" loading={loading} onClick={onRecheck}>
            {t('providerManager:actions.recheck')}
          </Button>
        </header>

        <DeviceTargetBar
          devices={devices}
          deviceId={deviceId}
          switching={switching}
          onSelect={onSelectDevice}
        />

        {error ? (
          <StatusMessage tone="danger" action={
            <Button variant="secondary" size="sm" loading={loading} onClick={onRecheck}>
              {t('providerManager:actions.recheck')}
            </Button>
          }>
            {t(isRemote ? 'providerManager:remoteLoadFailed' : 'providerManager:loadFailed', { error })}
          </StatusMessage>
        ) : null}

        {cliMissing ? (
          <StatusMessage tone="warn" action={installAction}>
            {t(isRemote ? 'providerManager:status.cliMissingRemote' : 'providerManager:status.cliMissing')}
            <span className={styles.hint}>
              {t(isRemote
                ? 'providerManager:status.cliMissingHintRemote'
                : 'providerManager:status.cliMissingHint')}
            </span>
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
