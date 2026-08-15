/**
 * 移动端终端 Extra Keys（Termux 风格）纯逻辑。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机软键盘缺少 Esc/Tab/Ctrl/方向与 ^C 等组合；需要固定内置键位表、PTY 序列编码与 sticky 修饰键规则，
 *   且与 UI 解耦以便单测。
 *
 * Code Logic（这个模块做什么）:
 *   导出分页键位定义、payload 编码、sticky 变换与武装超时常量；不触碰 DOM / WebSocket。
 */

export type MobileTerminalExtraKeyPage = 1 | 2;

export type MobileTerminalStickyModifier = 'ctrl' | 'alt';

export type MobileTerminalExtraKeyKind = 'payload' | 'modifier' | 'page';

export interface MobileTerminalExtraKeyDef {
  /** 稳定 id，用于 React key / 测试。 */
  id: string;
  kind: MobileTerminalExtraKeyKind;
  /** 键帽短标签（ASCII）；完整无障碍名走 i18n。 */
  label: string;
  /** i18n key 后缀：`workbench:mobile.terminalPanel.extraKeys.<ariaKey>` */
  ariaKey: string;
  /** kind=payload 时写入 PTY 的字节/文本。 */
  payload?: string;
  /** kind=modifier */
  modifier?: MobileTerminalStickyModifier;
  /** kind=page */
  targetPage?: MobileTerminalExtraKeyPage;
}

/** sticky 武装后无后续输入的自动解除时长（毫秒）。 */
export const MOBILE_TERMINAL_STICKY_TIMEOUT_MS = 3000;

const CSI = '\x1b[';

/**
 * Business Logic（为什么需要这些序列）:
 *   mobile 输入通道只接受字符串帧；extra keys 必须直接给出 PTY 可识别的控制序列或字面文本。
 *
 * Code Logic（编码约定）:
 *   方向/导航使用常见 xterm normal-mode CSI；Ctrl 字母用 ASCII 控制码；snippet 不含 CR。
 */
export const MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS = {
  esc: '\x1b',
  tab: '\t',
  // Shift+Tab 的标准 xterm 反向制表序列（CSI Z）；Claude Code 用它切换模式 / 补全列表反向选择。
  shiftTab: `${CSI}Z`,
  // 终端 Enter 发送 CR（回车）；Claude Code 用它确认 / 发送消息。
  enter: '\r',
  slash: '/',
  up: `${CSI}A`,
  down: `${CSI}B`,
  right: `${CSI}C`,
  left: `${CSI}D`,
  home: `${CSI}H`,
  end: `${CSI}F`,
  pageUp: `${CSI}5~`,
  pageDown: `${CSI}6~`,
  ctrlC: '\x03',
  ctrlD: '\x04',
  ctrlZ: '\x1a',
  ctrlL: '\x0c',
  cdUp: 'cd ..',
  lsLa: 'ls -la',
  clear: 'clear',
} as const;

const PAGE_1_KEYS: MobileTerminalExtraKeyDef[] = [
  { id: 'esc', kind: 'payload', label: 'Esc', ariaKey: 'esc', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.esc },
  {
    id: 'shift-tab',
    kind: 'payload',
    label: '⇧Tab',
    ariaKey: 'shiftTab',
    payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.shiftTab,
  },
  { id: 'slash', kind: 'payload', label: '/', ariaKey: 'slash', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slash },
  { id: 'up', kind: 'payload', label: '↑', ariaKey: 'up', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.up },
  { id: 'down', kind: 'payload', label: '↓', ariaKey: 'down', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.down },
  { id: 'left', kind: 'payload', label: '←', ariaKey: 'left', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.left },
  { id: 'right', kind: 'payload', label: '→', ariaKey: 'right', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.right },
  { id: 'enter', kind: 'payload', label: '⏎', ariaKey: 'enter', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.enter },
  { id: 'page-2', kind: 'page', label: '2', ariaKey: 'page2', targetPage: 2 },
];

