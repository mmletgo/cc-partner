import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent, ReactElement } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  armExtraKeyPopup,
  beginExtraKeyPopupPress,
  cancelExtraKeyPopupPress,
  extraKeyHasPopup,
  EXTRA_KEY_POPUP_TRIGGER_HIT_ID,
  getMobileTerminalExtraKeys,
  hitTestExtraKeyPopup,
  hoverExtraKeyPopup,
  IDLE_EXTRA_KEY_POPUP_PRESS,
  MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS,
  resolveExtraKeyPopupPointerUp,
  selectExtraKeyPopupItem,
  type ExtraKeyPopupHitRect,
  type ExtraKeyPopupPressSession,
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
    case 'slashRewind':
      return t('workbench:mobile.terminalPanel.extraKeys.slashRewind');
    case 'slashResume':
      return t('workbench:mobile.terminalPanel.extraKeys.slashResume');
    case 'slashCompact':
      return t('workbench:mobile.terminalPanel.extraKeys.slashCompact');
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

interface ExtraKeyPopupButtonProps {
  keyDef: MobileTerminalExtraKeyDef;
  disabled: boolean;
  ariaLabel: string;
  popupAriaLabel: string;
  itemAriaLabel: (item: MobileTerminalExtraKeyDef) => string;
  onKeyPress: (key: MobileTerminalExtraKeyDef) => void;
}

/**
 * ExtraKeyPopupButton（带长按弹出层的 extra key）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/` 短按插入斜杠；长按后在键上方滑选 `/rewind` `/resume` `/compact`。
 *   必须与 PointerPrimaryButton 的「按下即发」分开，否则长按无法成立。
 *
 * Code Logic（这个组件做什么）:
 *   pointerDown 只进入 pending 并启动 400ms 定时器；超时后 portal 弹出层到 body；
 *   move 用坐标 hit-test 更新 hover；up 按纯函数会话发送或取消；click 仅作键盘兜底。
 */
