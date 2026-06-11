/**
 * TagInput 多标签编辑器
 *
 * Business Logic（为什么需要这个组件）:
 *   Prompt 管理中用户需要自由创建和管理自定义标签，支持多标签分类。
 *   桌面端已有自由输入多标签功能，Web 端需要保持一致体验。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染已有标签为 Tag chips（带删除按钮）
 *   - 提供文本输入框，Enter 添加新标签（去重去空）
 *   - Backspace 在输入为空时删除最后一个标签
 *   - Blur 时自动提交待输入文本
 */

import { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Tag } from '@/components/primitives';
import styles from './TagInput.module.css';

export interface TagInputProps {
  /** 当前标签列表 */
  tags: string[];
  /** 标签变更回调 */
  onChange: (tags: string[]) => void;
  /** 输入框占位文本 */
  placeholder?: string;
  /** 额外容器类名 */
  className?: string;
}

/**
 * 渲染多标签输入器
 *
 * @param props TagInputProps
 * @returns flex 容器内嵌 Tag chips + 文本输入框
 */
export function TagInput({ tags, onChange, placeholder, className }: TagInputProps) {
  const { t } = useTranslation(['prompts']);
  const [inputValue, setInputValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  /**
   * 添加一个新标签（去重去空）
   *
   * @param value 待添加的标签文本
   */
  const addTag = useCallback(
    (value: string) => {
      const trimmed = value.trim();
      if (!trimmed || tags.includes(trimmed)) {
        setInputValue('');
        return;
      }
      onChange([...tags, trimmed]);
      setInputValue('');
    },
    [tags, onChange],
  );

  /**
   * 移除指定标签
   *
   * @param tag 要移除的标签名
   */
  const removeTag = useCallback(
    (tag: string) => {
      onChange(tags.filter((t) => t !== tag));
    },
    [tags, onChange],
  );

  /**
   * 键盘事件处理：Enter 添加 / Backspace 删除末尾
   *
   * @param e 键盘事件
   */
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        addTag(inputValue);
      } else if (e.key === 'Backspace' && !inputValue && tags.length > 0) {
        removeTag(tags[tags.length - 1]);
      }
    },
    [inputValue, tags, addTag, removeTag],
  );

  /**
   * 失焦时自动提交待输入文本（与桌面端 get_tags() 行为一致）
   */
  const handleBlur = useCallback(() => {
    if (inputValue.trim()) {
      addTag(inputValue);
    }
  }, [inputValue, addTag]);

  /** 点击容器任意区域聚焦输入框 */
  const handleContainerClick = useCallback(() => {
    inputRef.current?.focus();
  }, []);

  const containerClass = [styles.container, className].filter(Boolean).join(' ');

  return (
    <div className={containerClass} onClick={handleContainerClick}>
      {tags.map((tag) => (
        <Tag key={tag} size="sm" onClose={() => removeTag(tag)}>
          {tag}
        </Tag>
      ))}
      <input
        ref={inputRef}
        className={styles.input}
        value={inputValue}
        onChange={(e) => setInputValue(e.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={handleBlur}
        placeholder={placeholder ?? t('prompts:tagInputPlaceholder')}
      />
    </div>
  );
}
