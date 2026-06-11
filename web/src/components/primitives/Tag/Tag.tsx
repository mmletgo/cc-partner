/**
 * Tag 原子组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Prompt 管理、设备标签、过滤条件等场景需要展示简短分类标识（chip 形态），
 *   Tag 提供 5 种颜色 + 2 种尺寸 + 可选关闭按钮，统一品牌视觉。
 *
 * Code Logic（这个组件做什么）:
 *   渲染一个 chip 形内联元素；color 决定背景与文字颜色，danger/success/warn 走 12% 透明软色；
 *   onClose 存在时显示 X 图标，点击触发回调；事件冒泡被阻止以避免误触父级 onClick。
 */

import type { HTMLAttributes, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { XIcon } from '@/lib/icons';
import styles from './Tag.module.css';

export type TagColor = 'default' | 'accent' | 'success' | 'warn' | 'danger';
export type TagSize = 'sm' | 'md';

export interface TagProps extends HTMLAttributes<HTMLSpanElement> {
  /** chip 文本 */
  children: ReactNode;
  /** 颜色变体 */
  color?: TagColor;
  /** 尺寸，sm 20px / md 24px */
  size?: TagSize;
  /** 设置后显示关闭按钮并接收点击事件 */
  onClose?: (e: React.MouseEvent<HTMLButtonElement>) => void;
  className?: string;
}

/**
 * 渲染统一 Tag
 *
 * @param props Tag 属性
 * @returns <span> 元素
 */
export function Tag(props: TagProps) {
  const { t } = useTranslation(['common']);
  const { children, color = 'default', size = 'md', onClose, className, onClick, ...rest } = props;

  const classes = [styles.tag, styles[`color-${color}`], styles[`size-${size}`], className]
    .filter(Boolean)
    .join(' ');

  /**
   * 关闭按钮点击：阻止冒泡避免触发外部 onClick（如列表项选中）
   *
   * @param e React 鼠标事件
   */
  const handleClose = (e: React.MouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
    onClose?.(e);
  };

  return (
    <span data-color={color} data-size={size} className={classes} onClick={onClick} {...rest}>
      <span>{children}</span>
      {onClose ? (
        <button type="button" className={styles.close} onClick={handleClose} aria-label={t('common:tag.removeLabel')}>
          <XIcon size={10} />
        </button>
      ) : null}
    </span>
  );
}

Tag.displayName = 'Tag';
