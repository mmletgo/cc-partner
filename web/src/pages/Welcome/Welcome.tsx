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
 *   macOS 对屏幕录制/辅助功能/输入监控：系统设置里打开开关后，**当前进程**的
 *   检测 API 常仍返回未授权。产品目标是「用户在系统里授权后 Welcome 显示已授权」
 *   ——不在 UI 文案里教育用户手动退出；改为：从系统设置回到应用后多轮 recheck，
 *   若 TCC 三项仍未齐，静默经 LaunchServices `open` 重启一次以应用授权态
 *   （禁止直接 exec Contents/MacOS，否则会丢 TCC 主体）。
 *
 * Code Logic（这个页面做什么）:
 *   - 解析 app flavor（get_app_identity）→ 专属 onboarded/skipped key
 *   - 权限卡 mapPermissions；usePermissions({ stopWhenGranted: true })
 *   - 点 TCC「去设置」后记 pendingApply；visibility 恢复 → 多轮 refresh →
 *     仍缺则 configApi.relaunchForPermissions() 一次
 *   - 「继续使用」：写 onboarded、清 skipped → /
 *   - 「暂时跳过」：写 skipped（不写 onboarded）→ /
 *   - 所有 hooks 在 early return 之前
 */

import { useCallback, useEffect, useRef, useState } from 'react';
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
import type { PermissionType, PermissionsStatus } from '@/lib/types';
import appIconUrl from '@/assets/app-icon.png';
import styles from './Welcome.module.css';

/**
 * Business Logic（为什么需要这个常量）:
 *   屏幕录制 / 辅助功能 / 输入监控在 macOS 上授权后，当前进程检测常滞后于
 *   系统设置开关；从设置返回后若仍未齐需 relaunch 应用进程态。通知通常即时。
 *
 * Code Logic（这个常量做什么）:
 *   点「去设置」时若 type 在此集合内，标记 pendingApply 以便 visibility 恢复后处理。
 */
const PROCESS_STICKY_PERMISSIONS: ReadonlySet<PermissionType> = new Set([
  'screenCapture',
  'accessibility',
  'inputMonitoring',
]);

/**
 * Business Logic（为什么需要这个函数）:
 *   判断「是否仍有需进程重启才生效的权限未授权」。
 *
 * Code Logic（这个函数做什么）:
 *   任一项 sticky 权限 granted=false 则 true。
 */
function hasStickyDenied(status: PermissionsStatus): boolean {
  return (
    !status.screenCapture.granted ||
    !status.accessibility.granted ||
    !status.inputMonitoring.granted
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   给 OS/检测 API 一点时间在切回前台后刷新。
 *
 * Code Logic（这个函数做什么）:
 *   Promise 封装 setTimeout。
 */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

/**
 * Welcome 页面根组件
 */
export function Welcome() {
  const { t } = useTranslation(['welcome', 'common']);
  const navigate = useNavigate();
  const [flavor, setFlavor] = useState<AppFlavor>('release');
  /** 用户点过 sticky「去设置」，从系统设置返回后需 recheck / 可能 relaunch */
  const pendingApplyRef = useRef(false);
  /** 本页生命周期内最多静默 relaunch 一次，避免未授权时循环重启 */
  const relaunchUsedRef = useRef(false);
  /** 避免 visibility 回调重入 */
  const applyInFlightRef = useRef(false);

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

  // 四项齐：清 pending，无需 relaunch
  useEffect(() => {
    if (allRequiredGranted) {
      pendingApplyRef.current = false;
    }
  }, [allRequiredGranted]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从系统设置回到 Welcome 后，应尽快反映已授权；当前进程若仍读到未授权，
   *   则静默 relaunch 一次让 TCC 在新进程生效——不向用户展示「请退出重开」文案。
   *
   * Code Logic（这个函数做什么）:
   *   多轮 refresh + 直接 check_permissions；仍 sticky-denied 且未 relaunch 过则
   *   invoke relaunch_for_permissions（macOS 用 open .app）。
   */
  const applyPermissionsAfterSettings = useCallback(async () => {
    if (!pendingApplyRef.current || relaunchUsedRef.current || applyInFlightRef.current) {
      return;
    }
    applyInFlightRef.current = true;
    try {
      // 多轮 recheck：屏幕录制/辅助功能常在无需重启时于数百 ms 内翻转
      for (const delay of [0, 400, 900]) {
        if (delay > 0) {
          await sleep(delay);
        }
        await refresh();
        try {
          const latest = await configApi.permissions();
          if (!hasStickyDenied(latest)) {
            pendingApplyRef.current = false;
            return;
          }
        } catch {
          // 保留 pending，继续后续轮次 / relaunch
        }
      }

      if (!pendingApplyRef.current || relaunchUsedRef.current) {
        return;
      }

      let stillDenied = true;
      try {
        stillDenied = hasStickyDenied(await configApi.permissions());
      } catch {
        stillDenied = true;
      }
      if (!stillDenied) {
        pendingApplyRef.current = false;
        return;
      }

      // 静默 relaunch 一次以应用系统侧已打开的开关
      relaunchUsedRef.current = true;
      pendingApplyRef.current = false;
      await configApi.relaunchForPermissions();
    } finally {
      applyInFlightRef.current = false;
    }
  }, [refresh]);

  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState !== 'visible') {
        return;
      }
      void applyPermissionsAfterSettings();
    };
    document.addEventListener('visibilitychange', onVisibility);
    // 部分路径下从设置返回时窗口已 visible 且不派发 visibilitychange：
    // 用 focus 再兜一次
    window.addEventListener('focus', onVisibility);
    return () => {
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('focus', onVisibility);
    };
  }, [applyPermissionsAfterSettings]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点单项「去设置」只应请求该权限；对 sticky 类型标记 pendingApply，
   *   从设置返回后由 applyPermissionsAfterSettings 对齐或 relaunch。
   *
   * Code Logic（这个函数做什么）:
   *   标记 pending；request(type)；结束后再 force refresh。
   */
  const handleRequest = useCallback(
    (type: PermissionType) => {
      if (PROCESS_STICKY_PERMISSIONS.has(type)) {
        pendingApplyRef.current = true;
      }
      void (async () => {
        try {
          await request(type);
        } catch {
          // error 已由 hook 投影
        }
        // 若系统框（通知）即时授权，立即对齐；sticky 类型可能仍 false，等 visibility
        await refresh();
        void applyPermissionsAfterSettings();
      })();
    },
    [request, refresh, applyPermissionsAfterSettings],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   首轮失败或刷新失败后需要显式「重新检查」。
   *
   * Code Logic（这个函数做什么）:
   *   调用 refresh()；若仍有 pendingApply 则走 apply 路径。
   */
  const handleRecheck = useCallback(() => {
    void (async () => {
      await refresh();
      if (pendingApplyRef.current) {
        void applyPermissionsAfterSettings();
      }
    })();
  }, [refresh, applyPermissionsAfterSettings]);

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