const PAGE_2_KEYS: MobileTerminalExtraKeyDef[] = [
  { id: 'ctrl', kind: 'modifier', label: 'Ctrl', ariaKey: 'ctrl', modifier: 'ctrl' },
  { id: 'alt', kind: 'modifier', label: 'Alt', ariaKey: 'alt', modifier: 'alt' },
  { id: 'tab', kind: 'payload', label: 'Tab', ariaKey: 'tab', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.tab },
  { id: 'ctrl-c', kind: 'payload', label: '^C', ariaKey: 'ctrlC', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlC },
  { id: 'ctrl-d', kind: 'payload', label: '^D', ariaKey: 'ctrlD', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlD },
  { id: 'ctrl-z', kind: 'payload', label: '^Z', ariaKey: 'ctrlZ', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlZ },
  { id: 'ctrl-l', kind: 'payload', label: '^L', ariaKey: 'ctrlL', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlL },
  { id: 'home', kind: 'payload', label: 'Home', ariaKey: 'home', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.home },
  { id: 'end', kind: 'payload', label: 'End', ariaKey: 'end', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.end },
  {
    id: 'pgup',
    kind: 'payload',
    label: 'PgUp',
    ariaKey: 'pageUp',
    payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.pageUp,
  },
  {
    id: 'pgdn',
    kind: 'payload',
    label: 'PgDn',
    ariaKey: 'pageDown',
    payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.pageDown,
  },
  { id: 'cd-up', kind: 'payload', label: 'cd..', ariaKey: 'cdUp', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.cdUp },
  { id: 'ls-la', kind: 'payload', label: 'ls', ariaKey: 'lsLa', payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.lsLa },
  {
    id: 'clear-snippet',
    kind: 'payload',
    label: 'clr',
    ariaKey: 'clearSnippet',
    payload: MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.clear,
  },
  { id: 'page-1', kind: 'page', label: '1', ariaKey: 'page1', targetPage: 1 },
];

/**
 * Business Logic（为什么需要这个函数）:
 *   面板渲染与测试需要按页取出固定键位，且不得在运行期可变。
 *
 * Code Logic（这个函数做什么）:
 *   返回对应页键位定义数组的浅拷贝，避免调用方误改模块常量。
 */
