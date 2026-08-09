/**
 * Workbench「收藏快捷输入」浮层纯逻辑
 *
 * Business Logic（为什么需要这个模块）:
 *   用户在终端工作时希望一键唤出收藏的 Prompt 列表，挑选后直接把内容插入当前会话输入行
 *   （不回车），避免在 Prompt 库与终端之间反复切换。浮层的开关、标签筛选、搜索词属于
 *   纯 UI 状态机，快捷键匹配依赖与 Prompt 优化浮层一致的归一化规则，抽出为纯函数以便单测。
 *
 * Code Logic（这个模块做什么）:
 *   - 复用 promptOptimizerWidget.shortcutValueFromEvent 把 KeyboardEvent 归一化为
 *     `<ctrl>+<shift>+p` 这类持久化字符串，与配置的 hotkey 比较判定是否命中。
 *   - 提供 FavoriteQuickInputState 状态与 open/close/toggle/setTag/setQuery 纯转换函数。
 *   - 不持有任何 React 副作用或 DOM 访问，便于在叶子 hook 与测试中复用。
 */

import { shortcutValueFromEvent } from './promptOptimizerWidget';

/** 收藏快捷输入浮层默认快捷键（与后端 get_default_config 的 promptQuickInputHotkey 对齐）。 */
export const FAVORITE_QUICK_INPUT_DEFAULT_HOTKEY = '<ctrl>+/';

/** 「全部标签」筛选的稳定标识，与 Prompts 页 allTag 语义一致。 */
export const FAVORITE_QUICK_INPUT_ALL_TAG = 'all';

/** 收藏快捷输入浮层状态：开闭、选中标签、搜索词。 */
export interface FavoriteQuickInputState {
  open: boolean;
  selectedTag: string;
  query: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浮层初始挂载时需要一个干净的关闭态，避免上次会话残留 open=true 导致浮层闪现。
 *
 * Code Logic（这个函数做什么）:
 *   返回 open=false、selectedTag=ALL_TAG、query='' 的新状态对象。
 */
export function createFavoriteQuickInputState(): FavoriteQuickInputState {
  return { open: false, selectedTag: FAVORITE_QUICK_INPUT_ALL_TAG, query: '' };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   快捷键命中需与配置的 hotkey 字符串精确比较；浏览器 KeyboardEvent 的 key 大小写、
 *   修饰键顺序与持久化格式不一致，必须先归一化。复用 shortcutValueFromEvent 保证与
 *   Prompt 优化浮层同一套归一化规则，避免两套行为漂移。
 *
 * Code Logic（这个函数做什么）:
 *   hotkey 为空直接返回 false（用户清空快捷键即禁用）；否则从事件结构（兼容 DOM
 *   KeyboardEvent 与 PromptOptimizerShortcutEvent）提取归一化所需字段，构造完整的
 *   PromptOptimizerShortcutEvent 交给 shortcutValueFromEvent，非空且与 hotkey 严格相等才返回 true。
 */
export function isFavoriteQuickInputShortcut(
  event: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean },
  hotkey: string,
): boolean {
  if (!hotkey) return false;
  const value = shortcutValueFromEvent({
    type: 'keydown',
    key: event.key,
    ctrlKey: event.ctrlKey,
    metaKey: event.metaKey,
    altKey: event.altKey,
    shiftKey: event.shiftKey,
  });
  return value !== '' && value === hotkey;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工具栏按钮和快捷键都需要打开浮层，转换逻辑必须集中在纯函数。
 *
 * Code Logic（这个函数做什么）:
 *   返回 open=true 的新状态，保留 selectedTag/query 以便重开时筛选状态连续。
 */
export function openFavoriteQuickInputPanel(state: FavoriteQuickInputState): FavoriteQuickInputState {
  return { ...state, open: true };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Escape、选中 prompt 后、失焦都需要关闭浮层。
 *
 * Code Logic（这个函数做什么）:
 *   返回 open=false 的新状态，保留 selectedTag/query 以便再次打开时连续。
 */
export function closeFavoriteQuickInputPanel(state: FavoriteQuickInputState): FavoriteQuickInputState {
  return { ...state, open: false };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工具栏按钮和快捷键共用一个 toggle 入口，避免调用方自行判断当前 open 态。
 *
 * Code Logic（这个函数做什么）:
 *   翻转 open 字段，其余字段保持不变。
 */
export function toggleFavoriteQuickInputPanel(
  state: FavoriteQuickInputState,
): FavoriteQuickInputState {
  return { ...state, open: !state.open };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户在浮层内按标签 chip 筛选收藏 Prompt，需更新选中标签。
 *
 * Code Logic（这个函数做什么）:
 *   返回 selectedTag=tag 的新状态；tag 由调用方传入（ALL_TAG 或具体标签名）。
 */
export function setFavoriteQuickInputTag(
  state: FavoriteQuickInputState,
  tag: string,
): FavoriteQuickInputState {
  return { ...state, selectedTag: tag };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   搜索框受控输入需更新 query 字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回 query=query 的新状态。
 */
export function setFavoriteQuickInputQuery(
  state: FavoriteQuickInputState,
  query: string,
): FavoriteQuickInputState {
  return { ...state, query };
}
