/**
 * Token 统计页 Tauri invoke 封装。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面 Token 统计页的所有读写都应集中走本模块，避免散落 invoke 字符串
 *   与解码逻辑；filter 与 wire 请求结构 shape 不同，必须在同一处转换。
 *   list / summarize 都支持 providerIds / modelIds / projectIds 多值 IN。
 *
 * Code Logic（这个模块做什么）:
 *   - summarize：invokeDecoded<AgentLedgerSummary>('summarize_agent_ledger', { req }, decoder)
 *   - list：invokeDecoded<AgentLedgerSessionPage>('list_agent_ledger', { req }, decoder)
 *   - clear：invokeDecoded<number>('clear_agent_ledger', undefined, decoder)
 *   - export：本地 enum 校验 + invoke<string>('export_token_stats', { req })
 *   req camelCase 与 Rust DTO 严格对齐；filter 未设置字段以 null 透传给后端。
 */

import {
  agentLedgerSessionPageDecoder,
  agentLedgerSummaryDecoder,
  clearTokenStatsResultDecoder,
  exportFormatDecoder,
} from '@/lib/schemas/tokenStats';
import type {
  AgentLedgerFilters,
  AgentLedgerSessionPage,
  AgentLedgerSummary,
  ExportFormat,
} from '@/lib/types/tokenStats';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { invoke, invokeDecoded } from './client';

/**
 * Tauri 命令名（与 Rust #[tauri::command] 对齐，snake_case）。
 *
 * Business Logic（为什么需要）: 集中在 const 表便于测试锁定与批量改名。
 *
 * Code Logic（做什么）: as const 字面量集合。
 */
export const TOKEN_STATS_COMMANDS = {
  summarize: 'summarize_agent_ledger',
  list: 'list_agent_ledger',
  clear: 'clear_agent_ledger',
  export: 'export_token_stats',
} as const;

/**
 * 把前端 AgentLedgerFilters 转换为后端 SummarizeAgentLedgerReq wire shape。
 *
 * Business Logic（为什么需要）:
 *   前端 optional 字段（缺省 = undefined）需要在 wire 转 null，让 Rust `Option<T>` 收到 None。
 *
 * Code Logic（做什么）:
 *   每个标量字段 `value ?? null`；多 providerIds/modelIds/projectIds 数组字段以
 *   `arr ?? null` 透传（Rust 多值 IN 子句）。
 */
function filterToSummarizeReq(filter: AgentLedgerFilters): Record<string, unknown> {
  return {
    window: filter.window ?? null,
    projectId: filter.projectId ?? null,
    providerIds: filter.providerIds ?? null,
    modelIds: filter.modelIds ?? null,
    projectIds: filter.projectIds ?? null,
    worktreeId: filter.worktreeId ?? null,
    startedAfter: filter.startedAfter ?? null,
    startedBefore: filter.startedBefore ?? null,
    bucket: filter.bucket ?? null,
  };
}

/**
 * 把前端 AgentLedgerFilters 转换为 ListAgentLedgerReq wire shape。
 *
 * Business Logic（为什么需要）:
 *   list 与 summarize 语义对齐：providerIds / modelIds / projectIds 全部多值 IN 透传；
 *   明细表 UI 一次性允许用户多选并直接交付给后端，不需要 controller 二次裁剪。
 *
 * Code Logic（做什么）:
 *   projectId / projectIds 互斥：projectIds 优先（前端多值语义），单 projectId 与多值并存
 *   时由后端按 list 字段去重；其它字段 null/limit/cursor 透传。
 */
function filterToListReq(
  filter: AgentLedgerFilters,
  limit: number,
  cursor: string | null,
): Record<string, unknown> {
  return {
    projectId: filter.projectId ?? null,
    providerIds: filter.providerIds ?? null,
    modelIds: filter.modelIds ?? null,
    projectIds: filter.projectIds ?? null,
    worktreeId: filter.worktreeId ?? null,
    endedAfter: filter.startedAfter ?? null,
    endedBefore: filter.startedBefore ?? null,
    cursor: cursor ?? null,
    limit,
  };
}

/**
 * Token 统计页 IPC 入口集合。
 *
 * Business Logic（为什么需要）:
 *   UI 只暴露 tokenStatsApi.*，把命令名、wire shape 转换、decoder 校验隐藏在同一处。
 *
 * Code Logic（做什么）:
 *   4 个 async 方法分别调用 4 个 backend command；export 用 stringDecoder 二次校验
 *   （防后端偶发返回 {error: ...} 仍通过 invoke 流）。
 */
export const tokenStatsApi = {
  /** 时间窗聚合（KPI/Trend/Group 三块共享 summary）。 */
  summarize: (filter: AgentLedgerFilters): Promise<AgentLedgerSummary> =>
    invokeDecoded<AgentLedgerSummary>(
      TOKEN_STATS_COMMANDS.summarize,
      { req: filterToSummarizeReq(filter) },
      agentLedgerSummaryDecoder,
    ),

  /** 明细分页（cursor + limit 翻页）。 */
  list: (
    filter: AgentLedgerFilters,
    cursor: string | null = null,
    limit = 50,
  ): Promise<AgentLedgerSessionPage> =>
    invokeDecoded<AgentLedgerSessionPage>(
      TOKEN_STATS_COMMANDS.list,
      { req: filterToListReq(filter, limit, cursor) },
      agentLedgerSessionPageDecoder,
    ),

  /** 清空 ledger；返回删除行数。 */
  clear: (): Promise<number> =>
    invokeDecoded<number>(TOKEN_STATS_COMMANDS.clear, undefined, clearTokenStatsResultDecoder),

  /**
   * 导出 summary 全量数据为 CSV / JSON。
   *
   * Business Logic（为什么需要）:
   *   含完整趋势/分组；只要合法 export format 才入 invoke。
   *
   * Code Logic（做什么）:
   *   format 走 decoder；invoke 返回 string（绝对路径，UI 做 toast 与 reveal）。
   */
  export: (filter: AgentLedgerFilters, format: ExportFormat): Promise<string> => {
    exportFormatDecoder.decode(format);
    return invoke<string>(TOKEN_STATS_COMMANDS.export, {
      req: { ...filterToSummarizeReq(filter), format },
    });
  },

  /**
   * 在系统文件管理器中显示导出文件。
   *
   * Business Logic（为什么需要）:
   *   导出成功后用户要立刻打开访达/资源管理器核对 CSV/JSON，不应自己拼路径。
   *
   * Code Logic（做什么）:
   *   非空绝对路径交给 plugin-opener `revealItemInDir`；空路径 fail-closed。
   */
  revealExport: async (path: string): Promise<void> => {
    const trimmed = path.trim();
    if (!trimmed) {
      throw new Error('export path is empty');
    }
    await revealItemInDir(trimmed);
  },
};

/** 重新导出 wire-shape helper 便于测试锁定字段映射（不暴露内部 mutable 引用）。 */
export const tokenStatsRequestShapes = {
  summarize: filterToSummarizeReq,
  list: filterToListReq,
} as const;
