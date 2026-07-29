/**
 * Agent Hub API — Tauri invoke 封装。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端通过统一 API 读写 Multi-CLI Agent Hub 状态/资产/指令块，
 *   避免页面散落 invoke 字符串与解码逻辑。
 *
 * Code Logic（这个模块做什么）:
 *   使用 invokeDecoded + agentHub schema 做 fail-closed 边界校验；
 *   命令名为 snake_case 对齐 Rust #[tauri::command]。
 */

import {
  agentHubAssetDetailDecoder,
  agentHubAssetSummaryDecoder,
  agentHubAssetSummaryListDecoder,
  agentHubConfirmGitImportOutcomeDecoder,
  agentHubGitImportPreviewDecoder,
  agentHubGitLaneInspectReportDecoder,
  agentHubLanPushPreviewDecoder,
  agentHubMultiTargetPushReportDecoder,
  agentHubProjectPreviewDecoder,
  agentHubProjectStatusDecoder,
  agentHubResolvedProjectMappingDecoder,
  agentHubStatusDecoder,
  instructionBlockDtoDecoder,
  pluginPackageReportDecoder,
} from '@/lib/schemas/agentHub';
import type {
  AgentHubAssetDetail,
  AgentHubAssetSummary,
  AgentHubConfirmGitImportOutcome,
  AgentHubConfirmGitImportRequest,
  AgentHubConfirmProjectMappingRequest,
  AgentHubGitImportPreview,
  AgentHubGitLaneInspectReport,
  AgentHubLanPushPreview,
  AgentHubMultiTargetPushReport,
  AgentHubProjectPreview,
  AgentHubProjectStatus,
  AgentHubPushSelectionRequest,
  AgentHubResolvedProjectMapping,
  AgentHubStatus,
  AgentTarget,
  DesiredPresence,
  InstructionBlockDto,
  InstructionBlockMode,
  PluginPackageReport,
} from '@/lib/types/agentHub';
import { invokeDecoded } from './client';

/**
 * Tauri 命令名（snake_case）。
 *
 * Business Logic: 与后端 #[tauri::command] 对齐，测试可锁定常量。
 * Code Logic: as const 表。
 */
export const AGENT_HUB_COMMANDS = {
  getStatus: 'agent_hub_get_status',
  listAssets: 'agent_hub_list_assets',
  getAsset: 'agent_hub_get_asset',
  updateInstruction: 'agent_hub_update_instruction',
  updateInstructionBlock: 'agent_hub_update_instruction_block',
  pairInstructionVariants: 'agent_hub_pair_instruction_variants',
  previewProject: 'agent_hub_preview_project',
  enableProject: 'agent_hub_enable_project',
  resolveConflict: 'agent_hub_resolve_conflict',
  setTargetBinding: 'agent_hub_set_target_binding',
  setTargetPresence: 'agent_hub_set_target_presence',
  setTargetEnabled: 'agent_hub_set_target_enabled',
  restoreDetachedTarget: 'agent_hub_restore_detached_target',
  deleteAssetEverywhere: 'agent_hub_delete_asset_everywhere',
  previewLanPush: 'agent_hub_preview_lan_push',
  startLanPush: 'agent_hub_start_lan_push',
  getLanPush: 'agent_hub_get_lan_push',
  inspectGitLanes: 'agent_hub_inspect_git_lanes',
  previewGitImport: 'agent_hub_preview_git_import',
  confirmGitImport: 'agent_hub_confirm_git_import',
  confirmProjectMapping: 'agent_hub_confirm_project_mapping',
  getPluginPackageReport: 'agent_hub_get_plugin_package_report',
  previewPluginDelete: 'agent_hub_preview_plugin_delete',
} as const;

/**
 * 列表过滤参数。
 *
 * Business Logic: 页面按 scope/kind 过滤资产。
 * Code Logic: 可选 camelCase invoke 参数。
 */
