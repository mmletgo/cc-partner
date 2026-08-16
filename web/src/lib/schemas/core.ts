/**
 * 通用 runtime schema（与具体域解耦）。
 *
 * Business Logic（为什么需要这个模块）:
 *   `ContentVersion` 是 Prompts / Scratchpad / AgentHub 三槽历史共用的版本摘要 DTO；
 *   `core` 类型集中在 `lib/types/core.ts`，runtime decoder 与之平行。
 *
 * Code Logic（这个模块做什么）:
 *   定义 `contentVersionDecoder`（camelCase 严格）+ `contentVersionListDecoder`。
 *   与 prompts/scratchpad 的 history DTO 完全一致；title/contentPreview 可选，
 *   content 仅在 restore 时返回（list 永远不返回）。
 */

import type { ContentVersion, ContentVersionKind } from '../types/core';
import {
  defineDecoder,
  enumDecoder,
  nullableDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * 枚举 `kind` 严格匹配；'conflict' 仅用于同步冲突副本，'history' 为常规历史。
 */
const contentVersionKindDecoder: Decoder<ContentVersionKind> = enumDecoder(
  'ContentVersionKind',
  ['history', 'conflict'] as const,
);

/**
 * Business Logic: ContentVersion IPC decoder。
 * Code Logic: 必填 id/sourceDevice/contentHash/createdAt/kind；
 *   title/contentPreview/content 可空；未知字段允许前向兼容。
 */
export const contentVersionDecoder: Decoder<ContentVersion> = defineDecoder(
  'ContentVersion',
  (value, path) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      throw new Error(
        `Contract "ContentVersion" failed at ${path}: expected object`,
      );
    }
    const record = value as Record<string, unknown>;
    const id = record.id;
    const sourceDevice = record.sourceDevice;
    const contentHash = record.contentHash;
    const createdAt = record.createdAt;
    const kindRaw = record.kind;
    if (typeof id !== 'string') {
      throw new Error(`Contract "ContentVersion" failed at ${path}.id`);
    }
    if (typeof sourceDevice !== 'string') {
      throw new Error(
        `Contract "ContentVersion" failed at ${path}.sourceDevice`,
      );
    }
    if (typeof contentHash !== 'string') {
      throw new Error(
        `Contract "ContentVersion" failed at ${path}.contentHash`,
      );
    }
    if (typeof createdAt !== 'string') {
      throw new Error(
        `Contract "ContentVersion" failed at ${path}.createdAt`,
      );
    }
    const kind = contentVersionKindDecoder.decode(kindRaw, `${path}.kind`);
    const title =
      record.title === undefined
        ? null
        : nullableDecoder(stringDecoder).decode(record.title, `${path}.title`);
    const contentPreview =
      record.contentPreview === undefined
        ? null
        : nullableDecoder(stringDecoder).decode(
            record.contentPreview,
            `${path}.contentPreview`,
          );
    const content =
      record.content === undefined
        ? null
        : optionalDecoder(nullableDecoder(stringDecoder)).decode(
            record.content,
            `${path}.content`,
          );
    return {
      id,
      sourceDevice,
      contentHash,
      createdAt,
      kind,
      title,
      contentPreview,
      content,
    };
  },
);
