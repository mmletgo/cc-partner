export interface ShortcutKeyboardLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export type ShortcutRecordingResult =
  | { type: 'record'; value: string }
  | { type: 'clear'; value: '' }
  | { type: 'cancel' }
  | { type: 'pending' };

export interface ShortcutRecordingOptions {
  allowModifierOnly?: boolean;
  /**
   * 是否允许无修饰键的单键作为快捷键（如纯 Tab、单字符）。
   *
   * Business Logic（为什么需要这个 option）:
   *   截图等系统级快捷键必须强制至少一个修饰键，避免误触普通按键；
   *   但工作台「收藏快捷输入」浮层允许用户配成纯 Tab 这类窗口内单键，
   *   录制时不应拒绝无修饰键的主键。
   */
  allowBareKey?: boolean;
}

const MODIFIER_KEYS = new Set(['Alt', 'AltGraph', 'Control', 'Meta', 'OS', 'Shift', 'Super']);

const SPECIAL_KEY_MAP: Record<string, string> = {
  ArrowDown: '<down>',
  ArrowLeft: '<left>',
  ArrowRight: '<right>',
  ArrowUp: '<up>',
  Enter: '<enter>',
  Home: '<home>',
  End: '<end>',
  PageDown: '<page_down>',
  PageUp: '<page_up>',
  Tab: '<tab>',
};

const DISPLAY_KEY_MAP: Record<string, string> = {
  '<alt>': 'Alt',
  '<cmd>': 'Cmd',
  '<ctrl>': 'Ctrl',
  '<down>': 'Down',
  '<end>': 'End',
  '<enter>': 'Enter',
  '<home>': 'Home',
  '<left>': 'Left',
  '<option>': 'Alt',
  '<page_down>': 'PageDown',
  '<page_up>': 'PageUp',
  '<right>': 'Right',
  '<shift>': 'Shift',
  '<space>': 'Space',
  '<tab>': 'Tab',
  '<up>': 'Up',
};

const MODIFIER_VALUE_BY_KEY: Record<string, string> = {
  Alt: '<alt>',
  AltGraph: '<alt>',
  Control: '<ctrl>',
  Meta: '<cmd>',
  OS: '<cmd>',
  Shift: '<shift>',
  Super: '<cmd>',
};

/**
 * 生成平台默认截图快捷键
 *
 * Business Logic（为什么需要）:
 *   设置页的“恢复默认”需要写回后端可注册的快捷键格式，而不是只适合展示的
 *   `Cmd+Shift+S` 文本。
 *
 * Code Logic（做什么）:
 *   根据浏览器平台字符串判断 macOS 使用 `<cmd>`，其他平台使用 `<ctrl>`，
 *   返回后端 hotkey 模块可转换的 pynput 风格字符串。
 */
export function getDefaultShortcutValue(platform = globalThis.navigator?.platform ?? ''): string {
  return platform.toLowerCase().includes('mac') ? '<cmd>+<shift>+s' : '<ctrl>+<shift>+s';
}

/**
 * 归一化快捷键主键
 *
 * Business Logic（为什么需要）:
 *   用户按键时浏览器给出的 key 可能是大小写字母、空格或 ArrowUp 等名称；
 *   保存前需要归一为后端已有的 pynput 风格。
 *
 * Code Logic（做什么）:
 *   过滤单独修饰键；普通单字符转小写；功能键保留 fN；常用特殊键映射为
 *   `<xxx>` 形式；未知组合键返回 null 表示继续等待。
 */
function normalizeShortcutKey(key: string): string | null {
  if (MODIFIER_KEYS.has(key) || key === 'Dead') return null;
  if (key === ' ' || key === 'Spacebar' || key === 'Space') return '<space>';
  if (/^F\d{1,2}$/i.test(key)) return key.toLowerCase();
  if (key.length === 1) return key.toLowerCase();
  return SPECIAL_KEY_MAP[key] ?? null;
}

/**
 * 提取当前按下的修饰键
 *
 * Business Logic（为什么需要）:
 *   全局截图快捷键应当至少包含一个修饰键，避免把普通字母误设为系统级快捷键。
 *
 * Code Logic（做什么）:
 *   按 Cmd/Ctrl/Alt/Shift 的稳定顺序读取 KeyboardEvent 修饰键状态，返回 pynput
 *   风格的修饰键片段数组。
 */
function shortcutModifierParts(event: ShortcutKeyboardLike): string[] {
  const parts: string[] = [];
  if (event.metaKey) parts.push('<cmd>');
  if (event.ctrlKey) parts.push('<ctrl>');
  if (event.altKey) parts.push('<alt>');
  if (event.shiftKey) parts.push('<shift>');
  return parts;
}

/**
 * 解析一次快捷键录制按键
 *
 * Business Logic（为什么需要）:
 *   设置页进入录制态后，用户按下组合键就应直接记录，而不是让用户手打文本。
 *   Esc 负责取消，Backspace/Delete 负责清空。
 *
 * Code Logic（做什么）:
 *   将 KeyboardEvent 的修饰键和主键转换为后端可保存的 pynput 字符串；
 *   默认要求至少一个修饰键（截图等系统级快捷键）；
 *   allowBareKey=true 时允许无修饰键的单键（如纯 Tab），供窗口内浮层快捷键使用；
 *   修饰键单独按下且 allowModifierOnly=true 时记录单独修饰键。
 */
export function resolveShortcutRecording(
  event: ShortcutKeyboardLike,
  options: ShortcutRecordingOptions = {},
): ShortcutRecordingResult {
  if (event.key === 'Escape') return { type: 'cancel' };
  if (event.key === 'Backspace' || event.key === 'Delete') return { type: 'clear', value: '' };

  if (options.allowModifierOnly && MODIFIER_KEYS.has(event.key)) {
    const value = MODIFIER_VALUE_BY_KEY[event.key];
    return value ? { type: 'record', value } : { type: 'pending' };
  }

  const key = normalizeShortcutKey(event.key);
  const modifiers = shortcutModifierParts(event);
  if (!key) return { type: 'pending' };
  // 默认要求至少一个修饰键；allowBareKey 放行无修饰键的单键（如纯 Tab）
  if (modifiers.length === 0 && !options.allowBareKey) return { type: 'pending' };

  return { type: 'record', value: [...modifiers, key].join('+') };
}

/**
 * 格式化快捷键展示文本
 *
 * Business Logic（为什么需要）:
 *   后端持久化使用 `<cmd>+<shift>+s` 这类机器友好格式；设置页需要展示为
 *   用户容易识别的 `Cmd+Shift+S`。
 *
 * Code Logic（做什么）:
 *   按 `+` 拆分快捷键片段，修饰键和常用特殊键映射为展示名称，普通单字符转大写，
 *   最后重新用 `+` 拼接。
 */
export function formatShortcutForDisplay(shortcut: string): string {
  const parts = shortcut
    .split('+')
    .map((part) => part.trim().toLowerCase())
    .filter(Boolean);
  if (parts.length === 1 && parts[0] === '<ctrl>') return 'Control';

  return parts
    .map((part) => {
      if (DISPLAY_KEY_MAP[part]) return DISPLAY_KEY_MAP[part];
      if (/^f\d{1,2}$/.test(part)) return part.toUpperCase();
      return part.length === 1 ? part.toUpperCase() : part;
    })
    .join('+');
}