export interface AgentHubListAssetsArgs {
  scopeId?: string | null;
  kind?: string | null;
}

/**
 * 更新整份指令正文。
 */
export interface AgentHubUpdateInstructionArgs {
  assetId: string;
  contentMarkdown: string;
  expectedRevisionId?: string | null;
}

/**
 * 更新单个指令块。
 */
export interface AgentHubUpdateInstructionBlockArgs {
  assetId: string;
  blockId: string;
  mode?: InstructionBlockMode;
  commonMarkdown?: string;
  variants?: Partial<Record<AgentTarget, string>> | null;
  expectedRevisionId?: string | null;
}

/**
 * 将 targetOnly 块配对为 adapted 变体。
 */
export interface AgentHubPairInstructionVariantsArgs {
  assetId: string;
  blockIds: string[];
  commonMarkdown?: string;
  expectedRevisionId?: string | null;
}

/**
 * 解决冲突。
 */
export interface AgentHubResolveConflictArgs {
  assetId: string;
  conflictId: string;
  resolution: 'keepHub' | 'keepExternal' | 'manual' | string;
  contentMarkdown?: string | null;
}

/**
 * 设置 target binding。
 */
export interface AgentHubSetTargetBindingArgs {
  assetId: string;
  target: AgentTarget;
  desiredPresence: DesiredPresence;
  desiredEnabled: boolean;
}

/**
 * 设置 target presence（target-local）。
 */
export interface AgentHubSetTargetPresenceArgs {
  assetId: string;
  target: AgentTarget;
  desiredPresence: DesiredPresence;
}

/**
 * 设置 target enabled（target-local）。
 */
export interface AgentHubSetTargetEnabledArgs {
  assetId: string;
  target: AgentTarget;
  desiredEnabled: boolean;
}

/**
 * 恢复 detached target。
 */
export interface AgentHubRestoreDetachedTargetArgs {
  assetId: string;
  target: AgentTarget;
}

/**
 * 从所有 target 删除资产。
 */
export interface AgentHubDeleteAssetEverywhereArgs {
  assetId: string;
}

/**
 * Business Logic: 页面与 controller 统一入口。
 * Code Logic: 各方法 invokeDecoded 对应命令。
 */
