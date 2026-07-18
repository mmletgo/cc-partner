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
 *   检测 API 常仍返回未授权。产品目标是「用户在系统里授权后 Welcome 尽量同步
 *   显示已授权」——不自动 relaunch（避免闪白屏/反复重启）。从设置回前台后多轮
 *   recheck；若 sticky 三项仍未齐，进入 needs_reopen，由用户可选点击
 *   「重新打开应用」才调用 relaunchForPermissions。
 *
 * Code Logic（这个页面做什么）:
 *   - 解析 app flavor（get_app_identity）→ 专属 onboarded/skipped key
 *   - 权限卡 mapPermissions；usePermissions({ stopWhenGranted: true })
 *   - 相位机 welcomePermissionFlow：GO_SETTINGS → awaiting；visibility/focus
 *     → FOREGROUND + SYNC_DELAYS recheck；耗尽仍 denied → needs_reopen
 *   - handleRequest / visibility / recheck **禁止** relaunch；仅 handleReopen 可
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
import type { PermissionType } from '@/lib/types';
import appIconUrl from '@/assets/app-icon.png';
import styles from './Welcome.module.css';
import {
  SYNC_DELAYS_MS,
  hasStickyDenied,
  isStickyPermission,
  reduceWelcomePermPhase,
  welcomeHintKey,
  type WelcomePermEvent,
  type WelcomePermPhase,
} from './welcomePermissionFlow';

/**
 * Welcome 页面根组件
 *
 * Business Logic（为什么需要这个组件）:
 *   首次启动权限引导主 UI；协调权限卡、状态提示与可选重新打开。
 *
 * Code Logic（这个组件做什么）:
 *   持有 WelcomePermPhase 状态机；接线 request/refresh/relaunch；渲染引导布局。
 */
