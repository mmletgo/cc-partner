/**
 * 充电模式 API — 通过 Tauri invoke 调用 Rust 账本命令。
 *
 * Business Logic（为什么需要这个模块）:
 *   footer / 设置页 / 扣时心跳必须走 invoke，不能把余额放在 localStorage。
 *
 * Code Logic（这个模块做什么）:
 *   封装快照、切模式、上报焦点、流水、额度读写；成功 body fail-closed decode。
 */

import { invokeDecoded } from './client';
import {
  batteryConfigDecoder,
  batteryLedgerItemDecoder,
  batterySnapshotDecoder,
} from '@/lib/schemas/battery';
import { arrayDecoder } from '@/lib/runtimeSchema';
import type {
  BatteryConfig,
  BatteryLedgerItem,
  BatteryMode,
  BatterySnapshot,
} from '@/lib/types/battery';

const batteryLedgerListDecoder = arrayDecoder(batteryLedgerItemDecoder, { maxLength: 200 });

export const batteryApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   footer 环与遮罩需要权威余额。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_battery_snapshot。
   */
  getSnapshot: (): Promise<BatterySnapshot> =>
    invokeDecoded('get_battery_snapshot', undefined, batterySnapshotDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   footer / 设置页切换充电与无限必须同步主窗与卫星窗。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded set_battery_mode({ mode })。
   */
  setMode: (mode: BatteryMode): Promise<BatterySnapshot> =>
    invokeDecoded('set_battery_mode', { mode }, batterySnapshotDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   每个桌面窗上报自己是否在消耗路由且前台，后端 OR 聚合后只扣一份。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded report_battery_focus({ req })。
   */
  reportFocus: (windowLabel: string, consuming: boolean): Promise<BatterySnapshot> =>
    invokeDecoded(
      'report_battery_focus',
      { req: { windowLabel, consuming } },
      batterySnapshotDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   设置页流水列表需要最近入账 / 消耗记录。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded list_battery_ledger({ limit })。
   */
  listLedger: (limit = 50): Promise<BatteryLedgerItem[]> =>
    invokeDecoded('list_battery_ledger', { limit }, batteryLedgerListDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   设置页额度表单需要当前数字策略。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_battery_config。
   */
  getConfig: (): Promise<BatteryConfig> =>
    invokeDecoded('get_battery_config', undefined, batteryConfigDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   「恢复默认额度」必须读后端默认，而不是前端硬编码。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_default_battery_config。
   */
  getDefaultConfig: (): Promise<BatteryConfig> =>
    invokeDecoded('get_default_battery_config', undefined, batteryConfigDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户改完各来源分钟 / 日上限后整表覆盖。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded update_battery_config({ config })。
   */
  updateConfig: (config: BatteryConfig): Promise<BatteryConfig> =>
    invokeDecoded('update_battery_config', { config }, batteryConfigDecoder),
};
