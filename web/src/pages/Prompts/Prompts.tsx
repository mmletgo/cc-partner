/**
 * Prompts 页面 - Prompt 库管理
 *
 * Business Logic（为什么需要这个页面）:
 *   Prompt 是 Claude Partner 的核心资产之一：用户在日常工作中沉淀的指令模板。
 *   该页面是 /prompts 路由下的主视图，集中提供：搜索 / 标签筛选 / 同步 / 新建 / 卡片浏览 / inline 编辑 / 删除。
 *   同时让用户一眼看到自己收藏与最近使用的 Prompt。
 *
 * Code Logic（这个页面做什么）:
 *   - 顶部 page header + 副标题描述
 *   - 工具栏：搜索框（300ms debounce）+ 标签筛选 chips + 同步按钮 + 新建按钮
 *   - Prompt 网格：调用 promptsApi.list() 拉取，按搜索关键词 + 激活标签本地过滤
 *   - 点击卡片进入 inline 编辑模式（input + textarea + save/cancel）
 *   - 点击删除时弹 confirm 二次确认
 *   - 失败时显示错误提示，空数据时显示引导文案
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input, Tag } from '@/components/primitives';
import { TagInput } from '@/components/domain/TagInput';
import { promptsApi } from '@/api/prompts';
import type { Prompt } from '@/lib/types';
import { PlusIcon, SearchIcon, SyncIcon, TrashIcon, XIcon, CheckIcon, EditIcon } from '@/lib/icons';
import { debounce } from '@/lib/format';
import styles from './Prompts.module.css';

/** 编辑卡片用的草稿状态 */
interface DraftPrompt {
  id: string;
  title: string;
  content: string;
  tags: string[];
}

type LoadState = 'loading' | 'success' | 'error';

/**
 * Prompts 页面主组件
 */
