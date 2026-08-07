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
   * Business Logic: 本机 inventory 是 actual 状态真源（只读）。
   * Code Logic: agent_hub_inspect_portable_inventory。
   */
  inspect(): Promise<PortableInventorySnapshotDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.inspect,
      undefined,
      portableInventorySnapshotDecoder,
    );
  },

  /**
   * Business Logic: apply 前必须绑定 inventory hash 的短期 plan。
   * Code Logic: agent_hub_preview_portable_asset_action。
   */
  previewAction(
    request: PreviewPortableAssetActionRequest,
  ): Promise<PortableAssetActionPlanDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.previewAction,
      { request },
      portableAssetActionPlanDecoder,
    );
  },

  /**
   * Business Logic: 用户确认后 claim/apply；同 clientRequestId 幂等回放。
   * Code Logic: agent_hub_apply_portable_asset_action。
   */
  applyAction(request: ApplyPortableAssetActionRequest): Promise<PortableAssetActionResultDto> {
    return invokeDecoded(
      PORTABLE_INVENTORY_COMMANDS.applyAction,
      { request },
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
