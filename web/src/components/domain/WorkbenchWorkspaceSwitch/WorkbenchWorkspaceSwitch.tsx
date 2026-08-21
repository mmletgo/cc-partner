/**
 * WorkbenchWorkspaceSwitch（工作区三元切换）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 工作台有三个并列工作区：终端 / 网页浏览 / 文件浏览。原先这两个浏览入口以独立
 *   Button 形式挤在标题行右侧，与"返回终端"按钮并存时用户需要猜测谁负责哪个方向；终端全屏时
 *   标题行整块隐藏导致全屏用户根本无法切到浏览器/文件。新组件把三个工作区压成一个 segmented
 *   control，单选语义与值由父级 `workspaceView` 驱动，与三处工作区层叠渲染对齐；同时保留
 *   在终端全屏态下渲染，因为切到网页浏览/文件浏览正是离开全屏的合法路径。
 *
 * Code Logic（这个组件做什么）:
 *   接收 `value` / `onChange` 与三个 `{id, label, icon}` 选项，渲染 `role="radiogroup"` 三段
 *   控件。激活项走 accent 背景，非激活走 surface + meta 文字；禁用项 `aria-disabled` 并阻断
 *   click；每段图标 16px + 文字，窄宽时响应式隐藏文字（保留图标 + `title`/`aria-label`），与
 *   现有 `data-workbench-responsive-label` 合同保持一致。组件不感知业务数据。
 */

import type { ReactElement, ReactNode } from 'react';
import styles from './WorkbenchWorkspaceSwitch.module.css';

export type WorkbenchWorkspaceSwitchValue = 'terminal' | 'browser' | 'files';

export interface WorkbenchWorkspaceSwitchOption {
  /** 工作区 id（与父级 workspaceView 字面量一致）。 */
  id: WorkbenchWorkspaceSwitchValue;
  /** 用户可见文案（必须走 i18n）。 */
  label: string;
  /** 16px 图标；通常取自 `lib/icons.tsx`。 */
  icon: ReactNode;
  /** 当条件不满足时禁用该段（如网页浏览无 project/worktree）。文件浏览默认不禁用。 */
  disabled?: boolean;
}

export interface WorkbenchWorkspaceSwitchProps {
  /** 当前选中的工作区。 */
  value: WorkbenchWorkspaceSwitchValue;
  /** 用户点击非禁用段时回调；禁用段不会触发。 */
  onChange: (value: WorkbenchWorkspaceSwitchValue) => void;
  /** 2 或 3 段选项；网页浏览关闭时只有终端 / 文件。顺序即视觉从左到右。 */
  options: readonly WorkbenchWorkspaceSwitchOption[];
  /** radiogroup 可访问名称；父级传 i18n 后。 */
  ariaLabel: string;
  /** 整组额外 className；用于父级控制边距。 */
  className?: string;
}

/**
 * 渲染工作区三元 segmented control。
 *
 * Business Logic（为什么需要这个函数）:
 *   父级（Workbench 标题行）需要在任何状态下把当前 `workspaceView` 暴露为单选控件，且允许
 *   browser 在条件不满足时呈灰禁用态而不被点击。文件浏览默认保持可点，以便进入空白页。
 *
 * Code Logic（这个函数做什么）:
 *   三段 `<button type="button" role="radio">` 包在一个 `<div role="radiogroup">` 内；每段点击
 *   调 `onChange(option.id)`；选中项 `aria-checked` 并加 `data-active`；禁用项 `aria-disabled`
 *   并加 `data-disabled`，onClick 早期 return。键盘 ArrowLeft/Right 在三段间循环切换（roving）。
 */
export function WorkbenchWorkspaceSwitch(props: WorkbenchWorkspaceSwitchProps): ReactElement {
  const { value, onChange, options, ariaLabel, className } = props;

  const handleSelect = (option: WorkbenchWorkspaceSwitchOption): void => {
    if (option.disabled) return;
    onChange(option.id);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    const key = event.key;
    if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
      return;
    }
    event.preventDefault();
    const currentIndex = options.findIndex((option) => option.id === value);
    if (currentIndex < 0) return;
    let nextIndex = currentIndex;
    if (key === 'ArrowLeft') {
      nextIndex = (currentIndex - 1 + options.length) % options.length;
    } else if (key === 'ArrowRight') {
      nextIndex = (currentIndex + 1) % options.length;
    } else if (key === 'Home') {
      nextIndex = 0;
    } else if (key === 'End') {
      nextIndex = options.length - 1;
    }
    const next = options[nextIndex];
    if (!next || next.disabled) return;
    handleSelect(next);
  };

  const groupClass = [styles.group, className].filter(Boolean).join(' ');

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className={groupClass}
      data-workbench-responsive-action="true"
      onKeyDown={handleKeyDown}
    >
      {options.map((option) => {
        const active = option.id === value;
        const itemClass = [
          styles.option,
          active ? styles['option--active'] : null,
          option.disabled ? styles['option--disabled'] : null,
        ]
          .filter(Boolean)
          .join(' ');
        return (
          <button
            key={option.id}
            type="button"
            role="radio"
            aria-checked={active}
            aria-disabled={option.disabled || undefined}
            aria-label={option.label}
            title={option.label}
            tabIndex={active ? 0 : -1}
            data-active={active || undefined}
            data-disabled={option.disabled || undefined}
            disabled={option.disabled}
            className={itemClass}
            onClick={() => {
              handleSelect(option);
            }}
          >
            <span className={styles.optionIcon} aria-hidden="true">
              {option.icon}
            </span>
            <span data-workbench-responsive-label="true" className={styles.optionLabel}>
              {option.label}
            </span>
          </button>
        );
      })}
    </div>
  );
}