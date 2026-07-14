/**
 * Prompts 页面 - Prompt 库管理
 *
 * Business Logic（为什么需要这个页面）:
 *   Prompt 是 cc-partner 的核心资产之一：用户在日常工作中沉淀的指令模板。
 *   该页面是 /prompts 路由下的主视图，集中提供：搜索 / 标签筛选 / 同步 / 新建 / 卡片浏览 / inline 编辑 / 删除。
 *   同时让用户一眼看到自己收藏与最近使用的 Prompt。
 *   新建 / 更新 / 删除采用乐观更新，但 API 失败必须回滚并允许原地重试，不得静默保留伪成功状态。
 *
 * Code Logic（这个页面做什么）:
 *   - 顶部 page header + 副标题描述
 *   - 工具栏：搜索框（300ms debounce）+ 标签筛选 chips + 同步按钮 + 新建按钮
 *   - Prompt 网格：调用 promptsApi.list() 拉取，按搜索关键词 + 激活标签本地过滤
 *   - 点击卡片进入 inline 编辑模式（input + textarea + save/cancel）
 *   - 点击删除时弹 confirm 二次确认
 *   - mutation 走 applyOptimistic / commit / rollback 纯函数；失败恢复草稿并展示错误横幅
 *   - 同一实体 pending 时禁用冲突编辑/删除；标签从 prompts 派生
 *   - 版本历史 Drawer：查看摘要、恢复为新版本、复制内容；冲突用非阻塞 Pill 标识
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Input, Pill, Tag } from '@/components/primitives';
import { TagInput } from '@/components/domain/TagInput';
import {
  resolveVersionCopyText,
  VersionHistoryDrawer,
} from '@/components/domain/VersionHistoryDrawer';
import { promptsApi } from '@/api/prompts';
import type { ContentVersion, Prompt } from '@/lib/types';
import {
  PlusIcon,
  SearchIcon,
  SyncIcon,
  TrashIcon,
  XIcon,
  CheckIcon,
  EditIcon,
  HistoryIcon,
} from '@/lib/icons';
import { debounce } from '@/lib/format';
import styles from './Prompts.module.css';
import {
  applyOptimisticPromptMutation,
  commitPromptMutation,
  deriveTagsFromPrompts,
  promptMutationEntityId,
  rollbackPromptMutation,
  type PromptDraft,
  type PromptMutation,
} from './promptMutations';

/** 编辑卡片用的草稿状态（含本地 id 供表单 key） */
interface DraftPrompt extends PromptDraft {
  id: string;
}

type LoadState = 'loading' | 'success' | 'error';

/**
 * create mutation 的稳定 pending 键。
 *
 * Business Logic（为什么需要这个常量）:
 *   每次 create 会生成新的 optimisticId，若用它做门闩则双重点击变成两个实体。
 *
 * Code Logic（这个常量做什么）:
 *   所有 create 路径共用固定键，保证同一会话只允许一个 in-flight create。
 */
const CREATE_PENDING_KEY = 'create';

/**
 * Business Logic（为什么需要这个函数）:
 *   API 错误需要可读文案；非 Error 对象不能直接展示。
 *
 * Code Logic（这个函数做什么）:
 *   提取 Error.message，否则回退到 fallback。
 */
function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error && err.message ? err.message : fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pending 门闩必须对 create 使用稳定键，对 update/delete 使用实体 id。
 *
 * Code Logic（这个函数做什么）:
 *   create → CREATE_PENDING_KEY；其余 → promptMutationEntityId。
 */
function pendingKeyForMutation(mutation: PromptMutation): string {
  return mutation.kind === 'create' ? CREATE_PENDING_KEY : promptMutationEntityId(mutation);
}

