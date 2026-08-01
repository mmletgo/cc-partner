/**
 * PermissionStatusBadge 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   侧栏底部需要一个常驻的权限状态指示器：当产品展示权限任一未授权时
 *   显示，提示用户「需要授权」，点击进入 Welcome 逐项处理。
 *   全部授权后自动隐藏。它是 Welcome 首次引导之后的长期兜底入口。
 *
 * Code Logic（这个组件做什么）:
 *   - 用 usePermissions() 持续轮询权限（不停止，作长期兜底）
 *   - loading 或 allGranted 时不渲染
 *   - 未授权时渲染可点击横条：红色 StatusDot(busy) + 文案「需要授权」
 *   - 点击导航 `/welcome`，不批量触发权限副作用
 *   - 根元素 margin-top: auto 贴 Sidebar 内容区底部
 */

import { memo, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { StatusDot } from '@/components/primitives';
import { usePermissions } from '@/hooks/usePermissions';
import styles from './PermissionStatusBadge.module.css';

function PermissionStatusBadgeInner() {
  const { t } = useTranslation(['common']);
  const navigate = useNavigate();
  const { loading, allGranted } = usePermissions();

  const handleClick = useCallback(() => {
    navigate('/welcome');
  }, [navigate]);

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
