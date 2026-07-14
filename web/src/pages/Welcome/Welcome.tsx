/**
 * Welcome 欢迎/权限引导页
 *
 * Business Logic（为什么需要这个页面）:
 *   macOS 等系统要求桌面工具在首次使用前明确申请「屏幕录制 / 输入监控」等
 *   敏感权限，否则后续功能（截图、全局快捷键）会静默失败。Welcome 页在路由层
 *   独立于 AppShell（不进入主窗口），给首次使用的用户一个「先授权再用」的引导。
 *   首轮检查失败必须显示错误与重试，不得永久「检查中」。
 *
 * Code Logic（这个页面做什么）:
 *   - 全屏深色背景模拟 macOS 权限弹窗，居中 Window 容器展示 logo/标题/权限卡/CTA
 *   - 权限卡由 mapPermissions 渲染；每张卡 onRequest={() => request(entry.type)}
 *   - usePermissions({ stopWhenGranted: true }) 基于可见性轮询；required 全授权后停轮询
 *   - 首轮 loading 仅短暂显示 checking；失败展示 error + 重新检查；有状态时刷新失败保留卡片
 *   - 「继续使用」/「暂时跳过」写入 PERMISSION_ONBOARDED_KEY 后导航首页
 *   - 所有 hooks 集中在组件顶部，early return 之前
 */

import { useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { PermissionCard } from '@/components/domain';
import { usePermissions, PERMISSION_ONBOARDED_KEY } from '@/hooks/usePermissions';
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

  const finishOnboarding = useCallback(() => {
    localStorage.setItem(PERMISSION_ONBOARDED_KEY, '1');
    navigate('/');
  }, [navigate]);

  // hooks 全部在 early return 之前
  if (loading) {
    return (
      <div className={styles.backdrop}>
        <div className={styles.window} role="dialog" aria-label={t('welcome:title')}>
          <img className={styles.brand} src={appIconUrl} alt="" aria-hidden="true" />
          <h1 className={styles.title}>{t('welcome:title')}</h1>
          <p className={styles.subtitle}>{t('welcome:checkingPermission')}</p>
        </div>
      </div>
    );
  }

  if (!status) {
    return (
      <div className={styles.backdrop}>
        <div className={styles.window} role="dialog" aria-label={t('welcome:title')}>
          <img className={styles.brand} src={appIconUrl} alt="" aria-hidden="true" />
          <h1 className={styles.title}>{t('welcome:title')}</h1>
          <p className={styles.subtitle} role="alert">
            {error
              ? t('welcome:checkFailed', { error })
              : t('welcome:checkFailed', { error: t('welcome:unknownError') })}
          </p>
          <footer className={styles.footer}>
            <div className={styles.actions}>
              <Button variant="ghost" size="md" onClick={finishOnboarding}>
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
        </div>
      </div>
    );
  }

  const entries = mapPermissions(status, t);

  return (
    <div className={styles.backdrop}>
      <div className={styles.window} role="dialog" aria-label={t('welcome:title')}>
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
            <Button variant="ghost" size="md" onClick={finishOnboarding}>
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
      </div>
    </div>
  );
}

export default Welcome;
