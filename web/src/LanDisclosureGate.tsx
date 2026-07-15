/**
 * LanDisclosureGate — App 级 LAN 风险披露守卫。
 *
 * Business Logic（为什么需要这个组件）:
 *   覆盖新用户、升级用户与 Welcome skip：未确认前不得进入产品路由与权限 onboarding。
 *   展示本机地址、首选 TCP 62116、mDNS UDP 5353、端口递增说明与无身份校验风险文案。
 *
 * Code Logic（这个组件做什么）:
 *   使用 useLanDisclosureStartup；hooks 全在 early return 前；
 *   `/screenshot-overlay` 与 `/health-overlay` 旁路（系统遮罩不依赖 LAN）；
 *   loading/required/starting/error 渲染 gate UI；pass 渲染 children。
 */

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation } from 'react-router-dom';
import { Button, Card, StatusMessage } from '@/components/primitives';
import { useLanDisclosureStartup } from '@/hooks/useLanDisclosureStartup';
import styles from './LanDisclosureGate.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   截图/健康遮罩是系统级独立窗口，不依赖 LAN 服务，也不应被披露 gate 挡住。
 *
 * Code Logic（这个函数做什么）:
 *   pathname 精确匹配 `/screenshot-overlay` 或 `/health-overlay`（忽略 query）时返回 true。
 */
function isOverlayBypassPath(pathname: string): boolean {
  return pathname === '/screenshot-overlay' || pathname === '/health-overlay';
}

export type LanDisclosureGateProps = {
  children: ReactNode;
};

/**
 * Business Logic（为什么需要这个组件）:
 *   必须在 Routes 与 OnboardingGuard 之上拦截，Welcome skip 不能绕过。
 *
 * Code Logic（这个组件做什么）:
 *   phase=pass 渲染 children；其余展示披露卡/加载/错误重试。
 */
export function LanDisclosureGate({ children }: LanDisclosureGateProps): ReactNode {
  const { t } = useTranslation(['welcome', 'common']);
  const { pathname } = useLocation();
  const { phase, status, startResult, error, acknowledge, retry, openDiagnostics } =
    useLanDisclosureStartup();

  // hooks 全部在 early return 之前
  // 系统遮罩窗口：不依赖 LAN 后端，披露未确认时也必须能渲染（截图/健康提醒）。
  if (isOverlayBypassPath(pathname) || phase === 'pass') {
    return children;
  }

  if (phase === 'loading') {
    return (
      <div className={styles.backdrop} data-testid="lan-disclosure-gate">
        <main className={styles.window} aria-label={t('welcome:lanDisclosure.title')}>
          <h1 className={styles.title}>{t('welcome:lanDisclosure.title')}</h1>
          <p className={styles.subtitle}>{t('welcome:lanDisclosure.loading')}</p>
        </main>
      </div>
    );
  }

  const addresses =
    startResult?.localAddresses?.length
      ? startResult.localAddresses
      : (status?.localAddresses ?? []);
  const preferredPort = status?.preferredPort ?? 62116;
  const mdnsPort = status?.mdnsPort ?? 5353;
  const alreadyRunning = status?.alreadyRunning ?? false;
  const actualPort = startResult?.actualHttpPort ?? status?.actualHttpPort ?? null;

  return (
    <div className={styles.backdrop} data-testid="lan-disclosure-gate">
      <main className={styles.window} aria-label={t('welcome:lanDisclosure.title')}>
        <h1 className={styles.title}>{t('welcome:lanDisclosure.title')}</h1>
        <p className={styles.subtitle}>{t('welcome:lanDisclosure.subtitle')}</p>

        <Card variant="elevated" className={styles.card}>
          <Card.Body>
            <ul className={styles.factList}>
              <li>
                {t('welcome:lanDisclosure.localAddresses')}:{' '}
                {addresses.length > 0
                  ? addresses.join(', ')
                  : t('welcome:lanDisclosure.noAddresses')}
              </li>
              <li>
                {t('welcome:lanDisclosure.preferredPort', { port: preferredPort })}
              </li>
              <li>{t('welcome:lanDisclosure.portIncrement')}</li>
              <li>{t('welcome:lanDisclosure.mdnsPort', { port: mdnsPort })}</li>
              {alreadyRunning || actualPort != null ? (
                <li>
                  {t('welcome:lanDisclosure.alreadyRunning', {
                    port: actualPort ?? preferredPort,
                  })}
                </li>
              ) : null}
            </ul>
            <p className={styles.risk} role="note">
              {t('welcome:lanDisclosure.noIdentityRisk')}
            </p>
            {alreadyRunning ? (
              <p className={styles.hint}>{t('welcome:lanDisclosure.cliRunningHint')}</p>
            ) : null}
          </Card.Body>
        </Card>

        {error ? (
          <StatusMessage tone="danger" action={
            <Button variant="secondary" size="sm" onClick={() => void retry()}>
              {t('welcome:lanDisclosure.retry')}
            </Button>
          }>
            {t('welcome:lanDisclosure.error', { error })}
          </StatusMessage>
        ) : null}

        <footer className={styles.footer}>
          <Button
            variant="ghost"
            size="md"
            onClick={() => void openDiagnostics()}
          >
            {t('welcome:lanDisclosure.openDiagnostics')}
          </Button>
          <Button
            variant="primary"
            size="md"
            loading={phase === 'starting'}
            disabled={phase === 'starting'}
            onClick={() => void acknowledge()}
            data-testid="lan-disclosure-acknowledge"
          >
            {t('welcome:lanDisclosure.acknowledge')}
          </Button>
        </footer>
      </main>
    </div>
  );
}

export default LanDisclosureGate;
