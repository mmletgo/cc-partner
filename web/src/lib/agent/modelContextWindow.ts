/**
 * Agent 模型 → context_window 解析（纯函数，不依赖 i18n）。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台右侧「当前会话」卡需要在终态展示累计 tokens 占 context window 的百分比。
 *   Provider 不在 OSC payload 里注入 context window，本地必须硬编码 model → window 映射
 *   作为兜底；查不到时 UI 老实显示「无窗口信息」，禁止默认 200K 后假装精确。
 *
 * Code Logic（这个模块做什么）:
 *   - MODEL_CONTEXT_WINDOW：modelId → tokens（Claude / Codex / OpenCode 已知模型）；
 *   - resolveContextWindow(modelId)：命中返回数字；未命中返回 null；
 *   - 别名映射：modelId 可能带前缀/版本号，先归一化再查表。
 */

/** 归一化后的 modelId → context window tokens（仅作兜底；首选 provider 注入）。 */
const MODEL_CONTEXT_WINDOW: Readonly<Record<string, number>> = {
  // Claude Code
  'claude-haiku-4-5': 200_000,
  'claude-haiku-4-6': 200_000,
  'claude-sonnet-4': 200_000,
  'claude-sonnet-4-5': 200_000,
  'claude-sonnet-4-5-1m': 1_000_000,
  'claude-sonnet-4-6': 200_000,
  'claude-sonnet-4-6-1m': 1_000_000,
  'claude-opus-4': 200_000,
  'claude-opus-4-1m': 1_000_000,
  'claude-opus-4-5': 200_000,
  'claude-opus-4-5-1m': 1_000_000,
  'claude-opus-4-6': 200_000,
  // Codex（CLI 当前模型族；按需扩展）
  'gpt-5': 400_000,
  'gpt-5-codex': 400_000,
  'gpt-5-mini': 400_000,
  // OpenCode（兜底默认；真实 model id 走 provider 配置）
  'opencode-default': 200_000,
};

/**
 * 归一化 modelId：去除空白、转为小写；带日期/版本后缀的短横线段保留。
 * 例：`Claude-Sonnet-4.5-20250929` → `claude-sonnet-4.5-20250929`。
 */
function normalizeModelId(modelId: string): string {
  return modelId.trim().toLowerCase();
}

/**
 * resolveContextWindow
 *
 * Business Logic（为什么需要这个函数）:
 *   UI 需要显示「cumulative / window（pct%）」；window 拿不到时禁止伪造 200K。
 *
 * Code Logic（这个函数做什么）:
 *   接受 null/undefined/字符串 → 命中表返回数字；否则 null。归一化大小写与首尾空白。
 *
 * @param modelId ledger.modelId（nullable）或外部注入；null/undefined → null
 * @returns window tokens（>0）或 null
 */
export function resolveContextWindow(modelId: string | null | undefined): number | null {
  if (modelId == null) return null;
  const normalized = normalizeModelId(modelId);
  if (normalized.length === 0) return null;
  // 1. 精确命中
  const direct = MODEL_CONTEXT_WINDOW[normalized];
  if (typeof direct === 'number' && direct > 0) return direct;
  // 2. 1m 后缀：`…-1m` 或日期后缀前的 `-1m-`
  if (/(?:^|-)1m(?:-|$)/.test(normalized)) return 1_000_000;
  // 3. 前缀命中：去掉日期/版本后缀（短横线分隔的最后一段若像日期则丢弃）
  const segments = normalized.split('-');
  if (segments.length >= 4) {
    const trimmed = segments.slice(0, -1).join('-');
    const partial = MODEL_CONTEXT_WINDOW[trimmed];
    if (typeof partial === 'number' && partial > 0) return partial;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 在 hover/tooltip 场景需要把 context window 数字格式化为可读字符串；
 *   与 tokenFormat 保持一致（>5,000 → k，>=1,000,000 → M）。
 *
 * Code Logic（这个函数做什么）:
 *   委托 formatTokenCount；null 输入返回 null。
 */
export { formatTokenCount as formatContextWindow } from '../tokenFormat';