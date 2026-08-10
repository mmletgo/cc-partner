/**
 * Portable inventory / local action / same-agent pull API。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub 四类资产 UI 通过统一 API 消费后端严格 DTO；
 *   命令名与 request 形状对齐 Rust #[tauri::command]；
 *   成功 body fail-closed；transport 错误只 normalize，稳定 backend code 原样透传。
 *
 * Code Logic（这个模块做什么）:
 *   invokeDecoded + portableInventory schema；禁止 loose typing、无发明 optional defaults。
 */

import {
  portableAssetActionPlanDecoder,
  portableAssetActionResultDecoder,
  portableInventorySnapshotDecoder,
  portablePullPlanDecoder,
  portablePullResultDecoder,
  remotePortableInventoryDecoder,
} from '@/lib/schemas/portableInventory';
import type {
  ApplyPortableAssetActionRequest,
  ApplyPortablePullRequest,
  ListRemotePortableInventoryRequest,
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableAssetApi,
  PortableInventorySnapshotDto,
  PortablePullApi,
  PortablePullPlanDto,
  PortablePullResultDto,
  PreviewPortableAssetActionRequest,
  PreviewPortablePullRequest,
  RemotePortableInventoryDto,
} from '@/lib/types/portableInventory';
import { invokeDecoded } from './client';

/**
 * 可选设备 / 项目上下文（T7）。
 *
 * Business Logic: 用户级 deviceId=null 本机；项目级 projectRef 为本机或 remote:… 身份。
 * Code Logic: 仅非空字段参与 peer 判定；本机 projectRef 还需等待项目级 inventory 路由。
 */
export interface AgentHubRequestContext {
  deviceId?: string | null;
  projectRef?: string | null;
}

/**
 * peer / 远端项目上下文尚无后端 inspect/write 路径时的稳定错误码。
 *
 * Business Logic: UI 可切换上下文，但不得静默落到本机读写。
 * Code Logic: Error.message 与 Error.code 同为该常量。
 */
export const AGENT_HUB_PEER_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE' as const;
/** 本机 projectRef 当前没有 portable inventory V2 路由。 */
export const AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE =
  'AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE' as const;

/**
 * Business Logic: workbench 远端项目 id 形如 `remote:<deviceId>:<inner>`。
 * Code Logic: 前缀匹配；空串 false。
 */
export function isRemoteProjectRef(projectRef: string | null | undefined): boolean {
  const ref = projectRef?.trim() ?? '';
  return ref.startsWith('remote:');
}

/**
 * Business Logic: peer 设备或远端项目需要 peer 路径；本机 projectRef 由断言单独阻断。
 * Code Logic: 非空 deviceId，或 projectRef 以 remote: 开头。
 */
export function requiresPeerAgentHubPath(
  context?: AgentHubRequestContext | null,
): boolean {
  const deviceId = context?.deviceId?.trim() ?? '';
  if (deviceId.length > 0) return true;
  return isRemoteProjectRef(context?.projectRef);
}

/**
 * Business Logic: peer 上下文未接通前 fail-closed，禁止静默本机写/读冒充。
 * Code Logic: 抛带稳定 code 的 Error。
 */
export function assertLocalAgentHubContext(context?: AgentHubRequestContext | null): void {
  const projectRef = context?.projectRef?.trim() ?? '';
  if (projectRef.length > 0 && !isRemoteProjectRef(projectRef)) {
    throw Object.assign(new Error(AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE), {
      code: AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE,
    });
  }
  if (!requiresPeerAgentHubPath(context)) return;
  throw Object.assign(new Error(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE), {
    code: AGENT_HUB_PEER_CONTEXT_UNAVAILABLE,
  });
}

/**
 * Business Logic: 本机调用保持无参兼容旧 sidecar；项目上下文当前不允许静默透传。
 * Code Logic: peer/本机 project 已由 assert 拦截；仅用户级本机调用返回 undefined。
 */
function localInspectInvokeArgs(
  context?: AgentHubRequestContext | null,
): Record<string, unknown> | undefined {
  const projectRef = context?.projectRef?.trim() ?? '';
  if (projectRef.length === 0 || isRemoteProjectRef(projectRef)) {
    return undefined;
  }
  // 保留 helper 便于项目路由接通时扩展；当前 assert 会在本机 project 到达此处前阻断。
  return undefined;
}

/**
 * Tauri 命令名（snake_case）。
 *
 * Business Logic: 与后端 #[tauri::command] 与 brief 命令清单对齐。
 * Code Logic: as const 表，测试锁定常量。
 */
