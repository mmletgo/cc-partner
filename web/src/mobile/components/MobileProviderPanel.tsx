/**
 * MobileProviderPanel（移动端 Provider 切换面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 `/mobile` 无法走 Tauri invoke，需要经 HTTP 切换 cc-switch 已配置的 provider。
 *   操作的是「提供 /mobile 服务的后端设备」上的 provider，与移动端终端/文件/git 同一设备。
 *   CLI 缺失时只读提示（不在手机上触发远端安装）；不支持的后端版本优雅降级。
 *
 * Code Logic（这个组件做什么）:
 *   自包含：先 GET /api/health 探测 capability `provider-manager.v1` → 不支持显示 unsupported；
 *   支持 则 GET summary 加载各 agent provider 列表，POST switch 切换当前 provider。
 *   复用 providerManager 类型/decoder/i18n；hooks 全部在渲染之前（无 early return，条件渲染）。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill, StatusMessage } from '@/components/primitives';
import { getJson, postJson } from '@/api/workbenchHttp';
import {
  appProvidersDecoder,
  providerManagerSummaryDecoder,
} from '@/lib/schemas/providerManager';
import type {
  AgentApp,
  AppProviders,
  ProviderEntry,
  ProviderManagerSummary,
} from '@/lib/types/providerManager';
import styles from '../MobileWorkbench.module.css';

/** P2P capability token，与后端 `CAPABILITY_PROVIDER_MANAGER_V1` 一致。 */
const PROVIDER_MANAGER_CAPABILITY_V1 = 'provider-manager.v1';
const HEALTH_PATH = '/api/health';
const SUMMARY_PATH = '/api/provider-manager/summary';
const SWITCH_PATH = '/api/provider-manager/switch';

/** health 响应中与能力探测相关的字段（snake_case，对齐 P2P health）。 */
interface HealthResponse {
  protocol_version?: number;
  capabilities?: string[];
}

type ProviderLoadState = 'loading' | 'unsupported' | 'ready' | 'error';

type ProviderSummaryLoadResult =
  | { kind: 'unsupported' }
  | { kind: 'ready'; summary: ProviderManagerSummary };

/** 把 unknown reason 规整为可展示字符串。 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首次加载与用户重试必须执行完全一致的 capability 探测与 summary 读取。
 *
 * Code Logic（这个函数做什么）:
 *   纯异步 transport 编排，不触碰 React state；调用方在 Promise 回调中提交结果。
 */
async function fetchProviderSummary(): Promise<ProviderSummaryLoadResult> {
  const health = await getJson<HealthResponse>(HEALTH_PATH);
  const capabilities = health.capabilities ?? [];
  if (
    (health.protocol_version ?? 0) < 1 ||
    !capabilities.includes(PROVIDER_MANAGER_CAPABILITY_V1)
  ) {
    return { kind: 'unsupported' };
  }
  const summary = await getJson<ProviderManagerSummary>(SUMMARY_PATH, {
    decoder: providerManagerSummaryDecoder,
  });
  return { kind: 'ready', summary };
}

/**
 * Business Logic: 移动端 provider 切换的列表项（pure）。
 * Code Logic: 当前 provider 用 success Pill 标记，其余渲染切换按钮（CLI 缺失或切换中禁用）。
 */
function ProviderRow(props: {
  provider: ProviderEntry;
  app: AgentApp;
  switching: boolean;
  disabled: boolean;
  onSwitch: (app: AgentApp, providerId: string) => void;
}): ReactElement {
  const { provider, app, switching, disabled, onSwitch } = props;
  const { t } = useTranslation(['providerManager']);
  return (
    <div className={styles.themeRow}>
      <div className={styles.themeRowText}>
        <p className={styles.themeRowTitle}>{provider.name}</p>
        {provider.category ? (
          <p className={styles.themeRowMeta}>{provider.category}</p>
        ) : null}
      </div>
      {provider.isCurrent ? (
        <Pill tone="success" dot>
          {t('providerManager:status.current')}
        </Pill>
      ) : (
        <Button
          variant="secondary"
          size="sm"
          loading={switching}
          disabled={switching || disabled}
          onClick={() => {
            onSwitch(app, provider.id);
          }}
        >
          {t('providerManager:actions.switch')}
        </Button>
      )}
    </div>
  );
}

/**
 * Business Logic: 某 agent 的 provider 列表区块（pure）。
 * Code Logic: 标题用 apps.<app> 文案；当前 provider 名作副标题；逐行渲染 ProviderRow。
 */
function AppSection(props: {
  appProviders: AppProviders;
  switchingKey: string | null;
  disabled: boolean;
  onSwitch: (app: AgentApp, providerId: string) => void;
}): ReactElement {
  const { appProviders, switchingKey, disabled, onSwitch } = props;
  const { t } = useTranslation(['providerManager']);
  const current = appProviders.providers.find(
    (p) => p.id === appProviders.currentProviderId,
  );
  return (
    <section className={styles.settingsSection}>
      <h2 className={styles.settingsSectionTitle}>
        {t(`providerManager:apps.${appProviders.app}`)}
      </h2>
      {current ? (
        <p className={styles.themeRowMeta}>
          {t('providerManager:status.current')}：{current.name}
        </p>
      ) : null}
      {appProviders.providers.map((provider) => (
        <ProviderRow
          key={provider.id}
          provider={provider}
          app={appProviders.app}
          switching={switchingKey === `${appProviders.app}:${provider.id}`}
          disabled={disabled}
          onSwitch={onSwitch}
        />
      ))}
    </section>
  );
}