function ExtraKeyPopupButton({
  keyDef,
  disabled,
  ariaLabel,
  popupAriaLabel,
  itemAriaLabel,
  onKeyPress,
}: ExtraKeyPopupButtonProps): ReactElement {
  const [session, setSession] = useState<ExtraKeyPopupPressSession>(IDLE_EXTRA_KEY_POPUP_PRESS);
  const sessionRef = useRef<ExtraKeyPopupPressSession>(IDLE_EXTRA_KEY_POPUP_PRESS);
  const timerRef = useRef<number | null>(null);
  const handledByPointerRef = useRef(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const popupRef = useRef<HTMLDivElement | null>(null);
  const itemElsRef = useRef<Map<string, HTMLElement>>(new Map());

  /**
   * Business Logic（为什么需要这个函数）:
   *   短按抬手、取消、卸载都必须清掉长按定时器，否则会在松手后误打开弹出层。
   *
   * Code Logic（这个函数做什么）:
   *   若存在 timeout id 则 clearTimeout 并置空。
   */
  function clearLongPressTimer(): void {
    if (timerRef.current == null) return;
    window.clearTimeout(timerRef.current);
    timerRef.current = null;
  }

  useEffect(() => {
    return () => {
      clearLongPressTimer();
    };
  }, []);

  useLayoutEffect(() => {
    const popup = popupRef.current;
    const trigger = triggerRef.current;
    if (!popup || !trigger || session.phase !== 'open') return;
    const rect = trigger.getBoundingClientRect();
    const gap = 4;
    const margin = 8;
    const popupWidth = popup.getBoundingClientRect().width;
    const left = Math.min(
      Math.max(rect.left, margin),
      Math.max(margin, window.innerWidth - popupWidth - margin),
    );
    popup.style.left = `${left}px`;
    popup.style.bottom = `${window.innerHeight - rect.top + gap}px`;
    popup.style.visibility = 'visible';
  }, [session.phase]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   弹出层 pointer-events:none，命中必须用当前 DOM 矩形。
   *
   * Code Logic（这个函数做什么）:
   *   先收集 popup 项矩形，再追加 trigger，保证重叠时项优先。
   */
  function collectHitRegions(): ExtraKeyPopupHitRect[] {
    const regions: ExtraKeyPopupHitRect[] = [];
    for (const [id, el] of itemElsRef.current) {
      const rect = el.getBoundingClientRect();
      regions.push({
        id,
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
      });
    }
    const trigger = triggerRef.current;
    if (trigger) {
      const rect = trigger.getBoundingClientRect();
      regions.push({
        id: EXTRA_KEY_POPUP_TRIGGER_HIT_ID,
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
      });
    }
    return regions;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   松手时根据会话决定发送主键、弹出项或取消。
   *
   * Code Logic（这个函数做什么）:
   *   清定时器、回到 idle；send=false 时一律取消；否则 resolve 后 onKeyPress。
   */
  function finishPress(current: ExtraKeyPopupPressSession, send: boolean): void {
    clearLongPressTimer();
    const idle = cancelExtraKeyPopupPress();
    sessionRef.current = idle;
    setSession(idle);
    if (!send) return;
    const result = resolveExtraKeyPopupPointerUp(current);
    if (result.type !== 'send') return;
    const selected = selectExtraKeyPopupItem(keyDef, result.hitId);
    if (selected) onKeyPress(selected);
  }

  const popupOpen = session.phase === 'open';

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={styles.mobileTerminalExtraKey}
        data-key-id={keyDef.id}
        data-kind={keyDef.kind}
        data-has-popup="true"
        data-popup-open={popupOpen || undefined}
        aria-label={ariaLabel}
        aria-haspopup="menu"
        aria-expanded={popupOpen}
        title={ariaLabel}
        disabled={disabled}
        onPointerDown={(event: ReactPointerEvent<HTMLButtonElement>) => {
          event.preventDefault();
          if (disabled) return;
          handledByPointerRef.current = true;
          try {
            event.currentTarget.setPointerCapture(event.pointerId);
          } catch {
            // jsdom 或宿主不支持 capture 时仍继续手势。
          }
          clearLongPressTimer();
          const pending = beginExtraKeyPopupPress(keyDef.id);
          sessionRef.current = pending;
          setSession(pending);
          timerRef.current = window.setTimeout(() => {
            const opened = armExtraKeyPopup(sessionRef.current);
            sessionRef.current = opened;
            setSession(opened);
          }, MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS);
        }}
        onPointerMove={(event: ReactPointerEvent<HTMLButtonElement>) => {
          const current = sessionRef.current;
          if (current.phase !== 'open') return;
          const hit = hitTestExtraKeyPopup(event.clientX, event.clientY, collectHitRegions());
          const next = hoverExtraKeyPopup(current, hit);
          sessionRef.current = next;
          setSession(next);
        }}
        onPointerUp={(event: ReactPointerEvent<HTMLButtonElement>) => {
          event.preventDefault();
          finishPress(sessionRef.current, true);
        }}
        onPointerCancel={() => {
          finishPress(sessionRef.current, false);
        }}
        onClick={() => {
          if (handledByPointerRef.current) {
            handledByPointerRef.current = false;
            return;
          }
          if (disabled) return;
          onKeyPress(keyDef);
        }}
      >
        <span aria-hidden="true">{keyDef.label}</span>
      </button>
      {popupOpen
        ? createPortal(
            <div
              ref={popupRef}
              className={styles.mobileTerminalExtraKeyPopup}
              role="menu"
              aria-label={popupAriaLabel}
              style={{ visibility: 'hidden', left: 0, bottom: 0 }}
            >
              {(keyDef.popup ?? []).map((item) => {
                const itemLabel = itemAriaLabel(item);
                return (
                  <div
                    key={item.id}
                    role="menuitem"
                    className={styles.mobileTerminalExtraKeyPopupItem}
                    data-popup-item-id={item.id}
                    data-hover={session.hoverId === item.id || undefined}
                    aria-label={itemLabel}
                    ref={(el) => {
                      if (el) itemElsRef.current.set(item.id, el);
                      else itemElsRef.current.delete(item.id);
                    }}
                  >
                    <span aria-hidden="true">{item.label}</span>
                  </div>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
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
 *   无 popup 的键 pointerdown 经 PointerPrimaryButton 触发 onKeyPress；
 *   带 popup 的键走 ExtraKeyPopupButton 长按滑动；preventDefault 阻止按钮抢焦。
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
        if (extraKeyHasPopup(key)) {
          return (
            <ExtraKeyPopupButton
              key={key.id}
              keyDef={key}
              disabled={disabled}
              ariaLabel={ariaLabel}
              popupAriaLabel={t('workbench:mobile.terminalPanel.extraKeys.slashPopup')}
              itemAriaLabel={(item) => extraKeyAriaLabel(t, item)}
              onKeyPress={onKeyPress}
            />
          );
        }
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