export const PORTABLE_INVENTORY_COMMANDS = {
  inspect: 'agent_hub_inspect_portable_inventory',
  previewAction: 'agent_hub_preview_portable_asset_action',
  applyAction: 'agent_hub_apply_portable_asset_action',
  getAction: 'agent_hub_get_portable_asset_action',
  listRemoteInventory: 'agent_hub_list_remote_portable_inventory',
  previewPull: 'agent_hub_preview_portable_pull',
  applyPull: 'agent_hub_apply_portable_pull',
  getPull: 'agent_hub_get_portable_pull',
} as const;

/**
 * 本机 portable 资产 API。
 *
 * Business Logic: inventory inspect 与 preview/apply/get action。
 * Code Logic: invokeDecoded；request 参数名 `request`；get 用 clientRequestId。
 */
export const portableAssetApi: PortableAssetApi = {
  /**
   * Business Logic: 本机 inventory 是 actual 状态真源（只读）；peer 上下文 fail-closed。
   * Code Logic: assertLocal → agent_hub_inspect_portable_inventory（本机无参）。
   */
  inspect(context?: AgentHubRequestContext): Promise<PortableInventorySnapshotDto> {
    assertLocalAgentHubContext(context);
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.inspect,
      localInspectInvokeArgs(context),
      portableInventorySnapshotDecoder,
    );
  },

  /**
   * Business Logic: apply 前必须绑定 inventory hash 的短期 plan；peer 禁止冒充本机 preview。
   * Code Logic: assertLocal → strip context → agent_hub_preview_portable_asset_action。
   */
  previewAction(
    request: PreviewPortableAssetActionRequest,
  ): Promise<PortableAssetActionPlanDto> {
    assertLocalAgentHubContext(request);
    const { deviceId: _deviceId, projectRef: _projectRef, ...body } = request;
    void _deviceId;
    void _projectRef;
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.previewAction,
      { request: body },
      portableAssetActionPlanDecoder,
    );
  },

  /**
   * Business Logic: 用户确认后 claim/apply；同 clientRequestId 幂等回放；peer 禁止静默本机写。
   * Code Logic: assertLocal → strip context → agent_hub_apply_portable_asset_action。
   */
  applyAction(request: ApplyPortableAssetActionRequest): Promise<PortableAssetActionResultDto> {
    assertLocalAgentHubContext(request);
    const { deviceId: _deviceId, projectRef: _projectRef, ...body } = request;
    void _deviceId;
    void _projectRef;
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.applyAction,
      { request: body },
      portableAssetActionResultDecoder,
    );
  },

  /**
   * Business Logic: 对账 apply 结果（含 outcomeUnknown）；保留稳定 backend code。
   * Code Logic: agent_hub_get_portable_asset_action。
   */
  getAction(clientRequestId: string): Promise<PortableAssetActionResultDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.getAction,
      { clientRequestId },
      portableAssetActionResultDecoder,
    );
  },
};

/**
 * 同类远端 Pull API。
 *
 * Business Logic: metadata-only 远端 inventory + preview/apply/get pull。
 * Code Logic: 四个 invokeDecoded 命令。
 */
export const portablePullApi: PortablePullApi = {
  /**
   * Business Logic: 远端 inventory 仅 metadata（无 path/secret）。
   * Code Logic: agent_hub_list_remote_portable_inventory。
   */
  listRemote(request: ListRemotePortableInventoryRequest): Promise<RemotePortableInventoryDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.listRemoteInventory,
      { request },
      remotePortableInventoryDecoder,
    );
  },

  /**
   * Business Logic: apply 前同源 target pull plan。
   * Code Logic: agent_hub_preview_portable_pull。
   */
  preview(request: PreviewPortablePullRequest): Promise<PortablePullPlanDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.previewPull,
      { request },
      portablePullPlanDecoder,
    );
  },

  /**
   * Business Logic: objects→import→install；同 clientRequestId 幂等。
   * Code Logic: agent_hub_apply_portable_pull。
   */
  apply(request: ApplyPortablePullRequest): Promise<PortablePullResultDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.applyPull,
      { request },
      portablePullResultDecoder,
    );
  },

  /**
   * Business Logic: 对账 pull 结果（partial / outcomeUnknown）。
   * Code Logic: agent_hub_get_portable_pull。
   */
  get(clientRequestId: string): Promise<PortablePullResultDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.getPull,
      { clientRequestId },
      portablePullResultDecoder,
    );
  },
};
