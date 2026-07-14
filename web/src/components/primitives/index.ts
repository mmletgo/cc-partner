/**
 * Primitives 原子组件统一导出
 *
 * Business Logic（为什么需要这个入口）:
 *   业务组件（domain/layout）只需 `import { Button, Card, Input, Dialog } from '@/components/primitives'`
 *   即可使用所有原子组件，避免拼装多个 import 路径。
 *
 * Code Logic（这个入口做什么）:
 *   统一 re-export 原子组件及其类型（含 Dialog/Drawer/useModalLayer），供上层按需 import。
 */

export { Button } from './Button';
export type { ButtonProps, ButtonVariant, ButtonSize } from './Button';

export { Card } from './Card';
export type {
  CardProps,
  CardHeaderProps,
  CardBodyProps,
  CardFooterProps,
  CardVariant,
  CardPadding,
} from './Card';

export { Input } from './Input';
export type { InputProps, InputType, InputSize } from './Input';

export { Tag } from './Tag';
export type { TagProps, TagColor, TagSize } from './Tag';

export { Pill } from './Pill';
export type { PillProps, PillTone } from './Pill';

export { StatusDot } from './StatusDot';
export type { StatusDotProps, StatusDotStatus, StatusDotSize } from './StatusDot';

export { ProgressBar } from './ProgressBar';
export type { ProgressBarProps, ProgressBarSize, ProgressBarTone } from './ProgressBar';

export { Dialog, useModalLayer, getFocusableElements } from './Dialog';
export type { DialogProps, ModalLayerOptions } from './Dialog';

export { Drawer } from './Drawer';
export type { DrawerProps } from './Drawer';
