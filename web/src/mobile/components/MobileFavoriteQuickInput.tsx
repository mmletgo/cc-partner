/**
 * 移动端「收藏 Prompt 快捷输入」面板。
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端用户在终端工作时，需要从收藏的 Prompt 中快速挑一条插入当前会话输入行（不回车），
 *   作为桌面端快捷键浮层的触点等价入口。移动端只读消费，不暴露 toggle favorite 或编辑能力。
 *
 * Code Logic（这个组件做什么）:
 *   - 以共享 Dialog 原语渲染 bottom sheet（portal / Escape / backdrop / focus trap），禁止手写 modal。
 *   - open 变 true 时经 httpWorkbenchTransport.prompts.list({ favorite: true }) 拉取收藏列表；
 *     失败保留空列表 + StatusMessage danger（含重试）；客户端再按 selectedTag/搜索二次过滤。
 *   - 标签 chip 行复用 deriveTagsFromPrompts 派生（与 Prompts 页 / 桌面浮层一致语义）。
 *   - 选中条目 → 调注入的 onSelectPrompt（由父级把 content 写入终端，不拼 \r）后关闭 sheet。
 *   - 颜色/间距/圆角/阴影 100% tokens.css；用户可见文案走 t('workbench:mobile.favoriteQuickInput.*')。
 *   - 所有 hooks 在 return 之前；open=false 时由 Dialog 原语返回 null。
 */

import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog, StatusMessage } from '@/components/primitives';
import { PromptsIcon, RefreshIcon, SearchIcon, StarIcon, XIcon } from '@/lib/icons';
import type { Prompt } from '@/lib/types';
import { deriveTagsFromPrompts } from '@/pages/Prompts/promptMutations';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import styles from '../MobileWorkbench.module.css';

/** 「全部」标签哨兵；避免与真实 tag 字面量冲突。 */
const FAVORITE_ALL_TAG = '__all__';

