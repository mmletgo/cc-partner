/**
 * Welcome 欢迎/权限引导页
 *
 * Business Logic（为什么需要这个页面）:
 *   macOS 等系统要求桌面工具在首次使用前明确申请「屏幕录制 / 输入监控」等
 *   敏感权限，否则后续功能（截图、全局快捷键）会静默失败。Welcome 页在路由层
 *   独立于 AppShell（不进入主窗口），给首次使用的用户一个「先授权再用」的引导。
 *
 * Code Logic（这个页面做什么）:
 *   - 全屏深色背景模拟 macOS 权限弹窗，居中 Window 容器展示 logo/标题/权限卡/CTA
 *   - 两条权限卡：屏幕录制 / 输入监控（通知权限已移除，本项目无系统通知功能）
 *   - 用 usePermissions({ stopWhenGranted: true }) 轮询，全部授权后自动停止
 *   - PermissionCard 的「去设置」点击 → requestMissing()（弹系统授权框 + 打开设置面板）
 *   - 「继续使用」/「暂时跳过」都会写入 PERMISSION_ONBOARDED_KEY 后导航到首页，
 *     避免每次启动重复跳转
 *   - 所有 hooks 集中在组件顶部，early return 之前
 */

import { useCallback, type ReactElement } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { Button } from '@/components/primitives';
import { PermissionCard } from '@/components/domain';
import { usePermissions, PERMISSION_ONBOARDED_KEY } from '@/hooks/usePermissions';
import type { PermissionsStatus } from '@/lib/types';
import {
  InfoIcon,
  KeyboardIcon,
  ArrowRightIcon,
} from '@/lib/icons';
import styles from './Welcome.module.css';

/** 单条权限条目的展示格式 */
interface PermissionEntry {
  id: string;
  icon: ReactElement;
  title: string;
  description: string;
  granted: boolean;
}

/**
 * 将后端 PermissionsStatus 转换为 PermissionEntry 列表（屏幕录制 + 输入监控）
 *
 * @param status - 后端返回的权限状态
 * @param t - i18next 翻译函数（welcome ns）
 * @returns 用于渲染的权限条目数组
 */
function mapPermissions(status: PermissionsStatus, t: TFunction<'welcome'>): PermissionEntry[] {
  return [
    {
      id: 'screenCapture',
      icon: <InfoIcon />,
      title: t('permission.screenRecording.title'),
      description: t('permission.screenRecording.description'),
      granted: status.screenCapture.granted,
    },
    {
      id: 'inputMonitoring',
      icon: <KeyboardIcon />,
      title: t('permission.inputMonitoring.title'),
      description: t('permission.inputMonitoring.description'),
      granted: status.inputMonitoring.granted,
    },
  ];
}

/**
 * Welcome 页面根组件
 */
export function Welcome() {
  const { t } = useTranslation(['welcome']);
  const navigate = useNavigate();
  const { status, loading, allGranted, requestMissing } = usePermissions({
    stopWhenGranted: true,
  });

  const handleRequest = useCallback(() => {
    void requestMissing();
  }, [requestMissing]);

  const finishOnboarding = useCallback(() => {
    localStorage.setItem(PERMISSION_ONBOARDED_KEY, '1');
    navigate('/');
  }, [navigate]);

  // loading：首次 API 请求尚未返回（hooks 在 early return 之前）
  if (loading || !status) {
    return (
      <div className={styles.backdrop}>
        <div className={styles.window} role="dialog" aria-label={t('welcome:title')}>
          <div className={styles.brand} aria-hidden="true">
            CP
          </div>
          <h1 className={styles.title}>{t('welcome:title')}</h1>
          <p className={styles.subtitle}>{t('welcome:checkingPermission')}</p>
        </div>
      </div>
    );
  }

  const entries = mapPermissions(status, t);

  return (
    <div className={styles.backdrop}>
      <div className={styles.window} role="dialog" aria-label={t('welcome:title')}>
        <div className={styles.brand} aria-hidden="true">
          CP
        </div>

        <h1 className={styles.title}>{t('welcome:title')}</h1>
        <p className={styles.subtitle}>{t('welcome:subtitle')}</p>

        <div className={styles.permissionList} aria-label={t('welcome:permissionListAriaLabel')}>
          {entries.map((p) => (
            <PermissionCard
              key={p.id}
              icon={p.icon}
              title={p.title}
              description={p.description}
              granted={p.granted}
              onRequestAccess={handleRequest}
            />
          ))}
        </div>

        <footer className={styles.footer}>
          <span className={styles.hint}>
            {allGranted ? t('welcome:permissionReady') : t('welcome:waitingPermission')}
          </span>
          <div className={styles.actions}>
            <Button variant="ghost" size="md" onClick={finishOnboarding}>
              {t('welcome:skip')}
            </Button>
            <Button
              variant="primary"
              size="md"
              disabled={!allGranted}
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
