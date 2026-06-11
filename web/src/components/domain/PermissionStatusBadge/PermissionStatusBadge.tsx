/**
 * PermissionStatusBadge 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   侧栏底部需要一个常驻的权限状态指示器：当屏幕录制/输入监控任一未授权时
 *   显示，提示用户「需要授权」，点击触发后端请求授权 + 打开系统设置面板。
 *   全部授权后自动隐藏。它是 Welcome 首次引导之后的长期兜底入口。
 *
 * Code Logic（这个组件做什么）:
 *   - 用 usePermissions() 持续轮询权限（不停止，作长期兜底）
 *   - loading 或 allGranted 时不渲染
 *   - 未授权时渲染可点击横条：红色 StatusDot(busy) + 文案「需要授权」
 *   - 点击调用 requestMissing()（弹系统授权框 + 打开设置面板）
 *   - 根元素 margin-top: auto 贴 Sidebar 内容区底部
 */

import { memo, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { StatusDot } from '@/components/primitives';
import { usePermissions } from '@/hooks/usePermissions';
import styles from './PermissionStatusBadge.module.css';

function PermissionStatusBadgeInner() {
  const { t } = useTranslation(['common']);
  const { loading, allGranted, requestMissing } = usePermissions();

  const handleClick = useCallback(() => {
    void requestMissing();
  }, [requestMissing]);

  // hooks 在 early return 之前（React 规则：hooks 调用顺序不能条件化）
  if (loading || allGranted) {
    return null;
  }

  return (
    <button
      type="button"
      className={styles.badge}
      onClick={handleClick}
      title={t('common:permission.tapToGrant')}
    >
      <StatusDot status="busy" size="sm" />
      <span className={styles.text}>{t('common:permission.needsGrant')}</span>
    </button>
  );
}

export const PermissionStatusBadge = memo(PermissionStatusBadgeInner);
PermissionStatusBadge.displayName = 'PermissionStatusBadge';