export const agentHubApi = {
  /**
   * Business Logic: 首屏展示 CLI probe 与写兼容性。
   * Code Logic: agent_hub_get_status → AgentHubStatus。
   */
  getStatus: (): Promise<AgentHubStatus> =>
    invokeDecoded(AGENT_HUB_COMMANDS.getStatus, undefined, agentHubStatusDecoder),

  /**
   * Business Logic: 列表按 scope/kind 过滤。
   * Code Logic: agent_hub_list_assets。
   */
  listAssets: (args: AgentHubListAssetsArgs = {}): Promise<AgentHubAssetSummary[]> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.listAssets,
      {
        scopeId: args.scopeId ?? null,
        kind: args.kind ?? null,
      },
      agentHubAssetSummaryListDecoder,
    ),

  /**
   * Business Logic: 选中资产加载 blocks/conflicts。
   * Code Logic: agent_hub_get_asset。
   */
  getAsset: (assetId: string): Promise<AgentHubAssetDetail> =>
    invokeDecoded(AGENT_HUB_COMMANDS.getAsset, { assetId }, agentHubAssetDetailDecoder),

  /**
   * Business Logic: 保存整份指令 Markdown。
   * Code Logic: agent_hub_update_instruction。
   */
  updateInstruction: (args: AgentHubUpdateInstructionArgs): Promise<AgentHubAssetDetail> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.updateInstruction,
      {
        assetId: args.assetId,
        contentMarkdown: args.contentMarkdown,
        expectedRevisionId: args.expectedRevisionId ?? null,
      },
      agentHubAssetDetailDecoder,
    ),

  /**
   * Business Logic: 编辑块 mode/common/variants。
   * Code Logic: agent_hub_update_instruction_block。
   */
  updateInstructionBlock: (
    args: AgentHubUpdateInstructionBlockArgs,
  ): Promise<InstructionBlockDto> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.updateInstructionBlock,
      {
        assetId: args.assetId,
        blockId: args.blockId,
        mode: args.mode,
        commonMarkdown: args.commonMarkdown,
        variants: args.variants ?? null,
        expectedRevisionId: args.expectedRevisionId ?? null,
      },
      instructionBlockDtoDecoder,
    ),

  /**
   * Business Logic: 将多个 targetOnly 配对为 adapted。
   * Code Logic: agent_hub_pair_instruction_variants。
   */
  pairInstructionVariants: (
    args: AgentHubPairInstructionVariantsArgs,
  ): Promise<AgentHubAssetDetail> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.pairInstructionVariants,
      {
        assetId: args.assetId,
        blockIds: args.blockIds,
        commonMarkdown: args.commonMarkdown,
        expectedRevisionId: args.expectedRevisionId ?? null,
      },
      agentHubAssetDetailDecoder,
    ),

  /**
   * Business Logic: opt-in 前零写入预览。
   * Code Logic: agent_hub_preview_project。
   */
  previewProject: (projectId: string): Promise<AgentHubProjectPreview> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.previewProject,
      { projectId },
      agentHubProjectPreviewDecoder,
    ),

  /**
   * Business Logic: 用户确认后启用项目。
   * Code Logic: agent_hub_enable_project。
   */
  enableProject: (projectId: string): Promise<AgentHubProjectStatus> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.enableProject,
      { projectId },
      agentHubProjectStatusDecoder,
    ),

  /**
   * Business Logic: 解决 canonical/target 冲突。
   * Code Logic: agent_hub_resolve_conflict。
   */
  resolveConflict: (args: AgentHubResolveConflictArgs): Promise<AgentHubAssetDetail> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.resolveConflict,
      {
        assetId: args.assetId,
        conflictId: args.conflictId,
        resolution: args.resolution,
        contentMarkdown: args.contentMarkdown ?? null,
      },
      agentHubAssetDetailDecoder,
    ),

  /**
   * Business Logic: 切换某 target 的 desired presence/enabled。
   * Code Logic: agent_hub_set_target_binding。
   */
  setTargetBinding: (args: AgentHubSetTargetBindingArgs): Promise<AgentHubAssetSummary> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.setTargetBinding,
      {
        assetId: args.assetId,
        target: args.target,
        desiredPresence: args.desiredPresence,
        desiredEnabled: args.desiredEnabled,
      },
      agentHubAssetSummaryDecoder,
    ),

  /**
   * Business Logic: 仅改 target presence（Absent 只卸该 target）。
   * Code Logic: agent_hub_set_target_presence。
   */
  setTargetPresence: (args: AgentHubSetTargetPresenceArgs): Promise<AgentHubAssetSummary> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.setTargetPresence,
      {
        assetId: args.assetId,
        target: args.target,
        desiredPresence: args.desiredPresence,
      },
      agentHubAssetSummaryDecoder,
    ),

  /**
   * Business Logic: 仅改 target enabled，不改其它 CLI。
   * Code Logic: agent_hub_set_target_enabled。
   */
  setTargetEnabled: (args: AgentHubSetTargetEnabledArgs): Promise<AgentHubAssetSummary> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.setTargetEnabled,
      {
        assetId: args.assetId,
        target: args.target,
        desiredEnabled: args.desiredEnabled,
      },
      agentHubAssetSummaryDecoder,
    ),

  /**
   * Business Logic: 外部整文件删除后用户显式恢复。
   * Code Logic: agent_hub_restore_detached_target。
   */
  restoreDetachedTarget: (
    args: AgentHubRestoreDetachedTargetArgs,
  ): Promise<AgentHubAssetSummary> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.restoreDetachedTarget,
      {
        assetId: args.assetId,
        target: args.target,
      },
      agentHubAssetSummaryDecoder,
    ),

  /**
   * Business Logic: 唯一生成 canonical tombstone 的入口。
   * Code Logic: agent_hub_delete_asset_everywhere。
   */
  deleteAssetEverywhere: (
    args: AgentHubDeleteAssetEverywhereArgs,
  ): Promise<AgentHubAssetSummary> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.deleteAssetEverywhere,
      { assetId: args.assetId },
      agentHubAssetSummaryDecoder,
    ),

  /**
   * Business Logic: LAN push 前预览 selection 计数/hash（零传输）。
   * Code Logic: agent_hub_preview_lan_push。
   */
  previewLanPush: (request: AgentHubPushSelectionRequest): Promise<AgentHubLanPushPreview> =>
    invokeDecoded(AGENT_HUB_COMMANDS.previewLanPush, { request }, agentHubLanPushPreviewDecoder),

  /**
   * Business Logic: 启动源侧 multi-target LAN push。
   * Code Logic: agent_hub_start_lan_push。
   */
  startLanPush: (
    request: AgentHubPushSelectionRequest,
  ): Promise<AgentHubMultiTargetPushReport> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.startLanPush,
      { request },
      agentHubMultiTargetPushReportDecoder,
    ),

  /**
   * Business Logic: 读取 LAN push 进度。
   * Code Logic: agent_hub_get_lan_push。
   */
  getLanPush: (requestId: string): Promise<AgentHubMultiTargetPushReport | null> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.getLanPush,
      { requestId },
      {
        name: 'AgentHubMultiTargetPushReport|null',
        decode(value, path = '$') {
          if (value === null || value === undefined) return null;
          return agentHubMultiTargetPushReportDecoder.decode(value, path);
        },
      },
    ),

  /**
   * Business Logic: 只读枚举 Git device lanes。
   * Code Logic: agent_hub_inspect_git_lanes。
   */
  inspectGitLanes: (): Promise<AgentHubGitLaneInspectReport> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.inspectGitLanes,
      undefined,
      agentHubGitLaneInspectReportDecoder,
    ),

  /**
   * Business Logic: Git lane import 预览（零写入）。
   * Code Logic: agent_hub_preview_git_import。
   */
  previewGitImport: (laneDeviceId: string): Promise<AgentHubGitImportPreview> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.previewGitImport,
      { laneDeviceId },
      agentHubGitImportPreviewDecoder,
    ),

  /**
   * Business Logic: 确认 Git import（hash 精确匹配）。
   * Code Logic: agent_hub_confirm_git_import。
   */
  confirmGitImport: (
    request: AgentHubConfirmGitImportRequest,
  ): Promise<AgentHubConfirmGitImportOutcome> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.confirmGitImport,
      { request },
      agentHubConfirmGitImportOutcomeDecoder,
    ),

  /**
   * Business Logic: 保存 project mapping（默认 not opted-in）。
   * Code Logic: agent_hub_confirm_project_mapping。
   */
  confirmProjectMapping: (
    request: AgentHubConfirmProjectMappingRequest,
  ): Promise<AgentHubResolvedProjectMapping> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.confirmProjectMapping,
      { request },
      agentHubResolvedProjectMappingDecoder,
    ),

  /**
   * Business Logic: 拉取 Plugin package per-component 投影报告。
   * Code Logic: agent_hub_get_plugin_package_report → PluginPackageReport。
   */
  getPluginPackageReport: (assetId: string): Promise<PluginPackageReport> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.getPluginPackageReport,
      { assetId },
      pluginPackageReportDecoder,
    ),

  /**
   * Business Logic: 删除 preview（tombstone vs preserve）零写。
   * Code Logic: agent_hub_preview_plugin_delete。
   */
  previewPluginDelete: (assetId: string): Promise<PluginPackageReport> =>
    invokeDecoded(
      AGENT_HUB_COMMANDS.previewPluginDelete,
      { assetId },
      pluginPackageReportDecoder,
    ),
};