export function Prompts() {
  const { t } = useTranslation(['prompts', 'common']);

  // ── 列表数据 ──
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [loadState, setLoadState] = useState<LoadState>('loading');
  const [loadError, setLoadError] = useState<string | null>(null);

  // ── 搜索 / 筛选 ──
  const [searchInput, setSearchInput] = useState('');
  const [search, setSearch] = useState('');
  const [activeTag, setActiveTag] = useState<string>('all');
  const [allTags, setAllTags] = useState<string[]>([]);

  // ── 编辑 / 新建 / 删除 ──
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState<DraftPrompt | null>(null);
  const [creatingNew, setCreatingNew] = useState(false);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  /**
   * 拉取 Prompt 列表；API 成功使用真实数据，失败则保留现有数据或置空并设置错误状态
   */
  const loadPrompts = useCallback(async () => {
    try {
      const [data, tags] = await Promise.all([
        promptsApi.list(),
        promptsApi.listTags(),
      ]);
      setPrompts(Array.isArray(data) ? data : []);
      setAllTags(Array.isArray(tags) ? tags : []);
      setLoadState('success');
      setLoadError(null);
    } catch (err) {
      setPrompts((prev) => prev);
      setLoadState('error');
      setLoadError(err instanceof Error ? err.message : t('prompts:loadFailedGeneric'));
    }
  }, [t]);

  useEffect(() => {
    void loadPrompts();
  }, [loadPrompts]);

  // ── 搜索 300ms debounce ──
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const debouncedSetSearch = useCallback(
    debounce((v: unknown) => {
      if (typeof v === 'string') setSearch(v);
    }, 300),
    [],
  );

  const handleSearchInput = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const v = e.target.value;
      setSearchInput(v);
      debouncedSetSearch(v);
    },
    [debouncedSetSearch],
  );

  // ── 过滤后的列表 ──
  const filtered = useMemo(() => {
    const lower = search.trim().toLowerCase();
    return prompts.filter((p) => {
      if (activeTag !== 'all') {
        const promptTags = p.tags ?? (p.tag ? [p.tag] : []);
        if (!promptTags.includes(activeTag)) return false;
      }
      if (!lower) return true;
      return (
        p.title.toLowerCase().includes(lower) ||
        p.content.toLowerCase().includes(lower)
      );
    });
  }, [prompts, search, activeTag]);

  // ── 各标签计数（用于 chip 上的数字角标） ──
  const tagCounts = useMemo(() => {
    const counts: Record<string, number> = { all: prompts.length };
    for (const p of prompts) {
      const promptTags = p.tags ?? (p.tag ? [p.tag] : []);
      for (const t of promptTags) {
        counts[t] = (counts[t] || 0) + 1;
      }
    }
    return counts;
  }, [prompts]);

  // ── 进入编辑模式 ──
  const startEdit = useCallback((p: Prompt) => {
    setCreatingNew(false);
    setEditingId(p.id);
    setDraft({
      id: p.id,
      title: p.title,
      content: p.content,
      tags: p.tags ?? (p.tag ? [p.tag] : []),
    });
  }, []);

  const cancelEdit = useCallback(() => {
    setEditingId(null);
    setDraft(null);
    setCreatingNew(false);
  }, []);

  // ── 保存（新建或更新） ──
  const saveDraft = useCallback(
    async (e?: FormEvent) => {
      e?.preventDefault();
      if (!draft) return;
      const title = draft.title.trim();
      const content = draft.content.trim();
      if (!content) return;

      if (creatingNew) {
        // 本地乐观更新：先展示，API 成功后替换为服务端返回的真实记录
        const newPrompt: Prompt = {
          id: `local-${Date.now()}`,
          title,
          content,
          tags: draft.tags,
          updatedAt: new Date().toISOString(),
        };
        setPrompts((prev) => [newPrompt, ...prev]);
        try {
          const created = await promptsApi.create({ title, content, tags: draft.tags });
          if (created) {
            setPrompts((prev) => prev.map((p) => (p.id === newPrompt.id ? created : p)));
          }
        } catch {
          // 静默失败：保留本地新建
        }
      } else {
        // 更新
        setPrompts((prev) =>
          prev.map((p) =>
            p.id === draft.id
              ? { ...p, title, content, tags: draft.tags, updatedAt: new Date().toISOString() }
              : p,
          ),
        );
        try {
          await promptsApi.update(draft.id, { title, content, tags: draft.tags });
        } catch {
          // 静默失败：保留本地更新
        }
      }
      cancelEdit();
    },
    [draft, creatingNew, cancelEdit],
  );

  // ── 删除确认 ──
  const confirmDelete = useCallback(async () => {
    if (!pendingDeleteId) return;
    const id = pendingDeleteId;
    setPrompts((prev) => prev.filter((p) => p.id !== id));
    setPendingDeleteId(null);
    try {
      await promptsApi.remove(id);
    } catch {
      // 静默失败
    }
  }, [pendingDeleteId]);

  // ── 同步 ──
  const handleSync = useCallback(async () => {
    try {
      await promptsApi.sync();
      await loadPrompts();
    } catch {
      // 静默失败
    }
  }, [loadPrompts]);

  // ── 新建 ──
  const handleCreate = useCallback(() => {
    setEditingId(null);
    setCreatingNew(true);
    setDraft({ id: `new-${Date.now()}`, title: '', content: '', tags: [] });
  }, []);

  // ── 渲染 ──
  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>{t('prompts:eyebrow', { count: prompts.length })}</span>
        <h1 className={styles.title}>{t('prompts:title')}</h1>
        <p className={styles.lead}>{t('prompts:subtitle')}</p>
      </header>

      {/* 工具栏 */}
      <div className={styles.toolbar}>
        <div className={styles.toolbarMain}>
          <div className={styles.searchWrap}>
            <Input
              type="search"
              value={searchInput}
              onChange={handleSearchInput}
              placeholder={t('prompts:searchPlaceholder')}
              icon={<SearchIcon />}
              aria-label={t('prompts:searchAriaLabel')}
              className={styles.search}
            />
          </div>
          <div className={styles.toolbarActions}>
            <Button variant="secondary" size="sm" icon={<SyncIcon />} onClick={handleSync}>
              {t('prompts:sync')}
            </Button>
            <Button variant="primary" size="sm" icon={<PlusIcon />} onClick={handleCreate}>
              {t('common:action.new')}
            </Button>
          </div>
        </div>
        <div className={styles.chipRow} role="group" aria-label={t('prompts:filterByTagAriaLabel')}>
          <FilterChip
            label={t('prompts:allTag')}
            count={tagCounts.all ?? 0}
            active={activeTag === 'all'}
            onClick={() => setActiveTag('all')}
          />
          {allTags.map((t) => (
            <FilterChip
              key={t}
              label={t}
              count={tagCounts[t] ?? 0}
              active={activeTag === t}
              onClick={() => setActiveTag(t)}
            />
          ))}
        </div>
      </div>

      {/* 错误提示条 */}
      {loadState === 'error' ? (
        <p className={styles.notice} role="status">
          {loadError ? t('prompts:loadFailed', { error: loadError }) : t('prompts:loadFailedGeneric')}
        </p>
      ) : null}

      {/* 网格区 */}
      <section className={styles.gridSection}>
        {loadState === 'loading' && prompts.length === 0 ? (
          <GridSkeleton />
        ) : filtered.length === 0 ? (
          <div className={styles.empty}>
            {prompts.length === 0 ? (
              <>
                <p>{t('prompts:empty')}</p>
                <p className={styles.emptyHint}>{t('prompts:emptyHintCreate')}</p>
              </>
            ) : (
              <>
                <p>{t('prompts:emptyFiltered')}</p>
                <p className={styles.emptyHint}>{t('prompts:emptyFilteredHint')}</p>
              </>
            )}
          </div>
        ) : (
          <ul className={styles.grid}>
            {/* 新建占位卡片 */}
            {creatingNew && draft ? (
              <li>
                <EditPromptCard
                  draft={draft}
                  isNew
                  onChange={setDraft}
                  onSave={saveDraft}
                  onCancel={cancelEdit}
                />
              </li>
            ) : null}

            {filtered.map((p) =>
              editingId === p.id && draft ? (
                <li key={p.id}>
                  <EditPromptCard
                    draft={draft}
                    isNew={false}
                    onChange={setDraft}
                    onSave={saveDraft}
                    onCancel={cancelEdit}
                  />
                </li>
              ) : (
                <li key={p.id}>
                  <PromptCardView
                    prompt={p}
                    onEdit={() => startEdit(p)}
                    onDelete={() => setPendingDeleteId(p.id)}
                  />
                </li>
              ),
            )}
          </ul>
        )}
      </section>

      {/* 删除确认弹层 */}
      {pendingDeleteId ? (
        <div className={styles.modalMask} role="dialog" aria-modal="true" aria-labelledby="confirm-title">
          <Card variant="elevated" className={styles.modal}>
            <h3 id="confirm-title" className={styles.modalTitle}>
              {t('prompts:deleteTitle')}
            </h3>
            <p className={styles.modalText}>{t('prompts:deleteConfirm')}</p>
            <div className={styles.modalActions}>
              <Button variant="secondary" size="sm" onClick={() => setPendingDeleteId(null)}>
                {t('common:action.cancel')}
              </Button>
              <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={confirmDelete}>
                {t('common:action.delete')}
              </Button>
            </div>
          </Card>
        </div>
      ) : null}
    </div>
  );
}

