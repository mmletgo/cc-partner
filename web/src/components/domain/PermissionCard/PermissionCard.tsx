/**
 * PermissionCard 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   macOS 权限引导欢迎页与设置页权限管理需要把每条权限的状态渲染成
 *   "图标 + 标题 + 说明 + 授权状态/操作"的标准单元。统一的卡片外观和固定的
 *   64px 高度让用户能扫一眼就看到"哪些已授权、哪些还要去系统设置打开"。
 *   逐项请求期间按钮需禁用并 aria-busy，避免重复弹系统授权框。
 *
 * Code Logic（这个组件做什么）:
 *   - 64px 固定高度，16px 内边距，整体不响应 hover（静态信息）
 *   - 左侧 32x32 容器承载 icon（surface-warm 背景 + accent 文字色）
 *   - 中间标题 --text-md --weight-semibold + 描述 --text-sm --muted
 *   - 右侧根据 granted 切换：true → success Pill "已授权"；false → primary Button
 *   - requesting 时按钮 loading/disabled + aria-busy，文案切到「授权中」
 */

import { memo, useCallback, type CSSProperties, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import { CheckIcon, ArrowRightIcon } from '@/lib/icons';
import styles from './PermissionCard.module.css';

export interface PermissionCardProps {
  /** 大图标，预期是 16-24px 的 SVG/Icon 节点；卡片会把它居中放在 32x32 容器里 */
  icon: ReactNode;
  /** 权限名称，例如"屏幕录制" */
  title: string;
  /** 权限说明，1-2 行即可 */
  description: string;
  /** 是否已授权 */
  granted: boolean;
  /** 该项是否正在请求授权（禁用按钮并 aria-busy） */
  requesting?: boolean;
  /** 点击"去设置"按钮时触发，由父级决定是打开系统设置还是重新检查授权 */
  onRequestAccess?: () => void;
  className?: string;
  style?: CSSProperties;
}

/**
 * 渲染权限引导卡片
 *
 * @param props PermissionCardProps
 * @returns 64px 高的静态信息卡片
 */
function PermissionCardInner({
  icon,
  title,
  description,
  granted,
  requesting = false,
  onRequestAccess,
  className,
  style,
}: PermissionCardProps) {
  const { t } = useTranslation(['welcome']);
  const handleClick = useCallback(() => {
    if (requesting) return;
    onRequestAccess?.();
  }, [onRequestAccess, requesting]);

  const cardClasses = [styles.card, className].filter(Boolean).join(' ');

  return (
    <div
      className={cardClasses}
      style={style}
      data-granted={granted}
      data-requesting={requesting || undefined}
      aria-busy={requesting || undefined}
    >
      <div className={styles.iconBox} aria-hidden="true">
        {icon}
      </div>
      <div className={styles.content}>
        <div className={styles.title}>{title}</div>
        <div className={styles.description}>{description}</div>
      </div>
      <div className={styles.action}>
        {granted ? (
          <Pill tone="success" dot className={styles.statusPill}>
            <CheckIcon size={12} />
            <span>{t('welcome:permissionCard.granted')}</span>
          </Pill>
        ) : (
          <Button
            variant="primary"
            size="sm"
            onClick={handleClick}
            loading={requesting}
            disabled={requesting}
            aria-busy={requesting}
            iconRight={requesting ? undefined : <ArrowRightIcon />}
            className={styles.actionButton}
          >
            {requesting
              ? t('welcome:permissionCard.requesting')
              : t('welcome:permissionCard.goSettings')}
          </Button>
        )}
      </div>
    </div>
  );
}

export const PermissionCard = memo(PermissionCardInner);
PermissionCard.displayName = 'PermissionCard';
