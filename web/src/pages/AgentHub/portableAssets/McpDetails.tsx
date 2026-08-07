/**
 * McpDetails — MCP 资产专属详情。
 *
 * Business Logic（为什么需要这个组件）:
 *   MCP 只允许展示 transport/source/credential present+hash；禁止 secret 原文。
 *
 * Code Logic（这个组件做什么）:
 *   仅读取 present/hash；忽略 runtime 上多余 secret 字段。
 */

import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';

export interface McpDetailsProps {
  item: PortableInventoryItemDto;
  labels: {
    transport: string;
    source: string;
    credentialPresent: string;
    credentialHash: string;
    presentYes: string;
    presentNo: string;
    missing: string;
  };
}

/**
 * Business Logic: 从 warnings 提取 transport 诊断；不编造 transport。
 * Code Logic: 匹配 transport: 前缀或回落 missing。
 */
function resolveTransport(warnings: string[]): string | null {
  for (const warning of warnings) {
    const match = /^transport:(.+)$/i.exec(warning.trim());
    if (match?.[1]) return match[1];
  }
  return null;
}

/**
 * Business Logic: 只投影 present/hash，绝不读取 secret/token/value。
 * Code Logic: 显式字段访问。
 */
function safeCredential(item: PortableInventoryItemDto): {
  present: boolean | null;
  hash: string | null;
} {
  const cred = item.mcpCredential;
  if (!cred) return { present: null, hash: null };
  return {
    present: Boolean(cred.present),
    hash: typeof cred.hash === 'string' ? cred.hash : null,
  };
}

/**
 * Business Logic: MCP 详情事实面。
 * Code Logic: data attributes 供测试锁定 present/hash，禁止 secret text。
 */
export function McpDetails({ item, labels }: McpDetailsProps) {
  const transport = resolveTransport(item.warnings);
  const credential = safeCredential(item);

  return (
    <section className={styles.drawerSection} data-testid="portable-mcp-details">
      <div className={styles.metaBlock}>
        <div>
          <span className={styles.metaLabel}>{labels.transport}</span>
          <span data-testid="portable-mcp-transport">
            {transport ?? labels.missing}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.source}</span>
          <span data-testid="portable-mcp-source" className={styles.mono}>
            {item.sourcePath ?? labels.missing}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.credentialPresent}</span>
          <span
            data-testid="portable-mcp-credential-present"
            data-present={
              credential.present === null ? 'unknown' : credential.present ? 'true' : 'false'
            }
          >
            {credential.present === null
              ? labels.missing
              : credential.present
                ? labels.presentYes
                : labels.presentNo}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.credentialHash}</span>
          <span data-testid="portable-mcp-credential-hash" className={styles.mono}>
            {credential.hash ?? labels.missing}
          </span>
        </div>
      </div>
    </section>
  );
}

McpDetails.displayName = 'McpDetails';
