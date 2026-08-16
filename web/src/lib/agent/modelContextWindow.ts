/**
 * Agent 模型 → context_window 解析（纯函数，不依赖 i18n）。
 *
 * Business Logic（为什么需要这个模块）:
 *   状态卡「上下文长度」对齐 ccstatusline-zh：优先读 model 字符串里的 `[1M]`/`(200k)`，
 *   再查已知表，最后对非空 modelId 回落 200k（与 ccstatusline getContextConfig 一致）。
 *
 * Code Logic（这个模块做什么）:
 *   - parseContextWindowSize：从 id/display 抽 k/M；
 *   - MODEL_CONTEXT_WINDOW：精确/家族命中；
 *   - resolveContextWindow：hint → 表 → 1m 标记 → 去日期/`-build` → 200k。
 */

/** 归一化后的 modelId → context window tokens。 */
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
  // xAI / 内部 grok（jsonl 常写 grok-4.6-build，配置为 grok-4.6[1M]）
  'grok-4': 256_000,
  'grok-4.6': 1_000_000,
  'grok-4.6-build': 1_000_000,
  // Codex
  'gpt-5': 400_000,
  'gpt-5-codex': 400_000,
  'gpt-5-mini': 400_000,
  'opencode-default': 200_000,
};

/** ccstatusline `getContextConfig` 的最后回落（非空未知 model / 有占用无 model）。 */
export const DEFAULT_CONTEXT_WINDOW = 200_000;

/**
 * 归一化 modelId：去空白、小写。
 */
function normalizeModelId(modelId: string): string {
  return modelId.trim().toLowerCase();
}

/**
 * 从 model 字符串解析 `[1M]` / `(200k)` / `1M context`。
 */
export function parseContextWindowSize(modelIdentifier: string): number | null {
  const delimited = /(?:\(|\[)\s*(\d+(?:[,_]\d+)*(?:\.\d+)?)\s*([km])\s*(?:\)|\])/i.exec(
    modelIdentifier,
  );
  if (delimited?.[1] && delimited[2]) {
    const parsed = Number.parseFloat(delimited[1].replace(/[,_]/g, ''));
    if (Number.isFinite(parsed) && parsed > 0) {
      return Math.round(parsed * (delimited[2].toLowerCase() === 'm' ? 1_000_000 : 1_000));
    }
  }
  const contextMatch = /\b(\d+(?:[,_]\d+)*(?:\.\d+)?)\s*([km])(?:\s*(?:token\s*)?context)?\b/i.exec(
    modelIdentifier,
  );
  if (!contextMatch?.[1] || !contextMatch[2]) return null;
  const parsed = Number.parseFloat(contextMatch[1].replace(/[,_]/g, ''));
  if (!Number.isFinite(parsed) || parsed <= 0) return null;
  return Math.round(parsed * (contextMatch[2].toLowerCase() === 'm' ? 1_000_000 : 1_000));
}

function lookupTable(normalized: string): number | null {
  const direct = MODEL_CONTEXT_WINDOW[normalized];
  if (typeof direct === 'number' && direct > 0) return direct;
  const withoutBuild = normalized.replace(/-build$/, '');
  if (withoutBuild !== normalized) {
    const hit = MODEL_CONTEXT_WINDOW[withoutBuild];
    if (typeof hit === 'number' && hit > 0) return hit;
  }
  return null;
}

/**
 * resolveContextWindow
 *
 * Business Logic（为什么需要这个函数）:
 *   对齐 ccstatusline-zh ContextWindow：有 hint 用 hint，有表用表，否则 200k。
 *
 * Code Logic（这个函数做什么）:
 *   null/空串 → null；否则 hint → 表 → 1m 标记 → 去日期后缀 → 200k。
 */
export function resolveContextWindow(modelId: string | null | undefined): number | null {
  if (modelId == null) return null;
  const normalized = normalizeModelId(modelId);
  if (normalized.length === 0) return null;

  const hinted = parseContextWindowSize(modelId) ?? parseContextWindowSize(normalized);
  if (hinted != null && hinted > 0) return hinted;

  const table = lookupTable(normalized);
  if (table != null) return table;

  if (/(?:^|-)1m(?:-|$)/.test(normalized) || /\[[\s]*1m[\s]*\]/i.test(normalized)) {
    return 1_000_000;
  }

  const segments = normalized.split('-');
  if (segments.length >= 4) {
    const trimmed = segments.slice(0, -1).join('-');
    const partial = lookupTable(trimmed);
    if (partial != null) return partial;
  }

  return DEFAULT_CONTEXT_WINDOW;
}

export { formatTokenCount as formatContextWindow } from '../tokenFormat';