/**
 * Business Logic: 移动端 provider 切换面板入口（自包含）。
 * Code Logic: mount 时 health 探测 capability → summary 加载 → switch 切换；失败展示错误与重试。
 */
export function MobileProviderPanel(): ReactElement {
  const { t } = useTranslation(['workbench', 'providerManager', 'common']);
  const [loadState, setLoadState] = useState<ProviderLoadState>('loading');
  const [summary, setSummary] = useState<ProviderManagerSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [switchingKey, setSwitchingKey] = useState<string | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const loadSeqRef = useRef(0);

  const load = useCallback(async (): Promise<void> => {
    const seq = ++loadSeqRef.current;
    setLoadState('loading');
    setError(null);
    setSwitchError(null);
    try {
      const result = await fetchProviderSummary();
      if (seq !== loadSeqRef.current) return;
      if (result.kind === 'unsupported') {
        setSummary(null);
        setLoadState('unsupported');
        return;
      }
      setSummary(result.summary);
      setLoadState('ready');
    } catch (reason) {
      if (seq !== loadSeqRef.current) return;
      setError(getErrorMessage(reason));
      setLoadState('error');
    }
  }, []);

  useEffect(() => {
    let active = true;
    const seq = ++loadSeqRef.current;
    void fetchProviderSummary()
      .then((result) => {
        if (!active || seq !== loadSeqRef.current) return;
        if (result.kind === 'unsupported') {
          setSummary(null);
          setLoadState('unsupported');
          return;
        }
        setSummary(result.summary);
        setLoadState('ready');
      })
      .catch((reason: unknown) => {
        if (!active || seq !== loadSeqRef.current) return;
        setError(getErrorMessage(reason));
        setLoadState('error');
      });
    return () => {
      active = false;
    };
  }, []);

  const handleSwitch = useCallback(
    async (app: AgentApp, providerId: string): Promise<void> => {
      const key = `${app}:${providerId}`;
      setSwitchingKey(key);
      setSwitchError(null);
      try {
        const updated = await postJson<AppProviders>(
          SWITCH_PATH,
          { app, providerId },
          { policy: { kind: 'mutation' }, decoder: appProvidersDecoder },
        );
        setSummary((prev) =>
          prev
            ? { ...prev, apps: prev.apps.map((a) => (a.app === updated.app ? updated : a)) }
            : prev,
        );
      } catch (reason) {
        setSwitchError(t('providerManager:status.switchFailed', { error: getErrorMessage(reason) }));
      } finally {
        setSwitchingKey(null);
      }
    },
    [t],
  );

  const titleId = 'mobile-provider-title';
  const cliMissing = summary !== null && !summary.cli.available;
  const hasApps = (summary?.apps.length ?? 0) > 0;

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeader}>
        <h1 id={titleId}>{t('workbench:mobile.placeholders.provider.title')}</h1>
      </div>

      {loadState === 'loading' ? (
        <p className={styles.panelState} role="status">
          {t('workbench:loading')}
        </p>
      ) : null}

      {loadState === 'unsupported' ? (
        <StatusMessage tone="warn">
          {t('workbench:mobile.placeholders.provider.unsupported')}
        </StatusMessage>
      ) : null}

      {loadState === 'error' ? (
        <StatusMessage
          tone="danger"
          action={
            <Button variant="secondary" size="sm" onClick={() => void load()}>
              {t('providerManager:actions.recheck')}
            </Button>
          }
        >
          {t('providerManager:loadFailed', { error: error ?? '' })}
        </StatusMessage>
      ) : null}

      {loadState === 'ready' && summary ? (
        <>
          <p className={styles.panelState}>{t('providerManager:subtitle')}</p>

          <div className={styles.settingsSection}>
            <Button variant="ghost" size="sm" onClick={() => void load()}>
              {t('providerManager:actions.recheck')}
            </Button>
          </div>

          {cliMissing ? (
            <StatusMessage tone="warn">
              {t('providerManager:status.cliMissing')}
              <span className={styles.themeRowMeta}>{t('providerManager:status.cliMissingHint')}</span>
            </StatusMessage>
          ) : null}

          {switchError ? <StatusMessage tone="danger">{switchError}</StatusMessage> : null}

          {!hasApps ? (
            <StatusMessage tone="info">{t('providerManager:noProviders')}</StatusMessage>
          ) : null}

          {summary.apps.map((appProviders) => (
            <AppSection
              key={appProviders.app}
              appProviders={appProviders}
              switchingKey={switchingKey}
              disabled={cliMissing}
              onSwitch={handleSwitch}
            />
          ))}
        </>
      ) : null}
    </section>
  );
}
