/**
 * BatteryCreditToast — 入账 +Xm，挂在 AppShell 层。
 *
 * Business Logic（为什么需要这个组件）:
 *   充电动画不能被工作台遮罩挡住，必须在侧栏 footer 附近。
 *
 * Code Logic（这个组件做什么）:
 *   固定贴侧栏底部上方；有 source 时用 creditToastSource。
 */

import { useEffect, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';

import type { BatteryCreditToast as ToastModel } from '@/hooks/useBattery';
import styles from './BatteryCreditToast.module.css';

export interface BatteryCreditToastProps {
  toast: ToastModel | null;
  onDismiss: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户完成健康/闪卡后需要立刻看到充了多少。
 *
 * Code Logic（这个组件做什么）:
 *   1.6s 后自动消失；reduced-motion 不播动画。
 */
export function BatteryCreditToast({ toast, onDismiss }: BatteryCreditToastProps): ReactElement | null {
  const { t } = useTranslation('battery');

  useEffect(() => {
    if (!toast) return undefined;
    const id = window.setTimeout(onDismiss, 1600);
    return () => window.clearTimeout(id);
  }, [toast, onDismiss]);

  if (!toast) return null;
  const sourceLabel = toast.source ? t(`sources.${toast.source}`) : '';
  const text = toast.source
    ? t('creditToastSource', { minutes: toast.minutes, source: sourceLabel })
    : t('creditToast', { minutes: toast.minutes });

  return (
    <div className={styles.toast} data-testid="battery-credit-toast" role="status">
      {text}
    </div>
  );
}
