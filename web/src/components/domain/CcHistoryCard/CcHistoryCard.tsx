/**
 * CcHistoryCard 业务组件 - Claude 历史 prompt 展示卡片
 *
 * Business Logic（为什么需要这个组件）:
 *   CcHistory 页面右侧时间线把每条用户输入 prompt 渲染为可读单元，
 *   让用户快速浏览/搜索/复制，必要时一键转存为正式 Prompt 或删除。
 *   字段与 PromptCard 不一致（带 occurredAt/gitBranch/ccVersion/sessionId，
 *   且无需 inline 编辑），故不复用 PromptCard，另建轻量卡片。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card 复合组件（elevated variant）拼装 Header/Body
 *   - Header：occurredAt 时间 + gitBranch Tag + ccVersion + 操作按钮（复制/转存/删除）
 *   - Body：content 文本 pre-wrap + word-break；超长默认截断 6 行，提供展开/收起
 *   - 复制：navigator.clipboard.writeText，成功后通过 onCopied 回调通知父级 toast
 *   - 转存为 Prompt：通过 onSaveAsPrompt 委托父组件调 promptsApi.create（成功后父级 toast）
 *   - 删除：通过 onRequestDelete 委托父组件弹确认弹层
 *   - hover 时整卡上浮 1px，提供"可操作"反馈
 */

import { memo, useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Tag } from '@/components/primitives';
import type { CcHistoryItem } from '@/lib/types';
import { CopyIcon, PromptsIcon, TrashIcon } from '@/lib/icons';
import styles from './CcHistoryCard.module.css';

export interface CcHistoryCardProps {
  /** 单条历史 prompt 数据 */
  item: CcHistoryItem;
  /** 复制成功后父级回调（用于 toast 提示） */
  onCopied?: (item: CcHistoryItem) => void;
  /** 转存为 Prompt：父级执行 create 并 toast */
  onSaveAsPrompt?: (item: CcHistoryItem) => void;
  /** 请求删除：父级弹确认弹层 */
  onRequestDelete?: (item: CcHistoryItem) => void;
  className?: string;
}

/**
 * 把 ISO 时间字符串格式化为简洁的本地时间（YYYY-MM-DD HH:mm）
 *
 * @param iso ISO 时间字符串
 * @returns 本地时间字符串；解析失败时返回原串
 */
function formatTimestamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * 渲染 Claude 历史卡片
 *
 * @param props CcHistoryCardProps
 * @returns elevated 卡片，hover 浮起，content 可展开/收起
 */
function CcHistoryCardInner({
  item,
  onCopied,
  onSaveAsPrompt,
  onRequestDelete,
  className,
}: CcHistoryCardProps) {
  const { t } = useTranslation(['ccHistory', 'common']);
  const [expanded, setExpanded] = useState(false);

  /** 复制正文到剪贴板，成功后通知父级 */
  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(item.content);
      onCopied?.(item);
    } catch {
      // 剪贴板失败静默；不阻塞其他操作
    }
  }, [item, onCopied]);

  /** 转存为 Prompt：委托父级 */
  const handleSaveAsPrompt = useCallback(() => {
    onSaveAsPrompt?.(item);
  }, [item, onSaveAsPrompt]);

  /** 请求删除：委托父级弹确认 */
  const handleRequestDelete = useCallback(() => {
    onRequestDelete?.(item);
  }, [item, onRequestDelete]);

  /** 切换展开/收起 */
  const toggleExpand = useCallback(() => {
    setExpanded((prev) => !prev);
  }, []);

  return (
    <Card variant="elevated" className={[styles.card, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <time className={styles.timestamp} dateTime={item.occurredAt}>
          {formatTimestamp(item.occurredAt)}
        </time>
        <div className={styles.meta}>
          {item.gitBranch ? (
            <Tag size="sm" color="accent">
              {item.gitBranch}
            </Tag>
          ) : null}
          {item.ccVersion ? (
            <span className={styles.version} title={t('ccHistory:ccVersion', { version: item.ccVersion })}>
              v{item.ccVersion}
            </span>
          ) : null}
          <div className={styles.actions}>
            <Button
              variant="ghost"
              size="sm"
              icon={<CopyIcon />}
              onClick={handleCopy}
              aria-label={t('ccHistory:copy')}
              title={t('ccHistory:copy')}
            />
            <Button
              variant="ghost"
              size="sm"
              icon={<PromptsIcon />}
              onClick={handleSaveAsPrompt}
              aria-label={t('ccHistory:saveAsPrompt')}
              title={t('ccHistory:saveAsPrompt')}
            />
            <Button
              variant="danger"
              size="sm"
              icon={<TrashIcon />}
              onClick={handleRequestDelete}
              aria-label={t('common:action.delete')}
              title={t('common:action.delete')}
            />
          </div>
        </div>
      </Card.Header>

      <Card.Body className={styles.body}>
        <p className={expanded ? styles.contentExpanded : styles.content}>{item.content}</p>
        <button
          type="button"
          className={styles.expandBtn}
          onClick={toggleExpand}
          aria-expanded={expanded}
        >
          {expanded ? t('ccHistory:collapse') : t('ccHistory:expand')}
        </button>
      </Card.Body>
    </Card>
  );
}

export const CcHistoryCard = memo(CcHistoryCardInner);
CcHistoryCard.displayName = 'CcHistoryCard';