/**
 * Prompts 页面主组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在一个页面完成 Prompt 的浏览、筛选、同步与 CRUD。
 *
 * Code Logic（这个组件做什么）:
 *   管理列表 / 草稿 / pending / failedMutation 状态，把 API 调用包在事务式 mutation 流程中。
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

  // ── 编辑 / 新建 / 删除 ──
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState<DraftPrompt | null>(null);
  const [creatingNew, setCreatingNew] = useState(false);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  // ── 事务式 mutation 状态 ──
  const [pendingEntityIds, setPendingEntityIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  /** 同步门闩：避免 React state 批更新窗口内双过 pending 检查 */
  const pendingEntityIdsRef = useRef<Set<string>>(new Set());
  const [failedMutation, setFailedMutation] = useState<PromptMutation | null>(null);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [syncError, setSyncError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);

  // ── 版本历史 ──
  const [historyPromptId, setHistoryPromptId] = useState<string | null>(null);
  const [versions, setVersions] = useState<ContentVersion[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState<string | null>(null);
  const [restoringVersionId, setRestoringVersionId] = useState<string | null>(null);
  const [conflictPromptIds, setConflictPromptIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const historyRequestSeqRef = useRef(0);

  /**
   * Business Logic（为什么需要这个函数）:
   *   标签 chips 必须以当前列表为真源，避免 tags API 与乐观列表分叉。
   *
   * Code Logic（这个函数做什么）:
   *   从 prompts 派生排序后的标签数组。
   */
  const allTags = useMemo(() => deriveTagsFromPrompts(prompts), [prompts]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   冲突 Pill 需要知道哪些 Prompt 近期存在 conflict 版本，且不得阻塞编辑。
   *
   * Code Logic（这个函数做什么）:
   *   并行 listVersions；任一条 kind=conflict 则记入集合；失败视为无冲突。
   */
  const refreshConflictFlags = useCallback(async (items: Prompt[]) => {
    if (items.length === 0) {
      setConflictPromptIds(new Set());
      return;
    }
    const entries = await Promise.all(
      items.map(async (item) => {
        try {
          const list = await promptsApi.listVersions(item.id);
          const hasConflict = Array.isArray(list) && list.some((v) => v.kind === 'conflict');
          return [item.id, hasConflict] as const;
        } catch {
          return [item.id, false] as const;
        }
      }),
    );
    const next = new Set<string>();
    for (const [id, hasConflict] of entries) {
      if (hasConflict) next.add(id);
    }
    setConflictPromptIds(next);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   页面初次与同步后需要权威列表。
   *
   * Code Logic（这个函数做什么）:
   *   拉取 list；成功写入 prompts 并刷新冲突标记；失败保留现有数据并设置错误状态。
   *   标签不再依赖 listTags，统一从 prompts 派生。
   */
  const loadPrompts = useCallback(async () => {
    try {
      const data = await promptsApi.list();
      const list = Array.isArray(data) ? data : [];
      setPrompts(list);
      setLoadState('success');
      setLoadError(null);
      void refreshConflictFlags(list);
    } catch (err) {
      setLoadState('error');
      setLoadError(err instanceof Error ? err.message : t('prompts:loadFailedGeneric'));
    }
  }, [refreshConflictFlags, t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    void loadPrompts();
  }, [loadPrompts]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // ── 搜索 300ms debounce ──
  const debouncedSetSearch = useMemo(
    () =>
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
        const promptTags = p.tags && p.tags.length > 0 ? p.tags : p.tag ? [p.tag] : [];
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
      const promptTags = p.tags && p.tags.length > 0 ? p.tags : p.tag ? [p.tag] : [];
      for (const tag of promptTags) {
        counts[tag] = (counts[tag] || 0) + 1;
      }
    }
    return counts;
  }, [prompts]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   pending 集合需要可增删而不丢失其他实体，且同步占用避免双重点击。
   *
   * Code Logic（这个函数做什么）:
   *   同步写入 ref 并 setState 加入 entityId。
   */
  const markPending = useCallback((entityId: string) => {
    pendingEntityIdsRef.current.add(entityId);
    setPendingEntityIds(new Set(pendingEntityIdsRef.current));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   mutation 结束后释放该实体，允许再次编辑/删除。
   *
   * Code Logic（这个函数做什么）:
   *   同步从 ref 删除并 setState。
   */
  const clearPending = useCallback((entityId: string) => {
    if (!pendingEntityIdsRef.current.has(entityId)) return;
    pendingEntityIdsRef.current.delete(entityId);
    setPendingEntityIds(new Set(pendingEntityIdsRef.current));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   失败后需要恢复用户正在编辑的草稿，而不是丢失输入。
   *
   * Code Logic（这个函数做什么）:
   *   根据 mutation 种类恢复 creatingNew / editingId / draft。
   */
  const restoreDraftFromMutation = useCallback((mutation: PromptMutation) => {
    if (mutation.kind === 'create') {
      setCreatingNew(true);
      setEditingId(null);
      setDraft({
        id: mutation.optimisticId,
        title: mutation.draft.title,
        content: mutation.draft.content,
        tags: [...mutation.draft.tags],
      });
      return;
    }
    if (mutation.kind === 'update') {
      setCreatingNew(false);
      setEditingId(mutation.id);
      setDraft({
        id: mutation.id,
        title: mutation.draft.title,
        content: mutation.draft.content,
        tags: [...mutation.draft.tags],
      });
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   create/update/delete 共用同一套乐观 apply → API → commit/rollback 流程。
   *
   * Code Logic（这个函数做什么）:
   *   用 ref 同步门闩（create 稳定键）；标记 pending，应用乐观变更，调用 run，
   *   成功 commit 并清除错误；失败 rollback、保存 failedMutation、恢复草稿、展示错误。
   */
  const runMutation = useCallback(
    async (
      mutation: PromptMutation,
      run: () => Promise<Prompt | void>,
    ): Promise<boolean> => {
      const pendingKey = pendingKeyForMutation(mutation);
      if (pendingEntityIdsRef.current.has(pendingKey)) {
        return false;
      }

      markPending(pendingKey);
      setFailedMutation(null);
      setMutationError(null);
      setPrompts((prev) => applyOptimisticPromptMutation(prev, mutation));

      // 关闭编辑态，等结果
      setEditingId(null);
      setDraft(null);
      setCreatingNew(false);
      setPendingDeleteId(null);

      try {
        const server = await run();
        setPrompts((prev) =>
          commitPromptMutation(
            prev,
            mutation,
            server && typeof server === 'object' ? server : undefined,
          ),
        );
        setFailedMutation(null);
        setMutationError(null);
        return true;
      } catch (err) {
        setPrompts((prev) => rollbackPromptMutation(prev, mutation));
        setFailedMutation(mutation);
        setMutationError(
          errorMessage(
            err,
            mutation.kind === 'create'
              ? t('prompts:createFailedGeneric')
              : mutation.kind === 'update'
                ? t('prompts:updateFailedGeneric')
                : t('prompts:deleteFailedGeneric'),
          ),
        );
        if (mutation.kind === 'create' || mutation.kind === 'update') {
          restoreDraftFromMutation(mutation);
        }
        return false;
      } finally {
        clearPending(pendingKey);
      }
    },
    [markPending, clearPending, restoreDraftFromMutation, t],
  );

  // ── 进入编辑模式 ──
  const startEdit = useCallback((p: Prompt) => {
    if (pendingEntityIdsRef.current.has(p.id)) return;
    setCreatingNew(false);
    setEditingId(p.id);
    setDraft({
      id: p.id,
      title: p.title,
      content: p.content,
      tags: p.tags && p.tags.length > 0 ? p.tags : p.tag ? [p.tag] : [],
    });
    setFailedMutation(null);
    setMutationError(null);
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

      const draftPayload: PromptDraft = {
        title,
        content,
        tags: [...draft.tags],
      };

      if (creatingNew) {
        if (pendingEntityIdsRef.current.has(CREATE_PENDING_KEY)) return;
        const optimisticId = draft.id.startsWith('new-')
          ? `local-${Date.now()}`
          : draft.id;
        const mutation: PromptMutation = {
          kind: 'create',
          optimisticId,
          draft: draftPayload,
        };
        await runMutation(mutation, () =>
          promptsApi.create({
            title: draftPayload.title,
            content: draftPayload.content,
            tags: draftPayload.tags,
          }),
        );
        return;
      }

      const before = prompts.find((p) => p.id === draft.id);
      if (!before) return;
      if (pendingEntityIdsRef.current.has(draft.id)) return;

      const mutation: PromptMutation = {
        kind: 'update',
        id: draft.id,
        before,
        draft: draftPayload,
      };
      await runMutation(mutation, () =>
        promptsApi.update(draft.id, {
          title: draftPayload.title,
          content: draftPayload.content,
          tags: draftPayload.tags,
        }),
      );
    },
    [draft, creatingNew, prompts, runMutation],
  );

  // ── 删除确认 ──
  const confirmDelete = useCallback(async () => {
    if (!pendingDeleteId) return;
    const id = pendingDeleteId;
    if (pendingEntityIdsRef.current.has(id)) return;
    const index = prompts.findIndex((p) => p.id === id);
    if (index < 0) {
      setPendingDeleteId(null);
      return;
    }
    const before = prompts[index];
    if (!before) return;
    const mutation: PromptMutation = {
      kind: 'delete',
      id,
      before,
      index,
    };
    await runMutation(mutation, async () => {
      await promptsApi.remove(id);
    });
  }, [pendingDeleteId, prompts, runMutation]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   失败后用户应能用同一 payload 原地重试，无需重新填写。
   *
   * Code Logic（这个函数做什么）:
   *   读取 failedMutation，按 kind 重放 create/update/delete API。
   */
  const retryFailedMutation = useCallback(async () => {
    if (!failedMutation) return;
    const mutation = failedMutation;
    if (mutation.kind === 'create') {
      await runMutation(mutation, () =>
        promptsApi.create({
          title: mutation.draft.title,
          content: mutation.draft.content,
          tags: mutation.draft.tags,
        }),
      );
      return;
    }
    if (mutation.kind === 'update') {
      await runMutation(mutation, () =>
        promptsApi.update(mutation.id, {
          title: mutation.draft.title,
          content: mutation.draft.content,
          tags: mutation.draft.tags,
        }),
      );
      return;
    }
    await runMutation(mutation, async () => {
      await promptsApi.remove(mutation.id);
    });
  }, [failedMutation, runMutation]);

  // ── 同步 ──
  const handleSync = useCallback(async () => {
    if (syncing) return;
    setSyncing(true);
    setSyncError(null);
    try {
      await promptsApi.sync();
      await loadPrompts();
      setSyncError(null);
    } catch (err) {
      setSyncError(errorMessage(err, t('prompts:syncFailedGeneric')));
    } finally {
      setSyncing(false);
    }
  }, [loadPrompts, syncing, t]);

  // ── 新建 ──
  const handleCreate = useCallback(() => {
    setEditingId(null);
    setCreatingNew(true);
    setDraft({ id: `new-${Date.now()}`, title: '', content: '', tags: [] });
    setFailedMutation(null);
    setMutationError(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开版本历史时需要权威版本列表。
   *
   * Code Logic（这个函数做什么）:
   *   设置 historyPromptId，按 request seq 拉取 listVersions，并同步 conflict 标记。
   */
  const openVersionHistory = useCallback(
    async (promptId: string) => {
      const seq = ++historyRequestSeqRef.current;
      setHistoryPromptId(promptId);
      setVersionsLoading(true);
      setVersionsError(null);
      try {
        const list = await promptsApi.listVersions(promptId);
        if (historyRequestSeqRef.current !== seq) return;
        const safe = Array.isArray(list) ? list : [];
        setVersions(safe);
        setConflictPromptIds((prev) => {
          const next = new Set(prev);
          if (safe.some((v) => v.kind === 'conflict')) next.add(promptId);
          else next.delete(promptId);
          return next;
        });
      } catch (err) {
        if (historyRequestSeqRef.current !== seq) return;
        setVersions([]);
        setVersionsError(
          t('prompts:versionHistoryLoadFailed', {
            error: errorMessage(err, t('prompts:versionHistoryLoadFailedGeneric')),
          }),
        );
      } finally {
        if (historyRequestSeqRef.current === seq) {
          setVersionsLoading(false);
        }
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭历史抽屉后不应残留请求态与列表。
   *
   * Code Logic（这个函数做什么）:
   *   使挂起请求 stale，清空 history 状态。
   */
  const closeVersionHistory = useCallback(() => {
    historyRequestSeqRef.current += 1;
    setHistoryPromptId(null);
    setVersions([]);
    setVersionsError(null);
    setVersionsLoading(false);
    setRestoringVersionId(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   恢复历史版本应成为新的 active 行，并刷新卡片与冲突标记。
   *
   * Code Logic（这个函数做什么）:
   *   调用 restoreVersion，成功后合并列表、刷新 flags 并重载版本列表。
   */
  const handleRestoreVersion = useCallback(
    async (version: ContentVersion) => {
      if (!historyPromptId || restoringVersionId) return;
      setRestoringVersionId(version.id);
      setVersionsError(null);
      try {
        const restored = await promptsApi.restoreVersion(historyPromptId, version.id);
        setPrompts((prev) => {
          const idx = prev.findIndex((p) => p.id === restored.id);
          if (idx < 0) return [restored, ...prev];
          const next = [...prev];
          next[idx] = restored;
          return next;
        });
        const list = await promptsApi.listVersions(historyPromptId);
        const safe = Array.isArray(list) ? list : [];
        setVersions(safe);
        setConflictPromptIds((prev) => {
          const next = new Set(prev);
          if (safe.some((v) => v.kind === 'conflict')) next.add(historyPromptId);
          else next.delete(historyPromptId);
          return next;
        });
      } catch (err) {
        setVersionsError(
          t('prompts:versionRestoreFailed', {
            error: errorMessage(err, t('prompts:versionRestoreFailedGeneric')),
          }),
        );
      } finally {
        setRestoringVersionId(null);
      }
    },
    [historyPromptId, restoringVersionId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要把冲突/历史正文复制到剪贴板。
   *
   * Code Logic（这个函数做什么）:
   *   优先 content，否则 contentPreview，写入 clipboard。
   */
  const handleCopyVersion = useCallback(
    async (version: ContentVersion) => {
      const text = resolveVersionCopyText(version);
      if (!text) return;
      try {
        await navigator.clipboard.writeText(text);
      } catch (err) {
        setVersionsError(errorMessage(err, t('prompts:versionCopyFailed')));
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   错误横幅需要区分 create/update/delete 文案。
   *
   * Code Logic（这个函数做什么）:
   *   根据 failedMutation.kind 与 mutationError 生成展示文案。
   */
  const mutationErrorText = useMemo(() => {
    if (!mutationError || !failedMutation) return null;
    if (failedMutation.kind === 'create') {
      return t('prompts:createFailed', { error: mutationError });
    }
    if (failedMutation.kind === 'update') {
      return t('prompts:updateFailed', { error: mutationError });
    }
    return t('prompts:deleteFailed', { error: mutationError });
  }, [mutationError, failedMutation, t]);

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
            <Button
              variant="secondary"
              size="sm"
              icon={<SyncIcon />}
              onClick={() => {
                void handleSync();
              }}
              loading={syncing}
              disabled={syncing}
            >
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
          {allTags.map((tag) => (
            <FilterChip
              key={tag}
              label={tag}
              count={tagCounts[tag] ?? 0}
              active={activeTag === tag}
              onClick={() => setActiveTag(tag)}
            />
          ))}
        </div>
      </div>

      {/* 加载错误提示条 */}
      {loadState === 'error' ? (
        <p className={styles.notice} role="status">
          {loadError ? t('prompts:loadFailed', { error: loadError }) : t('prompts:loadFailedGeneric')}
        </p>
      ) : null}

      {/* mutation 失败 + 重试 */}
      {mutationErrorText && failedMutation ? (
        <div className={styles.noticeBanner} data-testid="prompt-mutation-error" role="alert">
          <p className={styles.noticeText}>{mutationErrorText}</p>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              void retryFailedMutation();
            }}
          >
            {t('common:action.retry')}
          </Button>
        </div>
      ) : null}

      {/* 同步失败 */}
      {syncError ? (
        <div className={styles.noticeBanner} data-testid="prompt-sync-error" role="alert">
          <p className={styles.noticeText}>{t('prompts:syncFailed', { error: syncError })}</p>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              void handleSync();
            }}
          >
            {t('common:action.retry')}
          </Button>
        </div>
      ) : null}

      {/* 网格区 */}
      <section className={styles.gridSection}>
        {loadState === 'loading' && prompts.length === 0 ? (
          <GridSkeleton />
        ) : filtered.length === 0 && !creatingNew ? (
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
                  saving={pendingEntityIds.has(CREATE_PENDING_KEY)}
                  onChange={setDraft}
                  onSave={(event) => {
                    void saveDraft(event);
                  }}
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
                    saving={pendingEntityIds.has(p.id)}
                    onChange={setDraft}
                    onSave={(event) => {
                      void saveDraft(event);
                    }}
                    onCancel={cancelEdit}
                  />
                </li>
              ) : (
                <li key={p.id}>
                  <PromptCardView
                    prompt={p}
                    actionsDisabled={pendingEntityIds.has(p.id)}
                    hasConflict={conflictPromptIds.has(p.id)}
                    onEdit={() => startEdit(p)}
                    onHistory={() => {
                      void openVersionHistory(p.id);
                    }}
                    onDelete={() => {
                      if (pendingEntityIdsRef.current.has(p.id)) return;
                      setPendingDeleteId(p.id);
                    }}
                  />
                </li>
              ),
            )}
          </ul>
        )}
      </section>

      {/* 删除确认弹层：共享 Dialog（portal / Escape / focus trap） */}
      <Dialog
        open={Boolean(pendingDeleteId)}
        titleId="confirm-title"
        onClose={() => setPendingDeleteId(null)}
        className={styles.modal}
      >
        <Card variant="elevated" className={styles.modalCard}>
          <h3 id="confirm-title" className={styles.modalTitle}>
            {t('prompts:deleteTitle')}
          </h3>
          <p className={styles.modalText}>{t('prompts:deleteConfirm')}</p>
          <div className={styles.modalActions}>
            <Button variant="secondary" size="sm" onClick={() => setPendingDeleteId(null)}>
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              icon={<TrashIcon />}
              onClick={() => {
                void confirmDelete();
              }}
            >
              {t('common:action.delete')}
            </Button>
          </div>
        </Card>
      </Dialog>

      <VersionHistoryDrawer
        open={Boolean(historyPromptId)}
        onClose={closeVersionHistory}
        versions={versions}
        loading={versionsLoading}
        error={versionsError}
        restoringVersionId={restoringVersionId}
        i18nNamespace="prompts"
        onRestore={(version) => {
          void handleRestoreVersion(version);
        }}
        onCopy={(version) => {
          void handleCopyVersion(version);
        }}
      />
    </div>
  );
}

// ────────────────────────────────────────────────────────────────
// 子组件
// ────────────────────────────────────────────────────────────────

/**
 * Business Logic（为什么需要这个组件）:
 *   标签筛选需要可点击 chip 与计数角标。
 *
 * Code Logic（这个组件做什么）:
 *   渲染带 aria-pressed 的筛选按钮。
 */
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

/**
 * Business Logic（为什么需要这个组件）:
 *   展示态卡片提供编辑/删除/历史入口；pending 时必须禁用冲突动作；
 *   冲突用非阻塞 Pill 提示，不打断浏览与编辑。
 *
 * Code Logic（这个组件做什么）:
 *   渲染标题/内容/标签与动作按钮；hasConflict 时显示 warn Pill。
 */
function PromptCardView({
  prompt,
  actionsDisabled,
  hasConflict,
  onEdit,
  onHistory,
  onDelete,
}: {
  prompt: Prompt;
  actionsDisabled: boolean;
  hasConflict: boolean;
  onEdit: () => void;
  onHistory: () => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation(['prompts', 'common']);
  return (
    <Card
      variant="elevated"
      className={styles.promptCard}
      data-testid={`prompt-card-${prompt.id}`}
    >
      <Card.Header className={styles.promptHeader}>
        <div className={styles.promptTitleRow}>
          <h3 className={styles.promptTitle}>{prompt.title}</h3>
          {hasConflict ? (
            <Pill tone="warn" dot data-testid={`prompt-conflict-pill-${prompt.id}`}>
              {t('prompts:versionConflictPill')}
            </Pill>
          ) : null}
        </div>
        <div className={styles.promptActions}>
          <Button
            variant="ghost"
            size="sm"
            icon={<HistoryIcon />}
            onClick={onHistory}
            disabled={actionsDisabled}
            aria-label={t('prompts:versionOpenHistory')}
            title={t('prompts:versionOpenHistory')}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={<EditIcon />}
            onClick={onEdit}
            disabled={actionsDisabled}
            aria-label={t('common:action.edit')}
            title={t('common:action.edit')}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={<TrashIcon />}
            onClick={onDelete}
            disabled={actionsDisabled}
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
            {prompt.tags.map((tag) => (
              <Tag key={tag} size="sm">
                {tag}
              </Tag>
            ))}
          </div>
        ) : prompt.tag ? (
          <Tag size="sm">{prompt.tag}</Tag>
        ) : (
          <span />
        )}
      </Card.Footer>
    </Card>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   新建与编辑共用同一表单卡片；保存中必须禁用提交，防止双重点击。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 title/content/tags 表单与保存/取消按钮；saving 时禁用 submit。
 */
function EditPromptCard({
  draft,
  isNew,
  saving,
  onChange,
  onSave,
  onCancel,
}: {
  draft: DraftPrompt;
  isNew: boolean;
  saving: boolean;
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
            disabled={saving}
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
            disabled={saving}
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
            disabled={saving}
          >
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            icon={<CheckIcon />}
            type="submit"
            disabled={!draft.content.trim() || saving}
            loading={saving}
            aria-busy={saving || undefined}
          >
            {t('common:action.save')}
          </Button>
        </Card.Footer>
      </form>
    </Card>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   首屏加载时避免空白闪烁。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 6 个骨架卡片。
 */
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
