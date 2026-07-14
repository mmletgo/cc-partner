/**
 * VersionHistory 纯 helper
 *
 * Business Logic（为什么需要这个模块）:
 *   列表复制与 Drawer 外的页面操作需要同一份正文解析规则。
 *
 * Code Logic（这个模块做什么）:
 *   从 ContentVersion 解析可复制文本；不包含 React 组件。
 */

import type { ContentVersion } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   列表与复制操作需要可读正文预览。
 *
 * Code Logic（这个函数做什么）:
 *   优先 content，其次 contentPreview；空则空串。
 */
export function resolveVersionCopyText(version: ContentVersion): string {
  if (typeof version.content === 'string' && version.content.length > 0) {
    return version.content;
  }
  if (typeof version.contentPreview === 'string') {
    return version.contentPreview;
  }
  return '';
}
