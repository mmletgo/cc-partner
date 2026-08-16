import { useCallback, useRef } from 'react';
import type { KeyboardEvent, ReactElement } from 'react';
import type { TFunction } from 'i18next';
import type { WorkbenchWorktree } from '@/lib/types';
import { worktreeStatusTone } from '@/pages/Workbench/workbenchWorktrees';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreeTabsProps {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  /** 整行禁用（busy 时父级传入）；不影响 chip 渲染但禁用点击 */
  busy?: boolean;
  /**
   * dirty-guard 入口（父级 `handleSelectWorktree` 已守护 busy + Files dirty snapshot）。
   * 返回 `false` 时保持当前 active 不切换（由父级保证），新组件不乐观切换。
   */
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  t: TFunction<'workbench'>;
}

/**
 * MobileWorktreeTabs（移动端 worktree 工作区 tab 列表）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端用户需要在 terminal panel 内直接切换 worktree 工作区，与桌面 `WorkbenchWorktreeBar` 行为一致；
 *   把 worktree 切换入口从 shell 顶部 statusRow 下移到 panel 内部，session tabs 上方。
 *
 * Code Logic（这个组件做什么）:
 *   渲染横向滚动 nav 容器，按 `worktreeStatusTone` 三档（neutral/warning/danger）映射
 *   `data-tone`；active chip 用 `data-active` + `aria-current="page"`；busy 时整行 disabled；
 *   `worktrees.length === 0` 时返回 null（min-height:0 自然收起）；键盘 ArrowLeft/Right/Home/End
 *   在 chip 间循环切换（与桌面 `WorkbenchSessionTabs` 风格一致）。tone 用 worktreeStatusTone
 *   而非 `getMobileWorktreeStatusKind`，两者语义不同（clean|dirty|conflict vs neutral|warning|danger）。
 */
export function MobileWorktreeTabs({
  worktrees,
  activeWorktreeId,
  busy = false,
  onSelect,
  t,
}: MobileWorktreeTabsProps): ReactElement | null {
  const navRef = useRef<HTMLElement | null>(null);
  const title = t('mobile.worktreeTabs.title');

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面 `WorkbenchSessionTabs` 用 roving tablist 风格让键盘用户快速切 chip；
   *   mobile 主交互是 touch，但保持键盘可达性避免退化。
   *
   * Code Logic（这个函数做什么）:
   *   ArrowLeft/Right/Home/End 循环切换 focus；其它键放行；preventDefault 避免滚动外层。
   */
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number): void => {
      const key = event.key;
      if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
        return;
      }
      if (worktrees.length === 0) return;
      event.preventDefault();
      const last = worktrees.length - 1;
      let nextIndex: number;
      if (key === 'Home') nextIndex = 0;
      else if (key === 'End') nextIndex = last;
      else if (key === 'ArrowLeft')
        nextIndex = currentIndex <= 0 ? last : currentIndex - 1;
      else nextIndex = currentIndex >= last ? 0 : currentIndex + 1;
      const nextChip = navRef.current?.querySelectorAll<HTMLButtonElement>(
        'button[data-mobile-worktree-chip]',
      )[nextIndex];
      nextChip?.focus();
    },
    [worktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   onSelect 返回 false 时父级拒绝切换（dirty guard 失败 / busy 拒绝）；
   *   新组件不持有 active 状态、不乐观切换，因此无需额外处理返回值。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时忽略点击；否则透传到父级 onSelect；返回值不消费。
   */
  const handleClick = useCallback(
    (worktree: WorkbenchWorktree): void => {
      if (busy) return;
      onSelect(worktree);
    },
    [busy, onSelect],
  );

  if (worktrees.length === 0) return null;

  return (
    <nav
      ref={navRef}
      className={styles.mobileWorktreeStrip}
      aria-label={title}
      data-busy={busy || undefined}
      data-testid="mobile-worktree-tabs"
    >
      {worktrees.map((worktree, index) => {
        const tone = worktreeStatusTone(worktree);
        const isActive = worktree.id === activeWorktreeId;
        const label = worktree.branch ?? worktree.name;
        return (
          <button
            key={worktree.id}
            type="button"
            data-mobile-worktree-chip
            data-active={isActive || undefined}
            data-tone={tone}
            aria-current={isActive ? 'page' : undefined}
            disabled={busy}
            className={styles.mobileWorktreeChip}
            onClick={() => handleClick(worktree)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            <span className={styles.mobileWorktreeDot} data-tone={tone} aria-hidden="true" />
            <span className={styles.mobileWorktreeChipName} title={label}>
              {label}
            </span>
          </button>
        );
      })}
    </nav>
  );
}