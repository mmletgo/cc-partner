/**
 * PromptCard 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Prompt 管理页和首页都需要把单条 Prompt 渲染为可识别的卡片单元，
 *   用户通过卡片浏览/复制/编辑/删除单条 Prompt。统一的卡片外观和操作布局
 *   可以降低视觉噪音，让用户在大量 Prompt 中快速定位。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card 复合组件（elevated variant）拼装 Header/Body/Footer
 *   - Header 展示标题 + 可选 Tag
 *   - Body 通过 line-clamp CSS 把长文本截断为 4 行
 *   - Footer 提供 Copy/Edit/Trash 三个 ghost/danger 按钮 + 右侧时间戳
 *   - 鼠标 hover 时整卡上浮 1px 并加深阴影，提供明确的"可点击"反馈
 */

import { memo, useCallback } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Tag } from '@/components/primitives';
import { CopyIcon, EditIcon, TrashIcon } from '@/lib/icons';
import styles from './PromptCard.module.css';

/** 单条 Prompt 数据模型（与后端字段保持一致） */
export interface PromptCardPrompt {
  id: string;
  title: string;
  content: string;
  tags?: string[];
  /** @deprecated 使用 tags 字段代替 */
  tag?: string;
  /** ISO 时间字符串 */
  updatedAt: string;
  vectorClock?: Record<string, number>;
}

export interface PromptCardProps {
  prompt: PromptCardPrompt;
  onEdit?: () => void;
  onDelete?: () => void;
  onCopy?: () => void;
  className?: string;
  style?: CSSProperties;
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
 * 渲染 Prompt 卡片
 *
 * @param props PromptCardProps
 * @returns elevated 卡片，hover 时浮起
 */
function PromptCardInner({ prompt, onEdit, onDelete, onCopy, className, style }: PromptCardProps) {
  const { t } = useTranslation(['common']);
  const handleCopy = useCallback(() => {
    onCopy?.();
  }, [onCopy]);

  const handleEdit = useCallback(() => {
    onEdit?.();
  }, [onEdit]);

  const handleDelete = useCallback(() => {
    onDelete?.();
  }, [onDelete]);

  return (
    <Card variant="elevated" className={[styles.card, className].filter(Boolean).join(' ')} style={style}>
      <Card.Header className={styles.header}>
        <h4 className={styles.title}>{prompt.title}</h4>
        {prompt.tags && prompt.tags.length > 0 ? (
          <div className={styles.tagList}>
            {prompt.tags.map((t) => <Tag key={t} size="sm">{t}</Tag>)}
          </div>
        ) : prompt.tag ? <Tag className={styles.tag}>{prompt.tag}</Tag> : null}
      </Card.Header>

      <Card.Body className={styles.body}>
        <p className={styles.content} title={prompt.content}>
          {prompt.content}
        </p>
      </Card.Body>

      <Card.Footer className={styles.footer}>
        <div className={styles.actions}>
          <Button
            variant="ghost"
            size="sm"
            icon={<CopyIcon />}
            onClick={handleCopy}
            aria-label={t('common:action.copy')}
            title={t('common:action.copy')}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={<EditIcon />}
            onClick={handleEdit}
            aria-label={t('common:action.edit')}
            title={t('common:action.edit')}
          />
          <Button
            variant="danger"
            size="sm"
            icon={<TrashIcon />}
            onClick={handleDelete}
            aria-label={t('common:action.delete')}
            title={t('common:action.delete')}
          />
        </div>
        <time className={styles.timestamp} dateTime={prompt.updatedAt}>
          {formatTimestamp(prompt.updatedAt)}
        </time>
      </Card.Footer>
    </Card>
  );
}

export const PromptCard = memo(PromptCardInner);
PromptCard.displayName = 'PromptCard';
