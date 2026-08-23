/**
 * 用户级镜像 preview / apply / get API。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub Pull/Push 改为一次镜像全部用户级 Agent；sidecar 命令尚未落地前
 *   前端必须锁定命令名与 request 形状，成功 body fail-closed。
 *
 * Code Logic（这个模块做什么）:
 *   invokeDecoded + userMirror schema；禁止 loose typing、无发明 optional defaults。
 */

import {
  userMirrorPlanDecoder,
  userMirrorResultDecoder,
} from '@/lib/schemas/userMirror';
import type {
  ApplyUserMirrorRequest,
  PreviewUserMirrorRequest,
  UserMirrorApi,
  UserMirrorPlanDto,
  UserMirrorResultDto,
} from '@/lib/types/userMirror';
import { invokeDecoded } from './client';

/**
 * Tauri 命令名（snake_case）。
 *
 * Business Logic: 与后续 sidecar #[tauri::command] 对齐，禁止漂移到旧 portable-pull 名。
 * Code Logic: as const 表，测试锁定常量。
 */
export const USER_MIRROR_COMMANDS = {
  preview: 'agent_hub_preview_user_mirror',
  apply: 'agent_hub_apply_user_mirror',
  get: 'agent_hub_get_user_mirror',
} as const;

/**
 * 用户级镜像 API。
 *
 * Business Logic: preview 绑定 plan；apply 确认后写入；get 对账含 outcomeUnknown。
 * Code Logic: 三条 invokeDecoded；request 参数名 `request`；get 用 clientRequestId。
 */
export const userMirrorApi: UserMirrorApi = {
  /**
   * Business Logic: apply 前必须拿到绑定两侧 inventory hash 的短期 plan。
   * Code Logic: agent_hub_preview_user_mirror。
   */
  preview(request: PreviewUserMirrorRequest): Promise<UserMirrorPlanDto> {
    return invokeDecoded(USER_MIRROR_COMMANDS.preview, { request }, userMirrorPlanDecoder);
  },

  /**
   * Business Logic: 用户确认后 apply；同 clientRequestId 幂等回放。
   * Code Logic: agent_hub_apply_user_mirror。
   */
  apply(request: ApplyUserMirrorRequest): Promise<UserMirrorResultDto> {
    return invokeDecoded(USER_MIRROR_COMMANDS.apply, { request }, userMirrorResultDecoder);
  },

  /**
   * Business Logic: 对账 apply 结果（含 partial / outcomeUnknown）；保留稳定 backend code。
   * Code Logic: agent_hub_get_user_mirror。
   */
  get(clientRequestId: string): Promise<UserMirrorResultDto> {
    return invokeDecoded(
      USER_MIRROR_COMMANDS.get,
      { clientRequestId },
      userMirrorResultDecoder,
    );
  },
};
