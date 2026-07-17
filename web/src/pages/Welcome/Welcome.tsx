/**
 * Welcome 欢迎/权限引导页
 *
 * Business Logic（为什么需要这个页面）:
 *   macOS 等系统要求桌面工具在首次使用前明确申请「屏幕录制 / 输入监控」等
 *   敏感权限，否则后续功能（截图、健康提醒）会静默失败。Welcome 页在路由层
 *   独立于 AppShell，给首次使用的用户「先授权再用」的引导。开发壳与发布版
 *   使用不同的 onboarding/skip localStorage key。
 *   首轮检查失败必须显示错误与重试，不得永久「检查中」。
 *
 * Code Logic（这个页面做什么）:
 *   - 解析 app flavor（get_app_identity）→ 专属 onboarded/skipped key
 *   - 权限卡 mapPermissions；usePermissions({ stopWhenGranted: true })
 *   - 「继续使用」：写 onboarded、清 skipped → /
 *   - 「暂时跳过」：写 skipped（不写 onboarded）→ /
 *   - 所有 hooks 在 early return 之前
 */

import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { PermissionCard } from '@/components/domain';
import { configApi } from '@/api/config';
import {
  usePermissions,
  permissionOnboardedKey,
  permissionSkippedKey,
  type AppFlavor,
} from '@/hooks/usePermissions';
import { ArrowRightIcon } from '@/lib/icons';
import { mapPermissions } from '@/lib/permissionEntries';
import type { PermissionType } from '@/lib/types';
import appIconUrl from '@/assets/app-icon.png';
import styles from './Welcome.module.css';

/**
 * Welcome 页面根组件
 */
export function Welcome() {
  const { t } = useTranslation(['welcome', 'common']);
  const navigate = useNavigate();
  const [flavor, setFlavor] = useState<AppFlavor>('release');
  const {
    status,
    loading,
    refreshing,
    error,
    requesting,
    allRequiredGranted,
    request,
    refresh,
  } = usePermissions({
    stopWhenGranted: true,
  });

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const identity = await configApi.appIdentity();
        if (!cancelled && (identity.flavor === 'dev' || identity.flavor === 'release')) {
          setFlavor(identity.flavor);
        }
      } catch {
        // 浏览器/旧后端：保持 release
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点单项「去设置」只应请求该权限。
   *
   * Code Logic（这个函数做什么）:
   *   调用 request(type)，吞掉 rejection（error 已由 hook 投影）。
   */
  const handleRequest = useCallback(
    (type: PermissionType) => {
      void request(type).catch(() => undefined);
    },
    [request],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   首轮失败或刷新失败后需要显式「重新检查」。
   *
   * Code Logic（这个函数做什么）:
   *   调用 refresh()。
   */
  const handleRecheck = useCallback(() => {
    void refresh();
  }, [refresh]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   四项已齐时进入主界面，并记「已完成授权」而非「跳过」。
   *
   * Code Logic（这个函数做什么）:
   *   写 flavor 专属 onboarded，清 skipped，navigate /。
   */
  const finishOnboarding = useCallback(() => {
    const onboardedKey = permissionOnboardedKey(flavor);
    const skippedKey = permissionSkippedKey(flavor);
    localStorage.setItem(onboardedKey, '1');
    localStorage.removeItem(skippedKey);
    navigate('/');
  }, [flavor, navigate]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可暂时跳过系统授权；与「已全部授权」分 key，便于之后仍可再引导。
   *
   * Code Logic（这个函数做什么）:
   *   写 flavor 专属 skipped，不写 onboarded，navigate /。
   */
  const skipOnboarding = useCallback(() => {
    const skippedKey = permissionSkippedKey(flavor);
    localStorage.setItem(skippedKey, '1');
    navigate('/');
  }, [flavor, navigate]);
  // hooks 全部在 early return 之前
  if (loading) {
    return (
      <div className={styles.backdrop}>
        <main className={styles.window} aria-label={t('welcome:title')}>
          <img className={styles.brand} src={appIconUrl} alt="" aria-hidden="true" />
          <h1 className={styles.title}>{t('welcome:title')}</h1>
          <p className={styles.subtitle}>{t('welcome:checkingPermission')}</p>
        </main>
      </div>
    );
  }

  if (!status) {
    return (
      <div className={styles.backdrop}>
        <main className={styles.window} aria-label={t('welcome:title')}>
          <img className={styles.brand} src={appIconUrl} alt="" aria-hidden="true" />
          <h1 className={styles.title}>{t('welcome:title')}</h1>
          <p className={styles.subtitle} role="alert">
            {error
              ? t('welcome:checkFailed', { error })
              : t('welcome:checkFailed', { error: t('welcome:unknownError') })}
          </p>
          <footer className={styles.footer}>
            <div className={styles.actions}>
              <Button variant="ghost" size="md" onClick={skipOnboarding}>
                {t('welcome:skip')}
              </Button>
              <Button
                variant="primary"
                size="md"
                onClick={handleRecheck}
                loading={refreshing}
                aria-busy={refreshing}
              >
                {t('welcome:recheck')}
              </Button>
            </div>
          </footer>
        </main>
      </div>
    );
  }

  const entries = mapPermissions(status, t);

  return (
    <div className={styles.backdrop}>
      <main className={styles.window} aria-label={t('welcome:title')}>
        <img className={styles.brand} src={appIconUrl} alt="" aria-hidden="true" />

        <h1 className={styles.title}>{t('welcome:title')}</h1>
        <p className={styles.subtitle}>{t('welcome:subtitle')}</p>

        {error ? (
          <p className={styles.subtitle} role="alert">
            {t('welcome:checkFailed', { error })}
          </p>
        ) : null}

        <div className={styles.permissionList} aria-label={t('welcome:permissionListAriaLabel')}>
          {entries.map((p) => (
            <PermissionCard
              key={p.id}
              icon={p.icon}
              title={p.title}
              description={p.description}
              granted={p.granted}
              requesting={requesting.has(p.id as PermissionType)}
              onRequestAccess={() => handleRequest(p.id as PermissionType)}
            />
          ))}
        </div>

        <footer className={styles.footer}>
          <span className={styles.hint}>
            {allRequiredGranted ? t('welcome:permissionReady') : t('welcome:waitingPermission')}
          </span>
          <div className={styles.actions}>
            <Button
              variant="secondary"
              size="md"
              onClick={handleRecheck}
              loading={refreshing}
              aria-busy={refreshing}
            >
              {t('welcome:recheck')}
            </Button>
            <Button variant="ghost" size="md" onClick={skipOnboarding}>
              {t('welcome:skip')}
            </Button>
            <Button
              variant="primary"
              size="md"
              disabled={!allRequiredGranted}
              onClick={finishOnboarding}
              iconRight={<ArrowRightIcon />}
            >
              {t('welcome:continue')}
            </Button>
          </div>
        </footer>
      </main>
    </div>
  );
}

export default Welcome;
