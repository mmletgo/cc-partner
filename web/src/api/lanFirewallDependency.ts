/**
 * 局域网防火墙依赖 API - 通过 Tauri invoke 读取当前端口/IP 与平台化放行指引。
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 依赖环境页需要展示局域网互联所需 TCP/UDP 端口和当前系统的手动放行方法。
 *
 * Code Logic（这个模块做什么）:
 *   封装 check_lan_firewall_dependency 命令；组件层只消费类型化 Promise，不直接 invoke。
 */

import { invoke } from './client';
import type { LanFirewallDependencyStatus } from '@/lib/types';

export const lanFirewallDependencyApi = {
  /** 检测局域网防火墙依赖状态与平台指引。 */
  check: () => invoke<LanFirewallDependencyStatus>('check_lan_firewall_dependency'),
};
