/**
 * Workbench「收藏快捷输入」浮层组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在终端区通过快捷键或工具栏按钮唤出收藏 Prompt 列表，搜索/按标签筛选后点击条目，
 *   把内容插入当前会话输入行（不回车）。浮层与 Prompt 优化浮层是两个独立浮层，状态机互不干扰。
 *
 * Code Logic（这个组件做什么）:
 *   - 纯展示 + 受控回调：状态由 useWorkbenchFavoriteQuickInput 叶子 hook 持有，本组件只渲染。
 *   - 标签 chip 行复用 deriveTagsFromPrompts 派生标签集合；列表只展示 favorite===true。
 *   - 颜色/间距/圆角/阴影 100% tokens.css；用户可见文案走 t('workbench:favoriteQuickInput.*')。
 *   - aside 定位参考 promptOptimizerPanel 模式，但数据流是列表选择，不复用其 controller。
 */

import { useMemo } from 'react';
import type { KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Input } from '@/components/primitives';
import { SearchIcon, XIcon, StarIcon } from '@/lib/icons';
import type { Prompt } from '@/lib/types';
import { deriveTagsFromPrompts } from '../Prompts/promptMutations';
import { FAVORITE_QUICK_INPUT_ALL_TAG } from './favoriteQuickInputWidget';
import styles from './WorkbenchFavoriteQuickInput.module.css';

/** 浮层受控 props（与叶子 hook 返回值一一对应）。 */
export interface WorkbenchFavoriteQuickInputProps {
  open: boolean;
  selectedTag: string;
  query: string;
  favoritePrompts: Prompt[];
  loading: boolean;
  loadError: string | null;
  onSelectTag: (tag: string) => void;
  onQueryChange: (query: string) => void;
  onSelectPrompt: (prompt: Prompt) => void;
  onClose: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浮层内 chip 需要复用 Prompts 页 FilterChip 的视觉与无障碍语义（aria-pressed + 计数），
 *   但 FilterChip 是 Prompts.tsx 局部组件；为避免跨页 import 与扩大范围，这里以同语义的轻量
 *   chip 实现，token 与 Prompts 页 chipActive 对齐。
 *
 * Code Logic（这个函数做什么）:
 *   渲染 button[aria-pressed] + label + count；active 时附加 chipActive class。
 */
function FavoriteFilterChip({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={[styles.chip, active ? styles.chipActive : ''].filter(Boolean).join(' ')}
      onClick={onClick}
      aria-pressed={active}
    >
      <span>{label}</span>
      <span className={styles.chipCount}>{count}</span>
    </button>
  );
}

/**
 * 收藏快捷输入浮层。
 *
 * Business Logic（为什么需要这个组件）:
 *   浮层是用户挑选收藏 Prompt 的视觉载体，必须在终端区上层展示并提供搜索/筛选/选择/关闭。
 *
 * Code Logic（这个函数做什么）:
 *   useTranslation 与 useMemo 置于 early return 之前；open=false 返回 null。
 *   派生 tags + filtered；渲染 header/search/chipRow/list/empty/error。
 */
export function WorkbenchFavoriteQuickInput({
  open,
  selectedTag,
  query,
  favoritePrompts,
  loading,
  loadError,
  onSelectTag,
  onQueryChange,
  onSelectPrompt,
  onClose,
}: WorkbenchFavoriteQuickInputProps) {
  const { t } = useTranslation(['workbench', 'common']);

  const tags = useMemo(() => deriveTagsFromPrompts(favoritePrompts), [favoritePrompts]);

  const filtered = useMemo(() => {
    const lower = query.trim().toLowerCase();
    return favoritePrompts.filter((p) => {
      if (selectedTag !== FAVORITE_QUICK_INPUT_ALL_TAG) {
        const promptTags = p.tags ?? [];
        if (!promptTags.includes(selectedTag)) return false;
      }
      if (!lower) return true;
      return (
        p.title.toLowerCase().includes(lower) || p.content.toLowerCase().includes(lower)
      );
    });
  }, [favoritePrompts, selectedTag, query]);

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      onClose();
    }
  };

  if (!open) return null;

  return (
    <aside
      className={styles.panel}
      aria-label={t('workbench:favoriteQuickInput.panelAriaLabel')}
      onKeyDown={handleKeyDown}
      data-testid="workbench-favorite-quick-input-panel"
    >
      <header className={styles.header}>
        <h3 className={styles.title}>{t('workbench:favoriteQuickInput.title')}</h3>
        <button
          type="button"
          className={styles.closeBtn}
          onClick={onClose}
          aria-label={t('workbench:favoriteQuickInput.close')}
        >
          <XIcon size={14} />
        </button>
      </header>

      <div className={styles.searchWrap}>
        <Input
          type="search"
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          placeholder={t('workbench:favoriteQuickInput.searchPlaceholder')}
          aria-label={t('workbench:favoriteQuickInput.searchAriaLabel')}
          icon={<SearchIcon />}
          className={styles.search}
          autoFocus
        />
      </div>

      <div
        className={styles.chipRow}
        role="group"
        aria-label={t('workbench:favoriteQuickInput.filterByTagAriaLabel')}
      >
        <FavoriteFilterChip
          label={t('workbench:favoriteQuickInput.allTag')}
          count={favoritePrompts.length}
          active={selectedTag === FAVORITE_QUICK_INPUT_ALL_TAG}
          onClick={() => onSelectTag(FAVORITE_QUICK_INPUT_ALL_TAG)}
        />
        {tags.map((tag) => (
          <FavoriteFilterChip
            key={tag}
            label={tag}
            count={favoritePrompts.filter((p) => {
              const pt = p.tags ?? [];
              return pt.includes(tag);
            }).length}
            active={selectedTag === tag}
            onClick={() => onSelectTag(tag)}
          />
        ))}
      </div>

      {loadError ? (
        <p className={styles.error} role="alert">
          {t('workbench:favoriteQuickInput.loadFailed', { error: loadError })}
        </p>
      ) : null}

      <div className={styles.list}>
        {loading ? (
          <p className={styles.loading}>{t('workbench:favoriteQuickInput.loading')}</p>
        ) : filtered.length === 0 ? (
          <div className={styles.empty}>
            {favoritePrompts.length === 0 ? (
              <>
                <p>{t('workbench:favoriteQuickInput.empty')}</p>
                <p className={styles.emptyHint}>
                  {t('workbench:favoriteQuickInput.emptyHint')}
                </p>
              </>
            ) : (
              <p>{t('workbench:favoriteQuickInput.emptyFiltered')}</p>
            )}
          </div>
        ) : (
          filtered.map((p) => (
            <button
              key={p.id}
              type="button"
              className={styles.item}
              onClick={() => onSelectPrompt(p)}
              aria-label={t('workbench:favoriteQuickInput.itemAriaLabel', { title: p.title })}
            >
              <div className={styles.itemHeader}>
                <span className={styles.itemTitle}>{p.title}</span>
                {p.favorite ? (
                  <StarIcon size={12} className={styles.itemStar} aria-hidden />
                ) : null}
              </div>
              <p className={styles.itemContent}>{p.content}</p>
            </button>
          ))
        )}
      </div>
    </aside>
  );
}
