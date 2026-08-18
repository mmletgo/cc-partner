/**
 * Orchestrator P2P 能力探测（对齐 PeerProtocolInfo::supports）。
 *
 * Business Logic（为什么需要这个模块）:
 *   「添加任务块」必须在对端缺 `orchestrator.task-blocks.v1` 时禁用，不能写死 true。
 *
 * Code Logic（这个模块做什么）:
 *   protocol_version/protoVersion >= 1 且 capabilities 精确包含 token 才算支持。
 */

/** 与 Rust `CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1` 对齐。 */
export const ORCHESTRATOR_TASK_BLOCKS_CAPABILITY = 'orchestrator.task-blocks.v1';

/**
 * 对端协议元数据（health snake_case 或 Device camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Desktop 用 devices 的 protoVersion；mobile health 用 protocol_version。
 *
 * Code Logic（字段说明）:
 *   两套版本字段互为别名；capabilities 缺省视为空。
 */
export interface OrchestratorPeerProtocolHint {
  protocol_version?: number;
  protoVersion?: number;
  capabilities?: readonly string[] | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧 peer / v0 health 不得被当成支持任务块，否则 UI 会放出后端必失败的按钮。
 *
 * Code Logic（这个函数做什么）:
 *   复刻 PeerProtocolInfo::supports：version>=1 且 token 全串精确匹配。
 */
export function peerSupportsOrchestratorTaskBlocks(
  info: OrchestratorPeerProtocolHint | null | undefined,
): boolean {
  if (!info) return false;
  const version =
    typeof info.protocol_version === 'number'
      ? info.protocol_version
      : typeof info.protoVersion === 'number'
        ? info.protoVersion
        : 0;
  if (version < 1) return false;
  const caps = Array.isArray(info.capabilities) ? info.capabilities : [];
  return caps.includes(ORCHESTRATOR_TASK_BLOCKS_CAPABILITY);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   本机 owning 项目与当前 UI 同版本，始终可建块；remote shortcut 必须看 owner 能力。
 *
 * Code Logic（这个函数做什么）:
 *   无项目 → false；非 remote → true；remote → peerSupportsOrchestratorTaskBlocks（缺 peer fail-closed）。
 */
export function canCreateOrchestratorTaskBlock(args: {
  projectKind?: string | null;
  peer?: OrchestratorPeerProtocolHint | null;
}): boolean {
  if (!args.projectKind) return false;
  if (args.projectKind !== 'remote') return true;
  return peerSupportsOrchestratorTaskBlocks(args.peer);
}