// ────────────────────────────────────────────────────────────────
// 子组件
// ────────────────────────────────────────────────────────────────

/** 标签筛选 chip */
function FilterChip({
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

/** 展示态 Prompt 卡片 */
function PromptCardView({
  prompt,
  onEdit,
  onDelete,
}: {
  prompt: Prompt;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation(['prompts', 'common']);
  return (
    <Card variant="elevated" className={styles.promptCard}>
      <Card.Header className={styles.promptHeader}>
        <h3 className={styles.promptTitle}>{prompt.title}</h3>
        <div className={styles.promptActions}>
          <Button
            variant="ghost"
            size="sm"
            icon={<EditIcon />}
            onClick={onEdit}
            aria-label={t('common:action.edit')}
            title={t('common:action.edit')}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={<TrashIcon />}
            onClick={onDelete}
            aria-label={t('common:action.delete')}
            title={t('common:action.delete')}
          />
        </div>
      </Card.Header>
      <Card.Body className={styles.promptBody}>
        <p className={styles.promptContent}>{prompt.content}</p>
      </Card.Body>
      <Card.Footer className={styles.promptFoot}>
        {prompt.tags && prompt.tags.length > 0 ? (
          <div className={styles.tagList}>
            {prompt.tags.map((t) => <Tag key={t} size="sm">{t}</Tag>)}
          </div>
        ) : prompt.tag ? <Tag size="sm">{prompt.tag}</Tag> : <span />}
      </Card.Footer>
    </Card>
  );
}

/** 编辑 / 新建态卡片 */
function EditPromptCard({
  draft,
  isNew,
  onChange,
  onSave,
  onCancel,
}: {
  draft: DraftPrompt;
  isNew: boolean;
  onChange: (next: DraftPrompt) => void;
  onSave: (e?: FormEvent) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation(['prompts', 'common']);
  return (
    <Card variant="elevated" className={[styles.promptCard, styles.promptCardEditing].join(' ')}>
      <form className={styles.editForm} onSubmit={onSave}>
        <Card.Header className={styles.promptHeader}>
          <input
            className={styles.editTitle}
            value={draft.title}
            onChange={(e) => onChange({ ...draft, title: e.target.value })}
            placeholder={t('prompts:titlePlaceholder')}
            aria-label={t('prompts:titleAriaLabel')}
            autoFocus={isNew}
          />
        </Card.Header>
        <Card.Body className={styles.promptBody}>
          <textarea
            className={styles.editContent}
            value={draft.content}
            onChange={(e) => onChange({ ...draft, content: e.target.value })}
            placeholder={t('prompts:contentPlaceholder')}
            aria-label={t('prompts:contentAriaLabel')}
            rows={4}
          />
          <div className={styles.editMeta}>
            <TagInput
              tags={draft.tags}
              onChange={(tags) => onChange({ ...draft, tags })}
              placeholder={t('prompts:tagInputPlaceholder')}
            />
          </div>
        </Card.Body>
        <Card.Footer className={styles.promptFoot}>
          <Button
            variant="ghost"
            size="sm"
            icon={<XIcon />}
            onClick={onCancel}
            type="button"
          >
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            icon={<CheckIcon />}
            type="submit"
            disabled={!draft.content.trim()}
          >
            {t('common:action.save')}
          </Button>
        </Card.Footer>
      </form>
    </Card>
  );
}

/** 网格骨架屏 */
function GridSkeleton() {
  const { t } = useTranslation(['prompts']);
  return (
    <ul className={styles.grid} aria-busy="true" aria-label={t('prompts:skeletonAriaLabel')}>
      {[0, 1, 2, 3, 4, 5].map((i) => (
        <li key={i} className={styles.skeletonCard}>
          <span className={styles.skeletonBlock} style={{ width: '60%', height: 14 }} />
          <span className={styles.skeletonBlock} style={{ width: '90%', height: 12 }} />
          <span className={styles.skeletonBlock} style={{ width: '80%', height: 12 }} />
        </li>
      ))}
    </ul>
  );
}
