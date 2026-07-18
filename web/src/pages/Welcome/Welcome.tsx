/**
 * Welcome 欢迎/权限引导页
 *
 * Business Logic（为什么需要这个页面）:
 *   macOS 等系统要求桌面工具在首次使用前明确申请「屏幕录制 / 输入监控」等
 *   敏感权限，否则后续功能（截图、健康提醒）会静默失败。Welcome 页在路由层
 *   独立于 AppShell，给首次使用的用户「先授权再用」的引导。开发壳与发布版
 *   使用不同的 onboarding/skip localStorage key。
 *   首轮检查失败必须显示错误与重试，不得永久「检查中」。
 *   屏幕录制/辅助功能/输入监控在系统设置打开开关后，**当前进程**常仍报告未授权，
 *   必须完全退出并重新打开后才生效；应用内需醒目提示并标明正确的系统设置条目名
 *   （Dev=`cc-partner (Dev)` / Release=`cc-partner`），避免用户开错开关或只点「重新检查」。
 *
 * Code Logic（这个页面做什么）:
 *   - 解析 app flavor（get_app_identity）→ 专属 onboarded/skipped key + 展示用 appLabel
 *   - 权限卡 mapPermissions；usePermissions({ stopWhenGranted: true })
 *   - 点 TCC「去设置」后置 showRestartHint；四项齐后自动清 hint
 *   - 「继续使用」：写 onboarded、清 skipped → /
 *   - 「暂时跳过」：写 skipped（不写 onboarded）→ /
 *   - 所有 hooks 在 early return 之前
 */

import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, StatusMessage } from '@/components/primitives';
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
 * Business Logic（为什么需要这个常量）:
 *   屏幕录制 / 辅助功能 / 输入监控在 macOS TCC 下授权后，当前进程的
 *   CGPreflight / AXIsProcessTrusted / IOHIDCheckAccess 常仍返回未授权，
 *   必须完全退出再启动；通知权限通常即时生效，不列入。
 *
 * Code Logic（这个常量做什么）:
 *   点「去设置」时若 type 在此集合内，展示 restartAfterGrantHint。
 */
const RESTART_AFTER_GRANT: ReadonlySet<PermissionType> = new Set([
  'screenCapture',
  'accessibility',
  'inputMonitoring',
]);

/**
 * Business Logic（为什么需要这个函数）:
 *   系统设置列表按应用显示名分条；开发壳与发布版必须对应不同条目，
 *   用户开错（如给 Release 开了却跑 Dev）会表现为「已开仍未授权」。
 *
 * Code Logic（这个函数做什么）:
 *   flavor=dev → `cc-partner (Dev)`；否则 `cc-partner`。
 */
function appLabelForFlavor(flavor: AppFlavor): string {
  return flavor === 'dev' ? 'cc-partner (Dev)' : 'cc-partner';
}

/**
 * Welcome 页面根组件
 */
export function Welcome() {
  const { t } = useTranslation(['welcome', 'common']);
  const navigate = useNavigate();
  const [flavor, setFlavor] = useState<AppFlavor>('release');
  /** 用户点过需重启才生效的「去设置」后展示醒目提示（四项齐后清） */
  const [showRestartHint, setShowRestartHint] = useState(false);
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

  // 四项均已授权后清除重启提示，避免成功态残留警告
  useEffect(() => {
    if (allRequiredGranted) {
      setShowRestartHint(false);
    }
  }, [allRequiredGranted]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点单项「去设置」只应请求该权限；TCC 三项授权后当前进程常仍报未授权，
   *   须完全退出重开——应用内必须提示，不能仅依赖系统设置文案。
   *
   * Code Logic（这个函数做什么）:
   *   对 RESTART_AFTER_GRANT 类型置 showRestartHint；调用 request(type)，吞掉 rejection。
   */
  const handleRequest = useCallback(
    (type: PermissionType) => {
      if (RESTART_AFTER_GRANT.has(type)) {
        setShowRestartHint(true);
      }
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
  const appLabel = appLabelForFlavor(flavor);

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

        {showRestartHint && !allRequiredGranted ? (
          <StatusMessage tone="warn">
            {t('welcome:restartAfterGrantHint', { appLabel })}
          </StatusMessage>
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
            {allRequiredGranted
              ? t('welcome:permissionReady')
              : t('welcome:waitingPermission', { appLabel })}
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
