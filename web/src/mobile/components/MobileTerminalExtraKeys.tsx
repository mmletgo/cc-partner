import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  getMobileTerminalExtraKeys,
  type MobileTerminalExtraKeyDef,
  type MobileTerminalStickyModifier,
} from '../mobileTerminalExtraKeys';
import styles from '../MobileWorkbench.module.css';
import { PointerPrimaryButton } from './PointerPrimaryButton';

export interface MobileTerminalExtraKeysProps {
  disabled: boolean;
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
 *   历史上键位拆成两页并通过末端 `1`/`2` 翻页按钮切换，键条容器本身已是横向滚动；
 *   现在所有键合并为单一扁平序列，容器加 scroll-snap 让滑动自动对齐按键起始位置，
 *   用户左右滑动即可浏览全部键，不再需要翻页按钮。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：渲染所有键（按扁平顺序）于横向可滚动容器，modifier 显示 aria-pressed；
 *   pointerdown 经 PointerPrimaryButton 触发 onKeyPress 并 preventDefault 阻止按钮抢焦，
 *   焦点保持在终端 helper textarea，软键盘自然不收；点击结果交给父组件解析并 enqueue。
 */
export function MobileTerminalExtraKeys({
  disabled,
  stickyModifier,
  onKeyPress,
}: MobileTerminalExtraKeysProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const keys = getMobileTerminalExtraKeys();

  return (
    <div
      className={styles.mobileTerminalExtraKeys}
      role="toolbar"
      aria-label={t('workbench:mobile.terminalPanel.extraKeys.toolbarAriaLabel')}
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