export function getMobileTerminalExtraKeys(page: MobileTerminalExtraKeyPage): MobileTerminalExtraKeyDef[] {
  return page === 1 ? [...PAGE_1_KEYS] : [...PAGE_2_KEYS];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   sticky Ctrl 要求把下一字符编码为 ASCII 控制码（Termux / 终端惯例）。
 *
 * Code Logic（这个函数做什么）:
 *   仅处理单字符；字母不区分大小写；`@`..`_` 与 `?` 走经典 ctrl 映射；否则返回 null。
 */
export function encodeCtrlKeyInput(data: string): string | null {
  if (data.length !== 1) return null;
  const code = data.charCodeAt(0);
  // Ctrl+Space / Ctrl+@ → NUL
  if (code === 0x20 || code === 0x40) return '\x00';
  // Ctrl+[a-z] / Ctrl+[A-Z]
  if ((code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a)) {
    return String.fromCharCode(code & 0x1f);
  }
  // Ctrl+[ \ ] ^ _] already in 0x1c-0x1f when shifted; map raw `@`..`_` range
  if (code >= 0x5b && code <= 0x5f) {
    return String.fromCharCode(code & 0x1f);
  }
  // Ctrl+?
  if (data === '?') return '\x7f';
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   sticky Alt 在多数终端里等价于 ESC 前缀（meta）。
 *
 * Code Logic（这个函数做什么）:
 *   单字符前加 `\x1b`；多字符不变换（避免粘贴被改写）。
 */
export function encodeAltKeyInput(data: string): string | null {
  if (data.length !== 1) return null;
  return `\x1b${data}`;
}

export interface ApplyStickyModifierResult {
  /** 实际应写入 PTY 的数据。 */
  data: string;
  /** 是否应解除 sticky 武装。 */
  consume: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm onData 与 sticky 修饰键需要统一规则：能映射则改写并消耗，不能映射则原样发送且仍消耗一次武装，避免卡死。
 *
 * Code Logic（这个函数做什么）:
 *   modifier 为空则透传；ctrl/alt 对单字符尝试编码，失败则透传原 data；凡进入 sticky 分支均 consume=true。
 */
export function applyStickyModifierToInput(
  modifier: MobileTerminalStickyModifier | null,
  data: string,
): ApplyStickyModifierResult {
  if (!modifier) return { data, consume: false };
  if (modifier === 'ctrl') {
    const encoded = encodeCtrlKeyInput(data);
    return { data: encoded ?? data, consume: true };
  }
  const encoded = encodeAltKeyInput(data);
  return { data: encoded ?? data, consume: true };
}

export type MobileTerminalStickyToggleResult =
  | { type: 'arm'; modifier: MobileTerminalStickyModifier }
  | { type: 'disarm' };

/**
 * Business Logic（为什么需要这个函数）:
 *   Ctrl/Alt 互斥且再次点击同键应取消，符合 Termux sticky 习惯。
 *
 * Code Logic（这个函数做什么）:
 *   当前已武装同一修饰键 → disarm；否则 arm 为新修饰键（替换另一修饰键）。
 */
export function toggleStickyModifier(
  current: MobileTerminalStickyModifier | null,
  next: MobileTerminalStickyModifier,
): MobileTerminalStickyToggleResult {
  if (current === next) return { type: 'disarm' };
  return { type: 'arm', modifier: next };
}

export type ResolveExtraKeyPressResult =
  | { type: 'send'; data: string }
  | { type: 'toggleModifier'; modifier: MobileTerminalStickyModifier }
  | { type: 'setPage'; page: MobileTerminalExtraKeyPage }
  | { type: 'ignore' };

/**
 * Business Logic（为什么需要这个函数）:
 *   按钮点击需要纯函数结果，便于测试并让 UI 层只负责副作用（enqueue / setState）。
 *
 * Code Logic（这个函数做什么）:
 *   按 kind 分支：payload 需非空字符串；modifier/page 读定义字段；非法定义 ignore。
 */
export function resolveMobileTerminalExtraKeyPress(
  key: MobileTerminalExtraKeyDef,
): ResolveExtraKeyPressResult {
  if (key.kind === 'payload') {
    if (typeof key.payload !== 'string' || key.payload.length === 0) return { type: 'ignore' };
    return { type: 'send', data: key.payload };
  }
  if (key.kind === 'modifier') {
    if (key.modifier !== 'ctrl' && key.modifier !== 'alt') return { type: 'ignore' };
    return { type: 'toggleModifier', modifier: key.modifier };
  }
  if (key.kind === 'page') {
    if (key.targetPage !== 1 && key.targetPage !== 2) return { type: 'ignore' };
    return { type: 'setPage', page: key.targetPage };
  }
  return { type: 'ignore' };
}

/**
 * 可被 extra keys 解除软键盘的焦点目标（xterm helper textarea / 其它可编辑控件）。
 *
 * Business Logic（为什么需要这个类型）:
 *   单元测试不依赖 jsdom，只要能描述 tag/class 并接收 blur 副作用即可。
 *
 * Code Logic（这个类型做什么）:
 *   覆盖 document.activeElement 上 blur 所需最小字段。
 */
export interface SoftKeyboardFocusTarget {
  tagName: string;
  isContentEditable?: boolean;
  classList?: { contains(token: string): boolean };
  blur(): void;
}

/**
 * xterm helper textarea 的最小可测接口：属性读写 + blur。
 *
 * Business Logic（为什么需要这个类型）:
 *   iOS/Android 仅 blur 往往拦不住软键盘重现；需要 readonly + inputmode=none 显式离开输入态。
 *
 * Code Logic（这个类型做什么）:
 *   描述 setAttribute/removeAttribute/blur，便于无 jsdom 单测。
 */
export interface MobileTerminalHelperTextarea {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
  blur(): void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户一旦用系统键盘输入过，xterm helper textarea 会保持焦点；extra keys 若继续保留该焦点，
 *   手机软键盘会再次弹出，遮挡终端并打断快捷键操作。软键盘只应在用户点击终端输入区时出现。
 *
 * Code Logic（这个函数做什么）:
 *   若 activeElement 是 textarea/input/contentEditable 或带 xterm-helper-textarea class，则 blur 并返回 true；
 *   其它焦点目标不处理，避免误伤无关控件。
 */
export function dismissMobileTerminalSoftKeyboard(
  activeElement: SoftKeyboardFocusTarget | null | undefined,
): boolean {
  if (!activeElement || typeof activeElement.blur !== 'function') return false;
  const tag = activeElement.tagName.toUpperCase();
  const isEditable =
    tag === 'TEXTAREA' ||
    tag === 'INPUT' ||
    Boolean(activeElement.isContentEditable) ||
    Boolean(activeElement.classList?.contains('xterm-helper-textarea'));
  if (!isEditable) return false;
  activeElement.blur();
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   进入「可打字」态时才允许系统键盘：用户点击终端输入区后调用。
 *
 * Code Logic（这个函数做什么）:
 *   去掉 helper textarea 的 readonly 与 inputmode=none，返回是否找到控件。
 */
export function enterMobileTerminalTypingMode(
  helperTextarea: MobileTerminalHelperTextarea | null | undefined,
): boolean {
  if (!helperTextarea) return false;
  helperTextarea.removeAttribute('readonly');
  helperTextarea.removeAttribute('inputmode');
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   按 extra keys 或初始化终端时必须离开打字态：系统键盘不得因快捷键重现。
 *
 * Code Logic（这个函数做什么）:
 *   对 helper textarea 设 readonly + inputmode=none 并 blur；再 blur 当前可编辑 activeElement。
 *   返回是否至少执行了 helper 或 activeElement 其中一侧的 dismiss。
 */
export function leaveMobileTerminalTypingMode(
  helperTextarea: MobileTerminalHelperTextarea | null | undefined,
  activeElement?: SoftKeyboardFocusTarget | null,
): boolean {
  let touched = false;
  if (helperTextarea) {
    helperTextarea.setAttribute('readonly', 'true');
    helperTextarea.setAttribute('inputmode', 'none');
    helperTextarea.blur();
    touched = true;
  }
  if (dismissMobileTerminalSoftKeyboard(activeElement ?? null)) {
    touched = true;
  }
  return touched;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   面板与 extra keys 需要定位当前终端的 xterm helper textarea。
 *
 * Code Logic（这个函数做什么）:
 *   在 root 内 query `.xterm-helper-textarea`；root 为空则返回 null。
 */
export function findMobileTerminalHelperTextarea(
  root: ParentNode | null | undefined,
): HTMLTextAreaElement | null {
  if (!root || typeof (root as ParentNode).querySelector !== 'function') return null;
  return root.querySelector(
    'textarea.xterm-helper-textarea, .xterm-helper-textarea',
  ) as HTMLTextAreaElement | null;
}

/**
 * xterm helper textarea 的 value 读写面（提交后清空用）。
 *
 * Business Logic（为什么需要这个类型）:
 *   移动端中文 IME 提交后 xterm 6 往往不把 helper textarea 清成空串；残留内容会在下一次
 *   composition/input 时被再次 substring 发出，表现为「输入过中文括号后再次输入会重复旧内容」。
 *
 * Code Logic（这个类型做什么）:
 *   描述 value + 可选 setSelectionRange，便于无 jsdom 单测。
 */
export interface MobileTerminalHelperTextareaValue {
  value: string;
  setSelectionRange?(start: number, end: number): void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm 只在 blur 或 Ctrl+C/Enter 时清空 helper textarea；手机软键盘中文输入（尤其全角括号）
 *   走 composition / insertText，不会触发清空，导致已提交文本残留并在下次输入被重复转发。
 *
 * Code Logic（这个函数做什么）:
 *   若 helper 存在且 value 非空，则置为空串并尽量把选区归零；空值/缺失返回 false。
 *   必须在 xterm 已从 textarea 读完本次提交（即 onData 已触发）之后调用。
 */
export function clearMobileTerminalHelperTextareaAfterCommit(
  helperTextarea: MobileTerminalHelperTextareaValue | null | undefined,
): boolean {
  if (!helperTextarea) return false;
  if (helperTextarea.value.length === 0) return false;
  helperTextarea.value = '';
  try {
    helperTextarea.setSelectionRange?.(0, 0);
  } catch {
    // 未聚焦或宿主不支持选区时忽略，value 清空已足够打断重复发送。
  }
  return true;
}
