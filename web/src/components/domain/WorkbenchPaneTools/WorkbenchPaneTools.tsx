/**
 * WorkbenchPaneTools（终端窗格操作四合一菜单）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 终端导航行 actions 曾塞入 12 个工具按钮导致过载；按作用域重新划分后，
 *   右分屏/下分屏/切换窗格/关闭窗格四个 pane 级操作收敛为一个「窗格」菜单入口，
 *   终端工具栏只保留会话搜索、Prompt 工具、适应尺寸、窗格菜单和全屏。
 *
 * Code Logic（这个组件做什么）:
 *   渲染一个触发按钮（Button secondary sm + 响应式 label，与同行工具栏按钮一致），
 *   点击后弹出共享 Dialog（portal / focus trap / Escape / backdrop，复用 WorkbenchProjectRail
 *   的 sourcePopover 紧凑样式模式）；菜单内 4 行 = 图标 + 文字 label，保留原 aria-label 与
 *   i18n key（workbench:splitPaneRight / splitPaneDown / switchPane / closePane），
 *   点击执行动作并关闭菜单。禁用逻辑与原四个按钮一致：分屏/关闭 = !canUsePanes ||
 *   remoteWriteDisabled；切换 = !canSwitchPane || remoteWriteDisabled；全部禁用时触发按钮禁用。
 */

import { useCallback, useRef, useState, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog } from '@/components/primitives';
import { ArrowRightIcon, SplitDownIcon, SplitRightIcon, XIcon } from '@/lib/icons';
import styles from './WorkbenchPaneTools.module.css';

export interface WorkbenchPaneToolsProps {
  /** 当前会话是否支持 pane 操作（可分屏/可关闭） */
  canUsePanes: boolean;
  /** 是否存在可切换的相邻 pane */
  canSwitchPane: boolean;
  /** 远端项目离线等导致的写禁用 */
  remoteWriteDisabled: boolean;
  /** 分屏方向回调（页面侧包 void） */
  onSplitPane: (direction: 'right' | 'down') => void;
  /** 切换 pane 回调 */
  onSwitchPane: () => void;
  /** 关闭当前 pane 回调 */
  onClosePane: () => void;
}

/**
 * 渲染「窗格」菜单（触发按钮 + 弹出四操作列表）。
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench.tsx 在终端工具栏 actions slot 调用本组件，让四个 pane 操作共享一个入口，
 *   同时保持既有 aria-label/i18n key 不变，方便测试与用户肌肉记忆。
 *
 * Code Logic（这个函数做什么）:
 *   useState 管理菜单开合；Dialog portal 渲染 4 个动作行；每个动作点击后执行回调并关闭菜单，
 *   焦点交还触发按钮（与 WorkbenchProjectRail closeSourcePicker 模式一致）。
 */
export function WorkbenchPaneTools({
  canUsePanes,
  canSwitchPane,
  remoteWriteDisabled,
  onSplitPane,
  onSwitchPane,
  onClosePane,
}: WorkbenchPaneToolsProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [menuOpen, setMenuOpen] = useState<boolean>(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const splitDisabled = !canUsePanes || remoteWriteDisabled;
  const switchDisabled = !canSwitchPane || remoteWriteDisabled;
  const triggerDisabled = splitDisabled && switchDisabled;

  /**
   * 关闭窗格菜单并尝试把焦点还回触发按钮。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户执行完某个窗格动作或取消菜单后，应回到触发入口，便于继续操作（与 ProjectRail 一致）。
   *
   * Code Logic（这个函数做什么）:
   *   setMenuOpen(false)；下一帧 focus 触发按钮。
   */
  const closeMenu = useCallback(() => {
    setMenuOpen(false);
    window.setTimeout(() => triggerRef.current?.focus(), 0);
  }, []);

  const actions = [
    {
      key: 'splitPaneRight',
      label: t('workbench:splitPaneRight'),
      icon: <SplitRightIcon />,
      disabled: splitDisabled,
      run: () => onSplitPane('right'),
    },
    {
      key: 'splitPaneDown',
      label: t('workbench:splitPaneDown'),
      icon: <SplitDownIcon />,
      disabled: splitDisabled,
      run: () => onSplitPane('down'),
    },
    {
      key: 'switchPane',
      label: t('workbench:switchPane'),
      icon: <ArrowRightIcon />,
      disabled: switchDisabled,
      run: onSwitchPane,
    },
    {
      key: 'closePane',
      label: t('workbench:closePane'),
      icon: <XIcon />,
      disabled: splitDisabled,
      run: onClosePane,
    },
  ];

  return (
    <>
      <Button
        ref={triggerRef}
        className={styles.trigger}
        variant="secondary"
        size="sm"
        icon={<SplitRightIcon />}
        title={t('workbench:paneTools.open')}
        aria-label={t('workbench:paneTools.open')}
        aria-haspopup="dialog"
        aria-expanded={menuOpen}
        data-workbench-responsive-action="true"
        disabled={triggerDisabled}
        onClick={() => setMenuOpen((open) => !open)}
      >
        <span data-workbench-responsive-label="true">{t('workbench:paneTools.open')}</span>
      </Button>
      <Dialog
        open={menuOpen}
        titleId="workbench-pane-tools-title"
        onClose={closeMenu}
        className={styles.paneMenu}
      >
        <h2 id="workbench-pane-tools-title" className="sr-only">
          {t('workbench:paneTools.open')}
        </h2>
        {actions.map((action) => (
          <button
            key={action.key}
            type="button"
            className={styles.paneOption}
            title={action.label}
            aria-label={action.label}
            disabled={action.disabled}
            onClick={() => {
              action.run();
              closeMenu();
            }}
          >
            {action.icon}
            <span>{action.label}</span>
          </button>
        ))}
      </Dialog>
    </>
  );
}
