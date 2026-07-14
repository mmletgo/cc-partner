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
 *   - accessible name 必须由调用方以 XOR 形式传入 ariaLabel 或 ariaLabelledBy（placeholder 不充当名称）
 */

import { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Tag } from '@/components/primitives';
import styles from './TagInput.module.css';

/** 基础 props：标签列表、变更与展示 */
interface TagInputBaseProps {
  /** 当前标签列表 */
  tags: string[];
  /** 标签变更回调 */
  onChange: (tags: string[]) => void;
  /** 输入框占位文本（不可替代 accessible name） */
  placeholder?: string;
  /** 额外容器类名 */
  className?: string;
}

/**
 * accessible-name XOR：恰好提供 ariaLabel 或 ariaLabelledBy 其一
 * （placeholder 不得充当名称）
 */
export type TagInputProps =
  | (TagInputBaseProps & { ariaLabel: string; ariaLabelledBy?: never })
  | (TagInputBaseProps & { ariaLabelledBy: string; ariaLabel?: never });

/**
 * 渲染多标签输入器
 *
 * Business Logic（为什么需要这个函数）:
 *   Prompt 编辑等表单需要可访问的多标签输入控件。
 *
 * Code Logic（这个函数做什么）:
 *   渲染 Tag chips + 文本输入；把 XOR accessible name 落到 input 的 aria-label/aria-labelledby。
 *
 * @param props TagInputProps
 * @returns flex 容器内嵌 Tag chips + 文本输入框
 */
export function TagInput(props: TagInputProps) {
  const { tags, onChange, placeholder, className } = props;
  const ariaLabel = 'ariaLabel' in props ? props.ariaLabel : undefined;
  const ariaLabelledBy = 'ariaLabelledBy' in props ? props.ariaLabelledBy : undefined;
  const { t } = useTranslation(['prompts']);
  const [inputValue, setInputValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  /**
   * 添加一个新标签（去重去空）
   *
   * Business Logic（为什么需要这个函数）:
   *   用户输入标签后需要即时写入列表，且不能重复/空白。
   *
   * Code Logic（这个函数做什么）:
   *   trim 后若非空且不在 tags 中则 onChange 追加，并清空输入。
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
   * Business Logic（为什么需要这个函数）:
   *   用户需要从已选标签中删除错误项。
   *
   * Code Logic（这个函数做什么）:
   *   过滤掉目标 tag 后 onChange。
   *
   * @param tag 要移除的标签名
   */
  const removeTag = useCallback(
    (tag: string) => {
      onChange(tags.filter((item) => item !== tag));
    },
    [tags, onChange],
  );

  /**
   * 键盘事件处理：Enter 添加 / Backspace 删除末尾
   *
   * Business Logic（为什么需要这个函数）:
   *   键盘流应能完成添加与回退删除，无需离开输入框。
   *
   * Code Logic（这个函数做什么）:
   *   Enter → addTag；空输入 Backspace → remove 最后一个 tag。
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
   *
   * Business Logic（为什么需要这个函数）:
   *   用户离开输入框时不应丢弃已键入但未按 Enter 的标签草稿。
   *
   * Code Logic（这个函数做什么）:
   *   若有非空输入则 addTag。
   */
  const handleBlur = useCallback(() => {
    if (inputValue.trim()) {
      addTag(inputValue);
    }
  }, [inputValue, addTag]);

  /**
   * 点击容器任意区域聚焦输入框
   *
   * Business Logic（为什么需要这个函数）:
   *   点击 chip 区空白也应进入输入态，降低定位成本。
   *
   * Code Logic（这个函数做什么）:
   *   focus 内部 input。
   */
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
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
      />
    </div>
  );
}
