/**
 * Workbench 工作区切换槽：终端 / 网页浏览 / 文件浏览。
 *
 * Business Logic（为什么需要）:
 *   网页浏览是内测功能，默认关闭；开关关闭时工作台不得露出第三段，已打开的浏览层要退回终端。
 *
 * Code Logic（做什么）:
 *   按 browserEnabled 组装 2 或 3 段选项；关闭时若当前值是 browser 则 onChange('terminal')。
 */
import { useEffect, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  WorkbenchWorkspaceSwitch,
  type WorkbenchWorkspaceSwitchOption,
  type WorkbenchWorkspaceSwitchValue,
} from '@/components/domain/WorkbenchWorkspaceSwitch';
import { BrowserIcon, FileIcon, TerminalIcon } from '@/lib/icons';

export interface WorkbenchWorkspaceSwitchSlotProps {
  value: WorkbenchWorkspaceSwitchValue;
  onChange: (next: WorkbenchWorkspaceSwitchValue) => void;
  browserEnabled: boolean;
  canOpenBrowser: boolean;
}

/**
 * Business Logic: 标题行工作区切换必须跟内测开关同步，不能只靠禁用第三段。
 * Code Logic: 组装 options；browser 关闭且当前为 browser 时切回 terminal。
 */
export function WorkbenchWorkspaceSwitchSlot(
  props: WorkbenchWorkspaceSwitchSlotProps,
): ReactElement {
  const { value, onChange, browserEnabled, canOpenBrowser } = props;
  const { t } = useTranslation(['workbench']);

  useEffect(() => {
    if (!browserEnabled && value === 'browser') {
      onChange('terminal');
    }
  }, [browserEnabled, onChange, value]);

  const options: WorkbenchWorkspaceSwitchOption[] = [
    {
      id: 'terminal',
      label: t('workbench:workspaceSwitch.terminal'),
      icon: <TerminalIcon />,
    },
    ...(browserEnabled
      ? [
          {
            id: 'browser' as const,
            label: t('workbench:browserPreview.openWorkspace'),
            icon: <BrowserIcon />,
            disabled: !canOpenBrowser,
          },
        ]
      : []),
    {
      id: 'files',
      label: t('workbench:fileWorkspace.openFiles'),
      icon: <FileIcon />,
    },
  ];

  return (
    <WorkbenchWorkspaceSwitch
      ariaLabel={t('workbench:workspaceSwitch.ariaLabel')}
      value={value}
      onChange={onChange}
      options={options}
    />
  );
}