export function Welcome() {
  const { t } = useTranslation(['welcome', 'common']);
  const navigate = useNavigate();
  const [flavor, setFlavor] = useState<AppFlavor>('release');
  const [phase, setPhase] = useState<WelcomePermPhase>('idle');
  const phaseRef = useRef<WelcomePermPhase>(phase);
  /** 防止 visibility/focus 并发进入多轮 sync */
  const syncInFlightRef = useRef(false);

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

  /**
   * Business Logic（为什么需要这个函数）:
   *   相位转移必须同步更新 ref，否则 await 间隙里 SYNC_TICK 会读到旧 phase。
   *
   * Code Logic（这个函数做什么）:
   *   以 phaseRef 为源 reduce → 写回 ref + setPhase。
   */
  const dispatch = useCallback((event: WelcomePermEvent) => {
    const next = reduceWelcomePermPhase(phaseRef.current, event);
    phaseRef.current = next;
    setPhase(next);
  }, []);

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

  // 四项齐：相位回 idle
  useEffect(() => {
    if (allRequiredGranted) {
      dispatch({ type: 'ALL_REQUIRED_GRANTED' });
    }
  }, [allRequiredGranted, dispatch]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从系统设置回到应用后，应尽快反映已授权；仍未齐则进入 needs_reopen
   *   提示可选重新打开——**禁止**自动 relaunch。
   *
   * Code Logic（这个函数做什么）:
   *   仅在 awaiting/needs_reopen/syncing 时：FOREGROUND → 按 SYNC_DELAYS_MS
   *   多轮 refresh + permissions()；SYNC_TICK / SYNC_EXHAUSTED 更新相位。
   */
  const runSyncAfterForeground = useCallback(async () => {
    const start = phaseRef.current;
    if (start !== 'awaiting' && start !== 'needs_reopen' && start !== 'syncing') {
      return;
    }
    if (syncInFlightRef.current) {
      return;
    }
    syncInFlightRef.current = true;
    /** permissions() 全轮失败时仍用全 denied 片推到 needs_reopen，避免卡在 syncing */
    const deniedSlice = {
      screenCapture: { granted: false },
      accessibility: { granted: false },
      inputMonitoring: { granted: false },
    };
    try {
      dispatch({ type: 'FOREGROUND' });
      let lastDenied = deniedSlice;
      for (let i = 0; i < SYNC_DELAYS_MS.length; i++) {
        const delay = SYNC_DELAYS_MS[i]!;
        if (delay > 0) {
          await new Promise<void>((resolve) => {
            window.setTimeout(resolve, delay);
          });
        }
        await refresh();
        let slice;
        try {
          slice = await configApi.permissions();
        } catch {
          // 本轮读失败：继续后续轮次；耗尽后用 lastDenied / deniedSlice 收口
          continue;
        }
        if (!hasStickyDenied(slice)) {
          dispatch({ type: 'SYNC_TICK', status: slice });
          return;
        }
        lastDenied = {
          screenCapture: slice.screenCapture,
          accessibility: slice.accessibility,
          inputMonitoring: slice.inputMonitoring,
        };
        if (i < SYNC_DELAYS_MS.length - 1) {
          dispatch({ type: 'SYNC_TICK', status: lastDenied });
        }
      }
      // 多轮 recheck 后 sticky 仍 denied，或全部 catch：进入 needs_reopen
      dispatch({ type: 'SYNC_EXHAUSTED', status: lastDenied });
    } finally {
      syncInFlightRef.current = false;
    }
  }, [dispatch, refresh]);

  useEffect(() => {
    const onVis = () => {
      if (document.visibilityState === 'visible') {
        void runSyncAfterForeground();
      }
    };
    document.addEventListener('visibilitychange', onVis);
    // 部分路径下从设置返回时窗口已 visible 且不派发 visibilitychange：用 focus 兜底
    window.addEventListener('focus', onVis);
    return () => {
      document.removeEventListener('visibilitychange', onVis);
      window.removeEventListener('focus', onVis);
    };
  }, [runSyncAfterForeground]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点单项「去设置」只应请求该权限；sticky 类型进入 awaiting。
   *   macOS 打开系统设置时 WebView 常仍 visible，visibilitychange 不触发，
   *   若只靠 focus/visibility 则永远进不了 needs_reopen（卡片一直「去设置」）。
   *   系统设置内打开开关后当前进程 sticky TCC 常仍 denied——必须多轮 recheck 后
   *   进入 needs_reopen 露出「重新打开应用」，否则用户只看到卡片仍「去设置」。
   *   **禁止**自动 relaunch。
   *
   * Code Logic（这个函数做什么）:
   *   dispatch GO_SETTINGS；await request(type)；refresh；
   *   sticky 则多次 schedule runSyncAfterForeground（800ms / 2.5s / 5s），
   *   覆盖「设置窗弹出后立刻同步」与「用户在设置里授权后仍在前台」两种时序。
   */
  const handleRequest = useCallback(
    (type: PermissionType) => {
      dispatch({ type: 'GO_SETTINGS', permission: type });
      void (async () => {
        try {
          await request(type);
        } catch {
          // error 已由 hook 投影
        }
        await refresh();
        if (isStickyPermission(type)) {
          // 不依赖 visibility：开系统设置后窗口可能一直 visible
          for (const delay of [800, 2500, 5000]) {
            window.setTimeout(() => {
              void runSyncAfterForeground();
            }, delay);
          }
        }
      })();
    },
    [dispatch, request, refresh, runSyncAfterForeground],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   仅用户主动点击「重新打开应用」才可 relaunch，以应用 sticky TCC 进程态。
   *
   * Code Logic（这个函数做什么）:
   *   dispatch REOPEN_CLICKED；invoke relaunchForPermissions（可能永不 resolve）。
   */
  const handleReopen = useCallback(() => {
    dispatch({ type: 'REOPEN_CLICKED' });
    void configApi.relaunchForPermissions().catch(() => undefined);
  }, [dispatch]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   首轮失败或刷新失败后需要显式「重新检查」。
   *
   * Code Logic（这个函数做什么）:
   *   调用 refresh()；若处于 awaiting/needs_reopen/syncing 则走前台同步路径。
   */
  const handleRecheck = useCallback(() => {
    void (async () => {
      await refresh();
      void runSyncAfterForeground();
    })();
  }, [refresh, runSyncAfterForeground]);

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
  const hintKey = welcomeHintKey(phase, allRequiredGranted);

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
          <span className={styles.hint}>{t(`welcome:${hintKey}`)}</span>
          <div className={styles.actions}>
            {phase === 'needs_reopen' ? (
              <Button variant="secondary" size="md" onClick={handleReopen}>
                {t('welcome:reopenApp')}
              </Button>
            ) : null}
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
