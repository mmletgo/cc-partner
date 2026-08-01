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
 *   - 相位机 welcomePermissionFlow：GO_SETTINGS → awaiting；visibility/focus /
 *     POST_SETTINGS 调度 → FOREGROUND + SYNC_DELAYS recheck；耗尽仍 denied →
 *     needs_reopen（「重新打开应用」保持到 sticky 全齐，recheck 不卸按钮）
 *   - 「重新检查」在 sticky 仍 denied 时经 USER_RECHECK 强制进入同步路径
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
import { mapPermissions, type PermissionEntryAction } from '@/lib/permissionEntries';
import type { PermissionType } from '@/lib/types';
import appIconUrl from '@/assets/app-icon.png';
import styles from './Welcome.module.css';
import {
  POST_SETTINGS_SYNC_SCHEDULE_MS,
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
  /** in-flight 期间又有 sync 请求时，结束后补跑一轮，避免丢掉 needs_reopen 收口 */
  const syncQueuedRef = useRef(false);
  /** sticky 去设置后 schedule 的 timer，卸载时清理 */
  const postSettingsTimersRef = useRef<number[]>([]);
  /** 最新 runSync，供 finally 排队补跑（避免 useCallback 自引用） */
  const runSyncRef = useRef<() => Promise<void>>(async () => undefined);

  const {
    status,
    loading,
    refreshing,
    error,
    requesting,
    allRequiredGranted,
    request,
    openSettings,
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
   *   in-flight 期间的再次请求必须排队，否则会丢掉耗尽收口，按钮永不出现。
   *
   * Code Logic（这个函数做什么）:
   *   仅在 awaiting/needs_reopen/syncing 时：FOREGROUND → 按 SYNC_DELAYS_MS
   *   多轮 refresh + permissions()；SYNC_TICK / SYNC_EXHAUSTED 更新相位；
   *   in-flight 时 set syncQueued，结束后补跑。
   */
  const runSyncAfterForeground = useCallback(async () => {
    const start = phaseRef.current;
    if (start !== 'awaiting' && start !== 'needs_reopen' && start !== 'syncing') {
      return;
    }
    if (syncInFlightRef.current) {
      syncQueuedRef.current = true;
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
      do {
        syncQueuedRef.current = false;
        dispatch({ type: 'FOREGROUND' });
        let lastDenied = deniedSlice;
        let exhausted = true;
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
            exhausted = false;
            break;
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
        // 多轮 recheck 后 sticky 仍 denied，或全部 catch：进入 / 保持 needs_reopen
        if (exhausted) {
          dispatch({ type: 'SYNC_EXHAUSTED', status: lastDenied });
        }
      } while (syncQueuedRef.current);
    } finally {
      syncInFlightRef.current = false;
      // do-while 退出后若又排队：经 ref 补跑（phase 已是 syncing/needs_reopen）
      if (syncQueuedRef.current) {
        window.setTimeout(() => {
          void runSyncRef.current();
        }, 0);
      }
    }
  }, [dispatch, refresh]);

  useEffect(() => {
    runSyncRef.current = runSyncAfterForeground;
  }, [runSyncAfterForeground]);

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
      for (const id of postSettingsTimersRef.current) {
        window.clearTimeout(id);
      }
      postSettingsTimersRef.current = [];
    };
  }, [runSyncAfterForeground]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   sticky 去设置后必须主动进入同步并最终 needs_reopen（系统设置时 WebView
   *   常仍 visible，不能只靠 visibility）。**禁止**自动 relaunch。
   *
   * Code Logic（这个函数做什么）:
   *   清旧 timer；按 POST_SETTINGS_SYNC_SCHEDULE_MS 调度 runSyncAfterForeground；
   *   in-flight 由 queue 合并，保证能耗尽收口。
   */
  const schedulePostSettingsSync = useCallback(() => {
    for (const id of postSettingsTimersRef.current) {
      window.clearTimeout(id);
    }
    postSettingsTimersRef.current = POST_SETTINGS_SYNC_SCHEDULE_MS.map((delay) =>
      window.setTimeout(() => {
        void runSyncAfterForeground();
      }, delay),
    );
  }, [runSyncAfterForeground]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   四态必须映射为独立动作：notDetermined=Request、Denied/Unavailable=Open Settings；
   *   任何一步都不得自动 relaunch。
   *
   * Code Logic（这个函数做什么）:
   *   request/openSettings 调各自 hook 后刷新，sticky 再进入同步相位。
   */
  const handlePermissionAction = useCallback(
    (type: PermissionType, action: PermissionEntryAction) => {
      if (action === 'none') return;

      dispatch({ type: 'GO_SETTINGS', permission: type });
      void (async () => {
        try {
          if (action === 'request') {
            await request(type);
          } else {
            await openSettings(type);
          }
        } catch {
          // error 已由 hook 投影
        }
        await refresh();
        if (isStickyPermission(type)) {
          schedulePostSettingsSync();
        }
      })();
    },
    [dispatch, openSettings, request, refresh, schedulePostSettingsSync],
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
   *   「重新检查」必须能把 sticky 仍 denied 的路径推进到 needs_reopen；
   *   过去在 idle 时 runSync 直接 return，用户点重新检查永远看不到重新打开。
   *
   * Code Logic（这个函数做什么）:
   *   refresh；若 sticky denied → USER_RECHECK 进入 syncing/保持 needs_reopen 再跑同步；
   *   否则仅 refresh。
   */
  const handleRecheck = useCallback(() => {
    void (async () => {
      await refresh();
      let slice;
      try {
        slice = await configApi.permissions();
      } catch {
        // 读失败仍尝试同步路径（runSync 内用 denied 片收口）
        dispatch({ type: 'USER_RECHECK' });
        void runSyncAfterForeground();
        return;
      }
      if (hasStickyDenied(slice)) {
        dispatch({ type: 'USER_RECHECK' });
        void runSyncAfterForeground();
      }
    })();
  }, [dispatch, refresh, runSyncAfterForeground]);

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
              requesting={requesting.has(p.id)}
              actionLabel={p.actionLabel}
              onRequestAccess={() => handlePermissionAction(p.id, p.action)}
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
