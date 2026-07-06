import type { PillTone } from '@/components/primitives/Pill';
import type { LanFirewallDependencyStatus, LanFirewallPlatform } from './types';

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 依赖环境页需要把局域网防火墙状态映射为统一的 Pill 语义色。
 *
 * Code Logic（这个函数做什么）:
 *   HTTP 未监听或无局域网 IP 视为 danger；其余情况仍用 warn，提醒用户防火墙真实放行需手动确认。
 */
export function lanFirewallStatusTone(status: LanFirewallDependencyStatus): PillTone {
  if (status.httpPort <= 0 || !status.lanIp) return 'danger';
  return 'warn';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前端平台名称必须走 i18n，不能直接渲染后端英文平台标签。
 *
 * Code Logic（这个函数做什么）:
 *   将后端平台 key 映射为 settings namespace 下的平台文案 key，未知平台回退 unsupported。
 */
export function platformLabelKey(platform: LanFirewallPlatform): string {
  if (platform === 'macos') return 'settings:lanFirewall.platform.macos';
  if (platform === 'windows') return 'settings:lanFirewall.platform.windows';
  if (platform === 'linux') return 'settings:lanFirewall.platform.linux';
  return 'settings:lanFirewall.platform.unsupported';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   命令块可能包含多条系统命令，测试和复制预览需要稳定拼接格式。
 *
 * Code Logic（这个函数做什么）:
 *   按后端返回顺序用换行拼接 command 字段；空命令列表返回空字符串。
 */
export function buildLanFirewallCommandPreview(status: LanFirewallDependencyStatus): string {
  return status.guidance.commands.map((item) => item.command).join('\n');
}
