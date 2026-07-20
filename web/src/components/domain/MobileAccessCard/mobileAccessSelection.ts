import type { MobileAccessEntry, MobileAccessInfo } from '@/lib/types';

/**
 * 从 MobileAccessInfo 解析可展示的访问入口列表。
 *
 * Business Logic（为什么需要这个函数）:
 *   后端新接口返回 entries，旧路径或兼容响应可能只有 urls；卡片需要统一拿到可选条目。
 *
 * Code Logic（这个函数做什么）:
 *   若 entries 非空直接返回；否则从非空 urls 解析 host（去掉 IPv6 方括号），
 *   id=host，第一条 isDefault=true；解析失败的 URL 跳过。
 */
export function resolveMobileAccessEntries(
  info: MobileAccessInfo | null | undefined,
): MobileAccessEntry[] {
  if (!info) return [];
  if (info.entries?.length) return info.entries;

  const entries: MobileAccessEntry[] = [];
  for (const raw of info.urls ?? []) {
    const url = raw.trim();
    if (!url) continue;
    let host: string;
    try {
      host = new URL(url).hostname.replace(/^\[|\]$/g, '');
    } catch {
      continue;
    }
    if (!host) continue;
    entries.push({
      id: host,
      url,
      host,
      isDefault: entries.length === 0,
    });
  }
  return entries;
}

/**
 * 选择默认访问入口 id。
 *
 * Business Logic（为什么需要这个函数）:
 *   弹层打开时应优先选中默认出站网段，避免用户扫到不可达地址。
 *
 * Code Logic（这个函数做什么）:
 *   优先 isDefault 条目的 id，否则第一项 id，空列表返回 null。
 */
export function selectDefaultMobileAccessEntryId(entries: MobileAccessEntry[]): string | null {
  return entries.find((entry) => entry.isDefault)?.id ?? entries[0]?.id ?? null;
}

/**
 * 格式化网段芯片标签文案。
 *
 * Business Logic（为什么需要这个函数）:
 *   可识别 wifi/wired 时展示角色+IP，帮助用户对照手机当前网段。
 *
 * Code Logic（这个函数做什么）:
 *   role 为 wifi/wired 时调用对应 labels 函数；否则返回裸 host。
 */
export function formatMobileAccessChipLabel(
  entry: MobileAccessEntry,
  labels: { wifi: (ip: string) => string; wired: (ip: string) => string },
): string {
  if (entry.role === 'wifi') return labels.wifi(entry.host);
  if (entry.role === 'wired') return labels.wired(entry.host);
  return entry.host;
}

/**
 * 根据当前选中 id 解析有效入口。
 *
 * Business Logic（为什么需要这个函数）:
 *   刷新后网卡列表可能变化，需保留仍有效的选中项，否则回落到默认项。
 *
 * Code Logic（这个函数做什么）:
 *   id 命中则返回该 entry；否则用 isDefault/first；空列表返回 null。
 */
export function resolveSelectedMobileAccessEntry(
  entries: MobileAccessEntry[],
  selectedId: string | null,
): MobileAccessEntry | null {
  if (entries.length === 0) return null;
  if (selectedId) {
    const hit = entries.find((entry) => entry.id === selectedId);
    if (hit) return hit;
  }
  const defaultId = selectDefaultMobileAccessEntryId(entries);
  return entries.find((entry) => entry.id === defaultId) ?? entries[0] ?? null;
}
