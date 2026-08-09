import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  getMobileTerminalExtraKeys,
  type MobileTerminalExtraKeyDef,
  type MobileTerminalExtraKeyPage,
  type MobileTerminalStickyModifier,
} from '../mobileTerminalExtraKeys';
import styles from '../MobileWorkbench.module.css';
import { PointerPrimaryButton } from './PointerPrimaryButton';

export interface MobileTerminalExtraKeysProps {
  disabled: boolean;
  page: MobileTerminalExtraKeyPage;
  stickyModifier: MobileTerminalStickyModifier | null;
  onKeyPress: (key: MobileTerminalExtraKeyDef) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   extra key 的 ariaKey 是运行期字符串，i18next 严格 key 联合类型不接受模板字面量插值。
 *
 * Code Logic（这个函数做什么）:
 *   用闭集 switch 把 ariaKey 映射到编译期合法的 t() 路径，未知 key 回退到 label。
 */
function extraKeyAriaLabel(
  t: TFunction<['workbench']>,
  key: MobileTerminalExtraKeyDef,
): string {
  switch (key.ariaKey) {
    case 'esc':
      return t('workbench:mobile.terminalPanel.extraKeys.esc');
    case 'enter':
      return t('workbench:mobile.terminalPanel.extraKeys.enter');
    case 'tab':
      return t('workbench:mobile.terminalPanel.extraKeys.tab');
    case 'shiftTab':
      return t('workbench:mobile.terminalPanel.extraKeys.shiftTab');
    case 'ctrl':
      return t('workbench:mobile.terminalPanel.extraKeys.ctrl');
    case 'alt':
      return t('workbench:mobile.terminalPanel.extraKeys.alt');
    case 'slash':
      return t('workbench:mobile.terminalPanel.extraKeys.slash');
    case 'up':
      return t('workbench:mobile.terminalPanel.extraKeys.up');
    case 'down':
      return t('workbench:mobile.terminalPanel.extraKeys.down');
    case 'left':
      return t('workbench:mobile.terminalPanel.extraKeys.left');
    case 'right':
      return t('workbench:mobile.terminalPanel.extraKeys.right');
    case 'page1':
      return t('workbench:mobile.terminalPanel.extraKeys.page1');
    case 'page2':
      return t('workbench:mobile.terminalPanel.extraKeys.page2');
    case 'ctrlC':
      return t('workbench:mobile.terminalPanel.extraKeys.ctrlC');
    case 'ctrlD':
      return t('workbench:mobile.terminalPanel.extraKeys.ctrlD');
    case 'ctrlZ':
      return t('workbench:mobile.terminalPanel.extraKeys.ctrlZ');
    case 'ctrlL':
      return t('workbench:mobile.terminalPanel.extraKeys.ctrlL');
    case 'home':
      return t('workbench:mobile.terminalPanel.extraKeys.home');
    case 'end':
      return t('workbench:mobile.terminalPanel.extraKeys.end');
    case 'pageUp':
      return t('workbench:mobile.terminalPanel.extraKeys.pageUp');
    case 'pageDown':
      return t('workbench:mobile.terminalPanel.extraKeys.pageDown');
    case 'cdUp':
      return t('workbench:mobile.terminalPanel.extraKeys.cdUp');
    case 'lsLa':
      return t('workbench:mobile.terminalPanel.extraKeys.lsLa');
    case 'clearSnippet':
      return t('workbench:mobile.terminalPanel.extraKeys.clearSnippet');
    default:
      return key.label;
  }
}

/**
 * MobileTerminalExtraKeys（移动端终端额外按键条）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机软键盘缺少 Esc/Tab/Ctrl/方向与 ^C 等；需要在终端 surface 底部提供 Termux 风格固定键位条。
 *   按 extra key 只发送按键，不主动收起软键盘——避免在输入法/输入态下 blur 终端 helper textarea
 *   打乱 xterm 输入追踪（已输入内容被重复发送）。软键盘由用户点击终端外区域收起。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：按 page 渲染横向可滚动按钮；modifier 显示 aria-pressed；
 *   pointerdown 经 PointerPrimaryButton 触发 onKeyPress 并 preventDefault 阻止按钮抢焦，
 *   焦点保持在终端 helper textarea，软键盘自然不收；点击结果交给父组件解析并 enqueue。
 */
export function MobileTerminalExtraKeys({
  disabled,
  page,
  stickyModifier,
  onKeyPress,
}: MobileTerminalExtraKeysProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const keys = getMobileTerminalExtraKeys(page);

  return (
    <div
      className={styles.mobileTerminalExtraKeys}
      role="toolbar"
      aria-label={t('workbench:mobile.terminalPanel.extraKeys.toolbarAriaLabel')}
      data-page={page}
    >
      {keys.map((key) => {
        const pressed =
          key.kind === 'modifier' && key.modifier != null && key.modifier === stickyModifier;
        const ariaLabel = extraKeyAriaLabel(t, key);
        return (
          <PointerPrimaryButton
            key={key.id}
            type="button"
            className={styles.mobileTerminalExtraKey}
            data-key-id={key.id}
            data-kind={key.kind}
            data-pressed={pressed || undefined}
            aria-label={ariaLabel}
            aria-pressed={key.kind === 'modifier' ? pressed : undefined}
            title={ariaLabel}
            disabled={disabled}
            onPrimary={() => onKeyPress(key)}
          >
            <span aria-hidden="true">{key.label}</span>
          </PointerPrimaryButton>
        );
      })}
    </div>
  );
}
