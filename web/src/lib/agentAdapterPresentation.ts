/**
 * Agent adapter catalog pure presentation helpers.
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings / Workbench / Orchestrator 必须对 openCodeVisible 使用同一 fail-closed 规则：
 *   missing bridge 不得呈现 available green；previewRequired 引导既有 Agent Hub 项目预览。
 *
 * Code Logic（这个模块做什么）:
 *   从 OrchestratorAgentAdapterCatalogItem 派生有效 bridge 状态、可用性 tone、blocked reason 与预览 deep link。
 */

import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types/orchestrator';
import type { OpenCodeBridgeStatus, OpenCodeBridgeView } from '@/lib/types/agentHub';

/** 固定派生 bridge 相对路径（与后端 OPENCODE_RUNTIME_BRIDGE_REL_PATH 对齐）。 */
export const OPENCODE_RUNTIME_BRIDGE_REL_PATH = '.opencode/plugins/cc-partner-runtime.ts';

/**
 * Business Logic: 仅 ready 允许启用 openCodeVisible。
 * Code Logic: status === 'ready'。
 */
export function isOpenCodeBridgeReady(
  status: OpenCodeBridgeStatus | null | undefined,
): boolean {
  return status === 'ready';
}

/**
 * Business Logic: OpenCode missing bridge 必须 fail-closed，不得 silent available。
 * Code Logic: 非 OpenCode 返回 null；OpenCode 缺省 → previewRequired。
 */
export function effectiveOpenCodeBridgeStatus(
  item: Pick<OrchestratorAgentAdapterCatalogItem, 'provider' | 'bridgeStatus'>,
): OpenCodeBridgeStatus | null {
  if (item.provider !== 'openCodeVisible') {
    return item.bridgeStatus ?? null;
  }
  return item.bridgeStatus ?? 'previewRequired';
}

/**
 * Business Logic: OpenCode 必须 available + bridge ready 才算可选。
 * Code Logic: 其它 provider 只看 available；OpenCode 双条件。
 */
export function isAgentAdapterEffectivelyAvailable(
  item: Pick<
    OrchestratorAgentAdapterCatalogItem,
    'provider' | 'available' | 'bridgeStatus'
  >,
): boolean {
  if (item.provider !== 'openCodeVisible') {
    return Boolean(item.available);
  }
  return Boolean(item.available) && isOpenCodeBridgeReady(effectiveOpenCodeBridgeStatus(item));
}

/**
 * Business Logic: 聚合可用性 tone；partial/preview 不得 success green。
 * Code Logic: effectively available → success；OpenCode previewRequired → warn；其余 neutral/danger。
 */
export function agentAdapterAvailabilityTone(
  item: Pick<
    OrchestratorAgentAdapterCatalogItem,
    'provider' | 'available' | 'bridgeStatus' | 'blockedReason' | 'reasonCode'
  >,
): 'success' | 'warn' | 'danger' | 'neutral' {
  if (isAgentAdapterEffectivelyAvailable(item)) {
    return 'success';
  }
  if (item.provider === 'openCodeVisible') {
    const bridge = effectiveOpenCodeBridgeStatus(item);
    if (bridge === 'previewRequired') return 'warn';
    if (bridge === 'conflict' || bridge === 'unsupported') return 'danger';
  }
  if (item.blockedReason || item.reasonCode) return 'danger';
  return 'neutral';
}

/**
 * Business Logic: 阻断文案优先 exact blockedReason / reasonCode / bridge。
 * Code Logic: 顺序 blockedReason → reasonCode → non-ready bridge status。
 */
export function agentAdapterBlockedReason(
  item: Pick<
    OrchestratorAgentAdapterCatalogItem,
    'provider' | 'available' | 'bridgeStatus' | 'blockedReason' | 'reasonCode'
  >,
): string | null {
  if (item.blockedReason) return item.blockedReason;
  if (item.reasonCode) return item.reasonCode;
  if (item.provider === 'openCodeVisible') {
    const bridge = effectiveOpenCodeBridgeStatus(item);
    if (bridge && bridge !== 'ready') return bridge;
  }
  return null;
}

/**
 * Business Logic: 构建 OpenCode bridge 视图（固定 path）。
 * Code Logic: OpenCodeBridgeView。
 */
export function buildOpenCodeBridgeView(
  status: OpenCodeBridgeStatus | null | undefined,
  blockedReason?: string | null,
): OpenCodeBridgeView {
  const effective = status ?? 'previewRequired';
  return {
    status: effective,
    relativePath: OPENCODE_RUNTIME_BRIDGE_REL_PATH,
    blockedReason: blockedReason ?? null,
    requiresProjectPreview: effective !== 'ready',
  };
}

/**
 * Business Logic: 打开既有 Agent Hub 项目预览，不直接 enable/overwrite。
 * Code Logic: `/agent-hub?preview=1&bridge=...&projectId?`。
 */
export function openCodeBridgePreviewHref(projectId?: string | null): string {
  const params = new URLSearchParams();
  params.set('preview', '1');
  params.set('bridge', OPENCODE_RUNTIME_BRIDGE_REL_PATH);
  const trimmed = projectId?.trim();
  if (trimmed) {
    params.set('projectId', trimmed);
  }
  return `/agent-hub?${params.toString()}`;
}

/**
 * Business Logic: provider 短标签 i18n key 后缀（workbench/orchestrator.providers.*）。
 * Code Logic: 已知四 provider；未知返回 null。
 */
export function agentProviderLabelKey(
  providerId: string,
):
  | 'providers.claudeCodeVisible'
  | 'providers.codexVisible'
  | 'providers.genericTerminal'
  | 'providers.openCodeVisible'
  | 'providers.grokBuildVisible'
  | 'providers.geminiCliVisible'
  | null {
  switch (providerId) {
    case 'claudeCodeVisible':
    case 'claudeCode':
      return 'providers.claudeCodeVisible';
    case 'codexVisible':
    case 'codex':
      return 'providers.codexVisible';
    case 'genericTerminal':
      return 'providers.genericTerminal';
    case 'openCodeVisible':
    case 'opencode':
      return 'providers.openCodeVisible';
    case 'grokBuildVisible':
    case 'grok':
      return 'providers.grokBuildVisible';
    case 'geminiCliVisible':
    case 'gemini':
      return 'providers.geminiCliVisible';
    default:
      return null;
  }
}

/**
 * Business Logic: settings.automation.provider.* key 后缀。
 * Code Logic: 四内置 provider；未知 null。
 */
export function settingsProviderLabelKey(
  providerId: string,
):
  | 'automation.provider.claudeCodeVisible'
  | 'automation.provider.codexVisible'
  | 'automation.provider.genericTerminal'
  | 'automation.provider.openCodeVisible'
  | 'automation.provider.grokBuildVisible'
  | 'automation.provider.geminiCliVisible'
  | null {
  switch (providerId) {
    case 'claudeCodeVisible':
      return 'automation.provider.claudeCodeVisible';
    case 'codexVisible':
      return 'automation.provider.codexVisible';
    case 'genericTerminal':
      return 'automation.provider.genericTerminal';
    case 'openCodeVisible':
      return 'automation.provider.openCodeVisible';
    case 'grokBuildVisible':
      return 'automation.provider.grokBuildVisible';
    case 'geminiCliVisible':
      return 'automation.provider.geminiCliVisible';
    default:
      return null;
  }
}