export interface MobileFavoriteQuickInputProps {
  open: boolean;
  onClose: () => void;
  /**
   * 选中 prompt 的回调；由父级把 prompt.content 写入当前 session 终端（不拼 \r），
   * 组件在调用后自行关闭 sheet。
   */
  onSelectPrompt: (prompt: Prompt) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTTP/transport 错误形态多样（Error / OrchestratorRuntimeTransportError / 字符串），UI 需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   优先取 Error.message；其它类型 String 化；空值回退空串由调用方补 fallback 文案。
 */
function extractErrorMessage(reason: unknown): string {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === 'string') return reason;
  return String(reason ?? '');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   标签筛选需要拿到单条 Prompt 的标签数组。
 *
 * Code Logic（这个函数做什么）:
 *   后端 to_dto 保证 tags 为真实数组（legacy 单值 tag 仅是其投影），直接返回 tags。
 */
function promptTagsOf(prompt: Prompt): string[] {
  return prompt.tags ?? [];
}

export function MobileFavoriteQuickInput({
  open,
  onClose,
  onSelectPrompt,
}: MobileFavoriteQuickInputProps): ReactElement | null {
  const { t } = useTranslation(['workbench', 'common']);
  const titleId = useId();

  const [favoritePrompts, setFavoritePrompts] = useState<Prompt[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState<number>(0);
  const [selectedTag, setSelectedTag] = useState<string>(FAVORITE_ALL_TAG);
  const [query, setQuery] = useState<string>('');

  // 请求序列守卫：open 切换或重试时丢弃上一个 in-flight 响应，避免慢响应覆盖新选择。
  const requestSeqRef = useRef<number>(0);

  useEffect(() => {
    if (!open) return;
    let active = true;
    const seq = ++requestSeqRef.current;
    setLoading(true);
    setLoadError(null);

    httpWorkbenchTransport.prompts
      ?.list({ favorite: true })
      .then((list) => {
        if (!active || seq !== requestSeqRef.current) return;
        setFavoritePrompts(list);
      })
      .catch((reason: unknown) => {
        if (!active || seq !== requestSeqRef.current) return;
        setFavoritePrompts([]);
        setLoadError(extractErrorMessage(reason));
      })
      .finally(() => {
        if (!active || seq !== requestSeqRef.current) return;
        setLoading(false);
      });

    return () => {
      active = false;
    };
  }, [open, reloadKey]);

  const tags = useMemo(() => deriveTagsFromPrompts(favoritePrompts), [favoritePrompts]);

  const filtered = useMemo(() => {
    const lower = query.trim().toLowerCase();
    return favoritePrompts.filter((prompt) => {
      if (selectedTag !== FAVORITE_ALL_TAG && !promptTagsOf(prompt).includes(selectedTag)) {
        return false;
      }
      if (!lower) return true;
      return (
        prompt.title.toLowerCase().includes(lower) ||
        prompt.content.toLowerCase().includes(lower)
      );
    });
  }, [favoritePrompts, selectedTag, query]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   拉取失败后用户要能重试，而不是只能关闭再重开。
   *
   * Code Logic（这个函数做什么）:
   *   抬高 reloadKey 触发数据 effect 重新跑一次。
   */
  const handleRetry = useCallback((): void => {
    setReloadKey((value) => value + 1);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   选中 prompt 后由父级把 content 写入当前终端（不回车），写入与关闭分离职责。
   *
   * Code Logic（这个函数做什么）:
   *   调注入的 onSelectPrompt；关闭由本组件负责，确保 sheet 总能收起。
   */
  const handleSelect = useCallback(
    (prompt: Prompt): void => {
      onSelectPrompt(prompt);
      onClose();
    },
    [onClose, onSelectPrompt],
  );

  const hasFavorites = favoritePrompts.length > 0;

  return (
    <Dialog
      open={open}
      titleId={titleId}
      onClose={onClose}
      className={styles.favoriteSheet}
      closeOnEscape={!loading}
      closeOnBackdrop={!loading}
    >
      <header className={styles.favoriteHeader}>
        <div className={styles.favoriteTitle}>
          <p>{t('workbench:mobile.favoriteQuickInput.kicker')}</p>
          <h2 id={titleId}>{t('workbench:mobile.favoriteQuickInput.title')}</h2>
        </div>
        <div className={styles.favoriteActions}>
          <button
            type="button"
            className={styles.favoriteActionButton}
            disabled={loading}
            onClick={handleRetry}
          >
            <RefreshIcon size={14} aria-hidden="true" />
            <span>{t('workbench:mobile.favoriteQuickInput.refresh')}</span>
          </button>
          <button
            type="button"
            className={styles.favoriteCloseButton}
            onClick={onClose}
            aria-label={t('workbench:mobile.favoriteQuickInput.close')}
          >
            <XIcon size={14} aria-hidden="true" />
          </button>
        </div>
      </header>

      <div className={styles.favoriteSearch}>
        <SearchIcon size={14} aria-hidden="true" />
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t('workbench:mobile.favoriteQuickInput.searchPlaceholder')}
          aria-label={t('workbench:mobile.favoriteQuickInput.searchAriaLabel')}
          className={styles.favoriteSearchInput}
        />
      </div>

      {tags.length > 0 ? (
        <div
          className={styles.favoriteChipRow}
          role="group"
          aria-label={t('workbench:mobile.favoriteQuickInput.filterByTagAriaLabel')}
        >
          <button
            type="button"
            className={`${styles.favoriteChip} ${
              selectedTag === FAVORITE_ALL_TAG ? styles.favoriteChipActive : ''
            }`.trim()}
            aria-pressed={selectedTag === FAVORITE_ALL_TAG}
            onClick={() => setSelectedTag(FAVORITE_ALL_TAG)}
          >
            <span>{t('workbench:mobile.favoriteQuickInput.allTag')}</span>
          </button>
          {tags.map((tag) => (
            <button
              key={tag}
              type="button"
              className={`${styles.favoriteChip} ${
                selectedTag === tag ? styles.favoriteChipActive : ''
              }`.trim()}
              aria-pressed={selectedTag === tag}
              onClick={() => setSelectedTag(tag)}
            >
              <span>{tag}</span>
            </button>
          ))}
        </div>
      ) : null}

      {loadError ? (
        <StatusMessage tone="danger" action={
          <button
            type="button"
            className={styles.favoriteStatusAction}
            disabled={loading}
            onClick={handleRetry}
          >
            {t('workbench:mobile.favoriteQuickInput.retry')}
          </button>
        }>
          {t('workbench:mobile.favoriteQuickInput.loadFailed', { error: loadError })}
        </StatusMessage>
      ) : null}

      <div className={styles.favoriteList}>
        {loading ? (
          <p className={styles.favoriteState}>{t('workbench:mobile.favoriteQuickInput.loading')}</p>
        ) : filtered.length === 0 ? (
          <div className={styles.favoriteEmpty}>
            {hasFavorites ? (
              <p className={styles.favoriteState}>
                {t('workbench:mobile.favoriteQuickInput.emptyFiltered')}
              </p>
            ) : (
              <>
                <span className={styles.favoriteEmptyIcon} aria-hidden="true">
                  <PromptsIcon size={20} />
                </span>
                <p className={styles.favoriteState}>
                  {t('workbench:mobile.favoriteQuickInput.empty')}
                </p>
                <p className={styles.favoriteEmptyHint}>
                  {t('workbench:mobile.favoriteQuickInput.emptyHint')}
                </p>
              </>
            )}
          </div>
        ) : (
          filtered.map((prompt) => (
            <button
              key={prompt.id}
              type="button"
              className={styles.favoriteItem}
              aria-label={t('workbench:mobile.favoriteQuickInput.itemAriaLabel', {
                title: prompt.title,
              })}
              onClick={() => handleSelect(prompt)}
            >
              <span className={styles.favoriteItemHeader}>
                <strong>{prompt.title}</strong>
                <StarIcon size={12} className={styles.favoriteItemStar} aria-hidden="true" />
              </span>
              <span className={styles.favoriteItemContent}>{prompt.content}</span>
            </button>
          ))
        )}
      </div>
    </Dialog>
  );
}
