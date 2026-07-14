/**
 * CcHistory 页面 - Claude 历史会话用户输入 Prompt 浏览
 *
 * Business Logic（为什么需要这个页面）:
 *   Claude Code 在本地 ~/.claude/projects 下沉淀了用户输入的 prompt 历史，
 *   这些是宝贵的"真实使用记录"。本页面把它们按项目(cwd)分组、以时间线呈现，
 *   让用户搜索 / 复制 / 一键转存为正式 Prompt / 删除，并可手动刷新采集、跨设备同步。
 *   项目切换与搜索词变更会并发请求；必须丢弃逆序响应，且刷新/同步失败不得静默。
 *
 * Code Logic（这个页面做什么）:
 *   - 顶部 page header（eyebrow/title/lead）
 *   - 工具栏：「刷新采集」按钮（ccHistoryApi.refresh）+「同步」按钮（promptsApi.sync）
 *   - 主体双栏 grid：左栏项目列表（点击高亮选中）、右栏选中项目的 prompt 时间线（顶部搜索框）
 *   - 数据流：loadProjects → 进页默认选中第一个项目；selectedProjectPath/search 变化 → loadPrompts
 *   - 使用独立 projectGuard / promptGuard（createLatestRequestGuard）在 success/catch/finally
 *     写状态前校验 token+context；selectedProject 变为 null 时 invalidate promptGuard
 *   - 复制/转存：成功后顶部 toast 提示；刷新/同步失败同样 toast（非阻塞）
 *   - 删除：弹 confirm 二次确认，确认后乐观移除
 *   - hooks 全部声明在顶部、用条件渲染（三元）而非 early return（项目铁律）
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Input } from '@/components/primitives';
import { CcHistoryCard } from '@/components/domain';
import { ccHistoryApi } from '@/api/ccHistory';
import { promptsApi } from '@/api/prompts';
import type { CcProject, CcHistoryItem } from '@/lib/types';
import { SearchIcon, SyncIcon, TrashIcon, HistoryIcon } from '@/lib/icons';
import { debounce, formatRelativeTime } from '@/lib/format';
import {
  buildCcHistoryPromptContext,
  createLatestRequestGuard,
} from './ccHistoryRequestState';
import styles from './CcHistory.module.css';

type LoadState = 'loading' | 'success' | 'error';

/**
 * CcHistory 页面主组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在单一页面浏览/搜索/复制/转存 Claude 历史 prompt，并安全处理并发加载。
 *
 * Code Logic（这个组件做什么）:
 *   维护 projects/prompts 双列表状态，经 latest-request 守卫加载，渲染双栏 UI。
 */
