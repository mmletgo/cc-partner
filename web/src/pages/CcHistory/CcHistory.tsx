/**
 * CcHistory 页面 - Claude 历史会话用户输入 Prompt 浏览
 *
 * Business Logic（为什么需要这个页面）:
 *   Claude Code 在本地 ~/.claude/projects 下沉淀了用户输入的 prompt 历史，
 *   这些是宝贵的"真实使用记录"。本页面把它们按项目(cwd)分组、以时间线呈现，
 *   让用户搜索 / 复制 / 一键转存为正式 Prompt / 删除，并可手动刷新采集、跨设备同步。
 *
 * Code Logic（这个页面做什么）:
 *   - 顶部 page header（eyebrow/title/lead）
 *   - 工具栏：「刷新采集」按钮（ccHistoryApi.refresh）+「同步」按钮（promptsApi.sync）
 *   - 主体双栏 grid：左栏项目列表（点击高亮选中）、右栏选中项目的 prompt 时间线（顶部搜索框）
 *   - 数据流：loadProjects → 进页默认选中第一个项目；selectedProjectPath/search 变化 → loadPrompts
 *   - 复制/转存：成功后顶部 toast 提示
 *   - 删除：弹 confirm 二次确认，确认后乐观移除
 *   - hooks 全部声明在顶部、用条件渲染（三元）而非 early return（项目铁律）
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input } from '@/components/primitives';
import { CcHistoryCard } from '@/components/domain';
import { ccHistoryApi } from '@/api/ccHistory';
import { promptsApi } from '@/api/prompts';
import type { CcProject, CcHistoryItem } from '@/lib/types';
import { SearchIcon, SyncIcon, TrashIcon, HistoryIcon } from '@/lib/icons';
import { debounce, formatRelativeTime } from '@/lib/format';
import styles from './CcHistory.module.css';

type LoadState = 'loading' | 'success' | 'error';

/**
 * CcHistory 页面主组件
 */
export function CcHistory() {
  const { t, i18n } = useTranslation(['ccHistory', 'common']);

  // ── 项目列表 ──
  const [projects, setProjects] = useState<CcProject[]>([]);
  const [projectsLoadState, setProjectsLoadState] = useState<LoadState>('loading');
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | null>(null);

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

  /**
   * 拉取项目列表；成功后若当前未选中且列表非空，默认选中第一个
   */
  const loadProjects = useCallback(async () => {
    setProjectsLoadState('loading');
    try {
      const data = await ccHistoryApi.listProjects();
      const list = Array.isArray(data) ? data : [];
      setProjects(list);
      setProjectsLoadState('success');
      setProjectsError(null);
      setSelectedProjectPath((prev) => {
        if (prev && list.some((p) => p.projectPath === prev)) return prev;
        return list.length > 0 ? list[0].projectPath : null;
      });
    } catch (err) {
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
   * 拉取选中项目的 prompt 列表（带可选搜索词）
   */
  const loadPrompts = useCallback(
    async (projectPath: string, search?: string) => {
      setPromptsLoadState('loading');
      try {
        const data = await ccHistoryApi.listPrompts(projectPath, search);
        setPrompts(Array.isArray(data) ? data : []);
        setPromptsLoadState('success');
        setPromptsError(null);
      } catch (err) {
        setPromptsLoadState('error');
        setPromptsError(err instanceof Error ? err.message : t('ccHistory:loadFailedGeneric'));
      }
    },
    [t],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    if (!selectedProjectPath) {
      setPrompts([]);
      setPromptsLoadState('success');
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

  const handleSearchInput = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const v = e.target.value;
      setSearchInput(v);
      debouncedSetSearch(v);
    },
    [debouncedSetSearch],
  );

  // ── toast 短暂提示（2.4s 后自动消失）──
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

  // ── 立即刷新采集 ──
  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const res = await ccHistoryApi.refresh();
      // 采集完成后刷新项目 + 当前选中项目的 prompt
      await loadProjects();
      if (selectedProjectPath) {
        await loadPrompts(selectedProjectPath, search || undefined);
      }
      if (res?.ok) {
        showToast(t('ccHistory:refreshDone', { count: res.collected }));
      }
    } catch {
      // 静默失败
    } finally {
      setRefreshing(false);
    }
  }, [loadProjects, loadPrompts, selectedProjectPath, search, showToast, t]);

  // ── 跨设备同步（复用 trigger_sync）──
  const handleSync = useCallback(async () => {
    try {
      await promptsApi.sync();
      await loadProjects();
      if (selectedProjectPath) {
        await loadPrompts(selectedProjectPath, search || undefined);
      }
    } catch {
      // 静默失败
    }
  }, [loadProjects, loadPrompts, selectedProjectPath, search]);

  // ── 复制成功 toast ──
  const handleCopied = useCallback(() => {
    showToast(t('ccHistory:copied'));
  }, [showToast, t]);

  // ── 转存为 Prompt：用 content 前 40 字做 title ──
  const handleSaveAsPrompt = useCallback(
    async (item: CcHistoryItem) => {
      try {
        const title = item.content.slice(0, 40).trim() || item.content.slice(0, 40);
        await promptsApi.create({ title, content: item.content, tags: [] });
        showToast(t('ccHistory:savedAsPrompt'));
      } catch {
        // 静默失败
      }
    },
    [showToast, t],
  );

  // ── 请求删除：记录 id 触发确认弹层 ──
  const handleRequestDelete = useCallback((item: CcHistoryItem) => {
    setPendingDeleteId(item.id);
  }, []);

  // ── 确认删除：乐观移除 ──
  const pendingItem = useMemo(
    () => prompts.find((p) => p.id === pendingDeleteId) ?? null,
    [prompts, pendingDeleteId],
  );

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
          ) : (
            <ul className={styles.projectList}>
              {projects.map((p) => {
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

      {/* 删除确认弹层 */}
      {pendingDeleteId ? (
        <div
          className={styles.modalMask}
          role="dialog"
          aria-modal="true"
          aria-labelledby="cc-confirm-title"
        >
          <Card variant="elevated" className={styles.modal}>
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
        </div>
      ) : null}
    </div>
  );
}

// ────────────────────────────────────────────────────────────────
// 子组件
// ────────────────────────────────────────────────────────────────

/** 时间线骨架屏 */
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