export function CcHistory() {
  const { t, i18n } = useTranslation(['ccHistory', 'common']);

  // ── 项目列表 ──
  const [projects, setProjects] = useState<CcProject[]>([]);
  const [projectsLoadState, setProjectsLoadState] = useState<LoadState>('loading');
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | null>(null);
  const [projectSearch, setProjectSearch] = useState('');

  // ── prompt 列表 ──
  const [prompts, setPrompts] = useState<CcHistoryItem[]>([]);
  const [promptsLoadState, setPromptsLoadState] = useState<LoadState>('loading');
  const [promptsError, setPromptsError] = useState<string | null>(null);

  // ── 搜索（300ms debounce）──
  const [searchInput, setSearchInput] = useState('');
  const [search, setSearch] = useState('');

  // ── 操作态 ──
  const [refreshing, setRefreshing] = useState(false);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── 独立 latest-request 守卫（项目列表 / prompt 列表）──
  const projectGuardRef = useRef(createLatestRequestGuard<null>());
  const promptGuardRef = useRef(createLatestRequestGuard<string>());

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户进入页面或刷新后需要看到按 cwd 聚合的项目列表，并默认选中可继续浏览的项目。
   *
   * Code Logic（这个函数做什么）:
   *   begin projectGuard → listProjects；仅 isCurrent 时写 projects/error/loadState；
   *   成功后若当前选中失效则回落到列表首项。
   */
  const loadProjects = useCallback(async () => {
    const token = projectGuardRef.current.begin(null);
    setProjectsLoadState('loading');
    try {
      const data = await ccHistoryApi.listProjects();
      if (!projectGuardRef.current.isCurrent(token, null)) return;
      const list = Array.isArray(data) ? data : [];
      setProjects(list);
      setProjectsLoadState('success');
      setProjectsError(null);
      setSelectedProjectPath((prev) => {
        if (prev && list.some((p) => p.projectPath === prev)) return prev;
        return list.length > 0 ? list[0].projectPath : null;
      });
    } catch (err) {
      if (!projectGuardRef.current.isCurrent(token, null)) return;
      setProjectsLoadState('error');
      setProjectsError(err instanceof Error ? err.message : t('ccHistory:loadFailedGeneric'));
    }
  }, [t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   选中项目或搜索词变化后，右栏必须只展示该上下文的 prompt，旧请求不得覆盖。
   *
   * Code Logic（这个函数做什么）:
   *   以 buildCcHistoryPromptContext 作 context begin promptGuard；listPrompts 后
   *   在 success/catch 写状态前 isCurrent 校验。
   */
  const loadPrompts = useCallback(
    async (projectPath: string, searchTerm?: string) => {
      const context = buildCcHistoryPromptContext(projectPath, searchTerm);
      const token = promptGuardRef.current.begin(context);
      setPromptsLoadState('loading');
      try {
        const data = await ccHistoryApi.listPrompts(projectPath, searchTerm);
        if (!promptGuardRef.current.isCurrent(token, context)) return;
        setPrompts(Array.isArray(data) ? data : []);
        setPromptsLoadState('success');
        setPromptsError(null);
      } catch (err) {
        if (!promptGuardRef.current.isCurrent(token, context)) return;
        setPromptsLoadState('error');
        setPromptsError(err instanceof Error ? err.message : t('ccHistory:loadFailedGeneric'));
      }
    },
    [t],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    if (!selectedProjectPath) {
      promptGuardRef.current.invalidate();
      setPrompts([]);
      setPromptsLoadState('success');
      setPromptsError(null);
      return;
    }
    void loadPrompts(selectedProjectPath, search || undefined);
  }, [selectedProjectPath, search, loadPrompts]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // ── 搜索 300ms debounce ──
  const debouncedSetSearch = useMemo(
    () =>
      debounce((v: unknown) => {
        if (typeof v === 'string') setSearch(v);
      }, 300),
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   搜索输入需即时回显，但请求应 debounce 避免每个按键打 API。
   *
   * Code Logic（这个函数做什么）:
   *   更新 searchInput，并经 300ms debounce 写入 search 触发 loadPrompts。
   */
  const handleSearchInput = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const v = e.target.value;
      setSearchInput(v);
      debouncedSetSearch(v);
    },
    [debouncedSetSearch],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   复制/转存/刷新/同步成功或失败都需要非阻塞反馈。
   *
   * Code Logic（这个函数做什么）:
   *   设置 toast 文案，2.4s 后自动清空。
   */
  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 2400);
  }, []);

  useEffect(() => {
    return () => {
      if (toastTimer.current) clearTimeout(toastTimer.current);
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击「刷新采集」时需重新扫描本地 Claude 会话并反馈结果；失败不得静默。
   *
   * Code Logic（这个函数做什么）:
   *   调 refresh；成功后经受保护 loader 刷新 projects/prompts 并 toast 采集数；
   *   失败 toast 错误文案；finally 仅在仍刷新中时清 refreshing（避免并发覆盖）。
   */
  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const res = await ccHistoryApi.refresh();
      // 采集完成后刷新项目 + 当前选中项目的 prompt（复用带守卫的 loader）
      await loadProjects();
      if (selectedProjectPath) {
        await loadPrompts(selectedProjectPath, search || undefined);
      }
      if (res?.ok) {
        showToast(t('ccHistory:refreshDone', { count: res.collected }));
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : t('ccHistory:refreshFailedGeneric');
      showToast(t('ccHistory:refreshFailed', { error: message }));
    } finally {
      setRefreshing(false);
    }
  }, [loadProjects, loadPrompts, selectedProjectPath, search, showToast, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击「同步」时需触发跨设备同步并刷新历史；失败不得静默。
   *
   * Code Logic（这个函数做什么）:
   *   调 promptsApi.sync；成功后经受保护 loader 刷新；失败 toast。
   */
  const handleSync = useCallback(async () => {
    try {
      await promptsApi.sync();
      await loadProjects();
      if (selectedProjectPath) {
        await loadPrompts(selectedProjectPath, search || undefined);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : t('ccHistory:syncFailedGeneric');
      showToast(t('ccHistory:syncFailed', { error: message }));
    }
  }, [loadProjects, loadPrompts, selectedProjectPath, search, showToast, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   复制成功需要即时反馈。
   *
   * Code Logic（这个函数做什么）:
   *   toast 已复制文案。
   */
  const handleCopied = useCallback(() => {
    showToast(t('ccHistory:copied'));
  }, [showToast, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户希望把历史 prompt 一键转存到 Prompt 库。
   *
   * Code Logic（这个函数做什么）:
   *   用 content 前 40 字做 title 调 promptsApi.create；成功 toast。
   */
  const handleSaveAsPrompt = useCallback(
    async (item: CcHistoryItem) => {
      try {
        const title = item.content.slice(0, 40).trim() || item.content.slice(0, 40);
        await promptsApi.create({ title, content: item.content, tags: [] });
        showToast(t('ccHistory:savedAsPrompt'));
      } catch {
        // 静默失败（转存非本任务范围）
      }
    },
    [showToast, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   删除需二次确认，防止误触。
   *
   * Code Logic（这个函数做什么）:
   *   记录 pendingDeleteId 触发确认弹层。
   */
  const handleRequestDelete = useCallback((item: CcHistoryItem) => {
    setPendingDeleteId(item.id);
  }, []);

  // ── 确认删除：乐观移除 ──
  const pendingItem = useMemo(
    () => prompts.find((p) => p.id === pendingDeleteId) ?? null,
    [prompts, pendingDeleteId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后应从时间线移除该历史 prompt。
   *
   * Code Logic（这个函数做什么）:
   *   乐观从 prompts/projects 计数移除，再调 remove；失败静默（非本任务范围）。
   */
  const confirmDelete = useCallback(async () => {
    if (!pendingDeleteId) return;
    const id = pendingDeleteId;
    setPrompts((prev) => prev.filter((p) => p.id !== id));
    setProjects((prev) =>
      prev.map((p) =>
        p.projectPath === selectedProjectPath
          ? { ...p, count: Math.max(0, p.count - 1) }
          : p,
      ),
    );
    setPendingDeleteId(null);
    try {
      await ccHistoryApi.remove(id);
    } catch {
      // 静默失败
    }
  }, [pendingDeleteId, selectedProjectPath]);

  // ── 选中项目对象（用于右栏标题等）──
  const selectedProject = useMemo(
    () => projects.find((p) => p.projectPath === selectedProjectPath) ?? null,
    [projects, selectedProjectPath],
  );

  // ── 项目筛选：按项目名与绝对路径本地匹配 ──
  const visibleProjects = useMemo(() => {
    const keyword = projectSearch.trim().toLowerCase();
    if (!keyword) return projects;
    return projects.filter((p) => {
      const name = p.projectName.toLowerCase();
      const path = p.projectPath.toLowerCase();
      return name.includes(keyword) || path.includes(keyword);
    });
  }, [projectSearch, projects]);

  const lang = i18n.language === 'zh' ? 'zh' : 'en';

  // ── 渲染 ──
  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>
          {t('ccHistory:eyebrow', { count: projects.reduce((s, p) => s + p.count, 0) })}
        </span>
        <h1 className={styles.title}>{t('ccHistory:title')}</h1>
        <p className={styles.lead}>{t('ccHistory:subtitle')}</p>
      </header>

      {/* 工具栏 */}
      <div className={styles.toolbar}>
        <div className={styles.toolbarActions}>
          <Button
            variant="secondary"
            size="sm"
            icon={<HistoryIcon />}
            onClick={handleRefresh}
            loading={refreshing}
            disabled={refreshing}
          >
            {refreshing ? t('ccHistory:refreshing') : t('ccHistory:refresh')}
          </Button>
          <Button variant="secondary" size="sm" icon={<SyncIcon />} onClick={handleSync}>
            {t('ccHistory:sync')}
          </Button>
        </div>
      </div>

      {/* 错误提示条（项目级）*/}
      {projectsLoadState === 'error' ? (
        <p className={styles.notice} role="status">
          {projectsError
            ? t('ccHistory:loadFailed', { error: projectsError })
            : t('ccHistory:loadFailedGeneric')}
        </p>
      ) : null}

      {/* 主体双栏 */}
      <section className={styles.body}>
        {/* 左栏：项目列表 */}
        <aside className={styles.sidebar} aria-label={t('ccHistory:projectListAriaLabel')}>
          {projects.length > 0 ? (
            <div className={styles.projectSearch}>
              <Input
                type="search"
                size="sm"
                value={projectSearch}
                onChange={(e) => setProjectSearch(e.target.value)}
                placeholder={t('ccHistory:projectSearchPlaceholder')}
                icon={<SearchIcon />}
                aria-label={t('ccHistory:projectSearchAriaLabel')}
              />
              {projectSearch.trim() ? (
                <span className={styles.projectSearchMeta}>
                  {t('ccHistory:projectSearchCount', { count: visibleProjects.length })}
                </span>
              ) : null}
            </div>
          ) : null}
          {projectsLoadState === 'loading' && projects.length === 0 ? (
            <ul className={styles.projectList} aria-busy="true">
              {[0, 1, 2, 3].map((i) => (
                <li key={i} className={styles.projectSkeleton}>
                  <span className={styles.skeletonBlock} style={{ width: '70%', height: 14 }} />
                  <span className={styles.skeletonBlock} style={{ width: '40%', height: 11 }} />
                </li>
              ))}
            </ul>
          ) : projects.length === 0 ? (
            <div className={styles.empty}>
              <p>{t('ccHistory:emptyProjects')}</p>
              <p className={styles.emptyHint}>{t('ccHistory:emptyProjectsHint')}</p>
            </div>
          ) : visibleProjects.length === 0 ? (
            <div className={styles.empty}>
              <p>{t('ccHistory:emptyProjectSearch')}</p>
              <p className={styles.emptyHint}>{t('ccHistory:emptyProjectSearchHint')}</p>
            </div>
          ) : (
            <ul className={styles.projectList}>
              {visibleProjects.map((p) => {
                const active = p.projectPath === selectedProjectPath;
                return (
                  <li key={p.projectPath}>
                    <button
                      type="button"
                      className={[styles.projectItem, active ? styles.projectItemActive : '']
                        .filter(Boolean)
                        .join(' ')}
                      onClick={() => setSelectedProjectPath(p.projectPath)}
                      aria-pressed={active}
                      title={p.projectPath}
                    >
                      <div className={styles.projectMain}>
                        <span className={styles.projectName}>{p.projectName}</span>
                        <span className={styles.projectPath}>{p.projectPath}</span>
                      </div>
                      <div className={styles.projectMeta}>
                        <span className={styles.projectCount}>{p.count}</span>
                        <span className={styles.projectTime}>
                          {t('ccHistory:lastOccurred', { time: formatRelativeTime(p.lastOccurredAt, lang) })}
                        </span>
                      </div>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </aside>

        {/* 右栏：prompt 时间线 */}
        <div className={styles.detail} aria-label={t('ccHistory:promptListAriaLabel')}>
          {/* 搜索框 */}
          {selectedProject ? (
            <div className={styles.searchWrap}>
              <Input
                type="search"
                value={searchInput}
                onChange={handleSearchInput}
                placeholder={t('ccHistory:searchPlaceholder')}
                icon={<SearchIcon />}
                aria-label={t('ccHistory:searchAriaLabel')}
                className={styles.search}
              />
              <span className={styles.detailCount}>
                {t('ccHistory:promptCount', { count: prompts.length })}
              </span>
            </div>
          ) : null}

          {/* 错误提示（prompt 级）*/}
          {promptsLoadState === 'error' ? (
            <p className={styles.notice} role="status">
              {promptsError
                ? t('ccHistory:loadFailed', { error: promptsError })
                : t('ccHistory:loadFailedGeneric')}
            </p>
          ) : null}

          {/* 时间线列表 */}
          {!selectedProject ? (
            <div className={styles.empty}>
              <p>{t('ccHistory:emptyProjects')}</p>
              <p className={styles.emptyHint}>{t('ccHistory:emptyProjectsHint')}</p>
            </div>
          ) : promptsLoadState === 'loading' && prompts.length === 0 ? (
            <TimelineSkeleton />
          ) : prompts.length === 0 ? (
            <div className={styles.empty}>
              <p>{t('ccHistory:emptyPrompts')}</p>
              <p className={styles.emptyHint}>{t('ccHistory:emptyPromptsHint')}</p>
            </div>
          ) : (
            <ul className={styles.timeline}>
              {prompts.map((item) => (
                <li key={item.id}>
                  <CcHistoryCard
                    item={item}
                    onCopied={handleCopied}
                    onSaveAsPrompt={handleSaveAsPrompt}
                    onRequestDelete={handleRequestDelete}
                  />
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>

      {/* toast */}
      {toast ? (
        <div className={styles.toast} role="status" aria-live="polite">
          {toast}
        </div>
      ) : null}

      {/* 删除确认弹层：共享 Dialog（portal / Escape / focus trap） */}
      <Dialog
        open={Boolean(pendingDeleteId)}
        titleId="cc-confirm-title"
        onClose={() => setPendingDeleteId(null)}
        className={styles.modal}
      >
        <Card variant="elevated" className={styles.modalCard}>
          <h3 id="cc-confirm-title" className={styles.modalTitle}>
            {t('ccHistory:deleteTitle')}
          </h3>
          <p className={styles.modalText}>{t('ccHistory:confirmDeleteText')}</p>
          {pendingItem ? (
            <p className={styles.modalPreview}>{pendingItem.content.slice(0, 120)}</p>
          ) : null}
          <div className={styles.modalActions}>
            <Button variant="secondary" size="sm" onClick={() => setPendingDeleteId(null)}>
              {t('common:action.cancel')}
            </Button>
            <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={confirmDelete}>
              {t('common:action.delete')}
            </Button>
          </div>
        </Card>
      </Dialog>
    </div>
  );
}

// ────────────────────────────────────────────────────────────────
// 子组件
// ────────────────────────────────────────────────────────────────

/**
 * Business Logic（为什么需要这个组件）:
 *   首屏/切换项目时右栏应显示骨架，避免空白闪烁。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 4 条时间线骨架块。
 */
function TimelineSkeleton() {
  const { t } = useTranslation(['ccHistory']);
  return (
    <ul className={styles.timeline} aria-busy="true" aria-label={t('ccHistory:skeletonAriaLabel')}>
      {[0, 1, 2, 3].map((i) => (
        <li key={i} className={styles.cardSkeleton}>
          <span className={styles.skeletonBlock} style={{ width: '40%', height: 11 }} />
          <span className={styles.skeletonBlock} style={{ width: '95%', height: 13 }} />
          <span className={styles.skeletonBlock} style={{ width: '85%', height: 13 }} />
        </li>
      ))}
    </ul>
  );
}
