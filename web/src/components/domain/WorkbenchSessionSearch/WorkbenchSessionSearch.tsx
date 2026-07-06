/**
 * WorkbenchSessionSearch 业务组件 - v3 中央 Command Palette
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在 Workbench 终端里经常在多个 Claude Code 会话之间工作，但磁盘上的 session
 *   是无意义的 jsonl 文件名，无法快速找回之前某段对话继续。本组件提供一个类似
 *   Spotlight/VS Code Command Palette 的浮层，让用户在当前 worktree 范围内按标题
 *   或对话内容搜索 Claude session，预览最近对话后一键新建 window 执行 resume。
 *
 * Code Logic（这个组件做什么）:
 *   受控浮层（open/onClose），内部维护 query/hits/activeIndex/preview 等 state；
 *   useEffect 监听 query+scope 做 300ms debounce 搜索；input 的 onKeyDown 处理
 *   ↑↓ 导航 / ⏎ 进入 preview / esc 返回或关闭；选中后切到 preview 视图渲染最近
 *   20 条对话，底部 resume 按钮回调父组件刷新 sessions 并 focus 新 window。
 *   hooks 全部声明在 early return 之前（AGENTS.md 第 20 条）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { workbenchApi } from '@/api/workbench';
import { Button } from '@/components/primitives';
import { SearchIcon } from '@/lib/icons';
import type { SessionPreview, SessionSearchHit } from '@/lib/types';
import styles from './WorkbenchSessionSearch.module.css';

export interface WorkbenchSessionSearchProps {
  /** 是否展开浮层 */
  open: boolean;
  /** 关闭浮层回调 */
  onClose: () => void;
  /** 当前项目 id（决定搜索命令的 projectId 参数） */
  projectId: string | null;
  /** 当前 worktree id（决定搜索范围，可能为 null 表示主工作区） */
  worktreeId: string | null;
  /** 是否远端项目（影响离线态/错误态提示） */
  isRemote: boolean;
  /** 当前 worktree 显示名（用于分组标签），无则回退 */
  worktreeName?: string | null;
  /** resume 成功后回调，父组件刷新 sessions + focusSession 到新 window */
  onResumed: (newSessionId: string) => void;
}

/** 用于命中高亮的纯文本片段 */
interface HighlightSegment {
  /** 是否为命中片段 */
  hit: boolean;
  /** 该段文本 */
  text: string;
}

/** 分钟 / 小时 / 天 的毫秒数 */
const MIN_MS = 60 * 1000;
const HOUR_MS = 60 * MIN_MS;
const DAY_MS = 24 * HOUR_MS;
/** debounce 延迟（毫秒） */
const SEARCH_DEBOUNCE_MS = 300;
/** 视为「新鲜」session 的时间窗口（最近 1 小时） */
const FRESH_THRESHOLD_MS = HOUR_MS;

/**
 * 把 ISO 时间戳格式化为中文相对时间（"刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 日期"）。
 *
 * @param iso ISO 时间字符串
 * @returns 形如 "2小时前" 的相对时间；解析失败回退原值
 */
function formatRelativeTime(iso: string, locale: string): string {
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return iso;
  const diff = Date.now() - ts;
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  if (diff < MIN_MS) return rtf.format(-Math.round(diff / 1000), 'second');
  if (diff < HOUR_MS) return rtf.format(-Math.round(diff / MIN_MS), 'minute');
  if (diff < DAY_MS) return rtf.format(-Math.round(diff / HOUR_MS), 'hour');
  if (diff < 7 * DAY_MS) return rtf.format(-Math.round(diff / DAY_MS), 'day');
  // 超过一周回退到日期
  return new Date(ts).toLocaleDateString(locale);
}

/**
 * 把字符串按 query 大小写不敏感地拆分为命中/非命中文本片段。
 *
 * @param text 原始文本
 * @param query 搜索关键词（为空则整体返回单个非命中介质）
 * @returns 片段数组，命中片段 hit=true
 */
function splitHighlightSegments(text: string, query: string): HighlightSegment[] {
  const trimmed = query.trim();
  if (!trimmed) return [{ hit: false, text }];
  // 转义正则元字符
  const escaped = trimmed.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(escaped, 'gi');
  const segments: HighlightSegment[] = [];
  let lastIndex = 0;
  let match = re.exec(text);
  while (match !== null) {
    if (match.index > lastIndex) {
      segments.push({ hit: false, text: text.slice(lastIndex, match.index) });
    }
    segments.push({ hit: true, text: match[0] });
    lastIndex = match.index + match[0].length;
    match = re.exec(text);
  }
  if (lastIndex < text.length) {
    segments.push({ hit: false, text: text.slice(lastIndex) });
  }
  return segments.length === 0 ? [{ hit: false, text }] : segments;
}

/**
 * 渲染带高亮的文本：query 命中片段用 <mark> 包裹。
 *
 * @param text 原始文本
 * @param query 搜索关键词
 * @returns React 节点
 */
function renderHighlighted(text: string, query: string): ReactNode {
  const segments = splitHighlightSegments(text, query);
  return segments.map((seg, idx) =>
    seg.hit ? (
      <mark key={idx} className={styles.mark}>
        {seg.text}
      </mark>
    ) : (
      <span key={idx}>{seg.text}</span>
    ),
  );
}

/**
 * 渲染 WorkbenchSessionSearch Command Palette 浮层。
 *
 * Business Logic（为什么需要这个函数）:
 *   把搜索/preview/resume 三个交互串成一个浮层，让用户在不离开 Workbench 终端的
 *   情况下快速找回并继续历史 Claude 会话。
 *
 * Code Logic（这个函数做什么）:
 *   声明全部 state/hook 后 early return；监听 query+scope 做 debounce 搜索；
 *   根据 previewHit 是否为空在「搜索视图」与「preview 视图」之间切换渲染。
 */
export function WorkbenchSessionSearch(props: WorkbenchSessionSearchProps): ReactNode {
  const { t, i18n } = useTranslation(['workbench', 'common']);
  const locale = i18n.language?.startsWith('zh') ? 'zh' : 'en';
  const { open, onClose, projectId, worktreeId, isRemote, worktreeName, onResumed } = props;

  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<SessionSearchHit[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [previewHit, setPreviewHit] = useState<SessionSearchHit | null>(null);
  const [previewData, setPreviewData] = useState<SessionPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [resuming, setResuming] = useState(false);

  const inputRef = useRef<HTMLInputElement>(null);
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** 标记本次 debounce 触发的搜索请求是否已过期（避免竞态覆盖最新结果） */
  const searchSeqRef = useRef(0);
  /** 标记 preview 请求序号，避免竞态 */
  const previewSeqRef = useRef(0);

  /**
   * 执行一次搜索请求（供 debounce 与重试按钮复用）。
   * 用序号防竞态：仅最后一次请求的结果会被写入 state。
   */
  const runSearch = useCallback(() => {
    if (!projectId || !open) return;
    const seq = ++searchSeqRef.current;
    setLoading(true);
    setError(null);
    workbenchApi.claudeSessions
      .search(projectId, worktreeId, query)
      .then((result) => {
        if (searchSeqRef.current !== seq) return;
        setHits(result);
        setActiveIndex(0);
      })
      .catch((err: unknown) => {
        if (searchSeqRef.current !== seq) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message || t('workbench:sessionSearch.error'));
        setHits([]);
      })
      .finally(() => {
        if (searchSeqRef.current !== seq) return;
        setLoading(false);
      });
  }, [projectId, worktreeId, query, open, t]);

  /**
   * debounce 搜索：query / scope / open 变化时延迟 300ms 触发。
   * open 为 true 时才发起搜索；切换为 false 时不触发。
   */
  useEffect(() => {
    if (!open) return undefined;
    if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current);
    debounceTimerRef.current = setTimeout(() => {
      runSearch();
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current);
    };
  }, [open, runSearch]);

  /** 打开浮层时清空 preview 视图并聚焦输入框 */
  useEffect(() => {
    if (!open) return;
    setPreviewHit(null);
    setPreviewData(null);
    setPreviewError(null);
    // 入场动画后聚焦
    const focusTimer = setTimeout(() => inputRef.current?.focus(), 30);
    return () => clearTimeout(focusTimer);
  }, [open]);

  /** 关闭浮层时重置 query（下次打开从空搜索开始） */
  useEffect(() => {
    if (open) return;
    setQuery('');
    setHits([]);
    setError(null);
  }, [open]);

  /**
   * 进入 preview：拉取该 session 的最近 20 条对话。
   */
  const openPreview = useCallback(
    (hit: SessionSearchHit) => {
      if (!projectId) return;
      const seq = ++previewSeqRef.current;
      setPreviewHit(hit);
      setPreviewData(null);
      setPreviewError(null);
      setPreviewLoading(true);
      workbenchApi.claudeSessions
        .preview(projectId, worktreeId, hit.sessionId)
        .then((result) => {
          if (previewSeqRef.current !== seq) return;
          setPreviewData(result);
        })
        .catch((err: unknown) => {
          if (previewSeqRef.current !== seq) return;
          const message = err instanceof Error ? err.message : String(err);
          setPreviewError(message || t('workbench:sessionSearch.error'));
        })
        .finally(() => {
          if (previewSeqRef.current !== seq) return;
          setPreviewLoading(false);
        });
    },
    [projectId, worktreeId, t],
  );

  /**
   * 执行 resume：新建 window 并注入 claude --resume 命令，成功后通知父组件。
   */
  const handleResume = useCallback(() => {
    if (!projectId || !previewHit || resuming) return;
    setResuming(true);
    workbenchApi.claudeSessions
      .resume(projectId, worktreeId, previewHit.sessionId)
      .then((result) => {
        if (result?.ok && result.sessionId) {
          onResumed(result.sessionId);
        } else {
          setPreviewError(t('workbench:sessionSearch.resumeFailed'));
        }
      })
      .catch((err: unknown) => {
        const message = err instanceof Error ? err.message : String(err);
        setPreviewError(message || t('workbench:sessionSearch.resumeFailed'));
      })
      .finally(() => setResuming(false));
  }, [projectId, worktreeId, previewHit, resuming, onResumed, t]);

  /**
   * 列表视图键盘导航：↑↓ 移动高亮、⏎ 进入 preview、esc 返回/关闭。
   */
  const handleListKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'ArrowDown') {
        if (hits.length === 0) return;
        e.preventDefault();
        setActiveIndex((idx) => (idx + 1) % hits.length);
      } else if (e.key === 'ArrowUp') {
        if (hits.length === 0) return;
        e.preventDefault();
        setActiveIndex((idx) => (idx - 1 + hits.length) % hits.length);
      } else if (e.key === 'Enter') {
        const target = hits[activeIndex];
        if (target) {
          e.preventDefault();
          openPreview(target);
        }
      } else if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    },
    [hits, activeIndex, openPreview, onClose],
  );

  /** preview 视图键盘：esc 返回列表 */
  const handlePreviewKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        setPreviewHit(null);
        setPreviewData(null);
        setPreviewError(null);
        // 返回列表后重新聚焦输入框
        setTimeout(() => inputRef.current?.focus(), 30);
      }
    },
    [],
  );

  /** 点击 scrim 关闭浮层 */
  const handleScrimClick = useCallback(() => {
    onClose();
  }, [onClose]);

  /** 点击 palette 内部阻止冒泡到 scrim */
  const stopPropagation = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
  }, []);

  const trimmedQuery = query.trim();
  const resultCountText = useMemo(
    () =>
      t('workbench:sessionSearch.resultCount', {
        count: hits.length,
        defaultValue: '{{count}}',
      }),
    [hits.length, t],
  );

  // early return 必须在所有 hooks 之后（AGENTS.md 第 20 条）
  if (!open) return null;

  const renderBody = (): ReactNode => {
    // 远端离线态（isRemote 且无 projectId 不可能，这里仅做兜底文案层）
    // ── 三态：loading / error / empty / results ──
    if (loading && hits.length === 0) {
      return (
        <div className={styles.stateBox}>
          <div className={styles.stateTitle}>{t('workbench:sessionSearch.loadingScan')}</div>
        </div>
      );
    }
    if (error) {
      return (
        <div className={styles.stateBox}>
          <div className={styles.stateErrorTitle}>{t('workbench:sessionSearch.error')}</div>
          <div className={styles.stateHint}>{error}</div>
          <Button variant="secondary" size="sm" onClick={runSearch}>
            {t('workbench:sessionSearch.retry')}
          </Button>
        </div>
      );
    }
    if (hits.length === 0) {
      return (
        <div className={styles.stateBox}>
          <div className={styles.stateTitle}>{t('workbench:sessionSearch.empty')}</div>
          <div className={styles.stateHint}>{t('workbench:sessionSearch.emptyHint')}</div>
        </div>
      );
    }
    return (
      <>
        <div className={styles.groupLabel}>
          {t('workbench:sessionSearch.groupRecent', {
            name: worktreeName || 'main',
            defaultValue: worktreeName || 'main',
          })}
        </div>
        <div className={styles.body} role="listbox" aria-label={t('workbench:sessionSearch.open')}>
          {hits.map((hit, idx) => {
            const fresh = Date.now() - Date.parse(hit.lastActivityAt) < FRESH_THRESHOLD_MS;
            const time = formatRelativeTime(hit.lastActivityAt, locale);
            const shortId = hit.sessionId.slice(0, 8);
            const preview = hit.previewSnippets[0] ?? '';
            return (
              <div
                key={hit.sessionId}
                className={styles.resultItem}
                role="option"
                aria-selected={idx === activeIndex}
                data-active={idx === activeIndex || undefined}
                tabIndex={-1}
                onMouseEnter={() => setActiveIndex(idx)}
                onClick={() => openPreview(hit)}
              >
                <span className={styles.timelineDot} data-fresh={fresh || undefined} />
                <div className={styles.resultMain}>
                  <div className={styles.resultTitle}>{renderHighlighted(hit.title, trimmedQuery)}</div>
                  {preview ? (
                    <div className={styles.resultPreview}>{renderHighlighted(preview, trimmedQuery)}</div>
                  ) : null}
                </div>
                <div className={styles.resultAside}>
                  <span>{time}</span>
                  <span>
                    {hit.messageCount} · {shortId}
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </>
    );
  };

  const renderPreviewBody = (): ReactNode => {
    if (previewLoading) {
      return (
        <div className={styles.stateBox}>
          <div className={styles.stateTitle}>{t('workbench:sessionSearch.loadingPreview')}</div>
        </div>
      );
    }
    if (previewError) {
      return (
        <div className={styles.stateBox}>
          <div className={styles.stateErrorTitle}>{t('workbench:sessionSearch.resumeFailed')}</div>
          <div className={styles.stateHint}>{previewError}</div>
        </div>
      );
    }
    if (!previewData) return null;
    return (
      <>
        <div className={styles.previewMeta}>
          {previewData.cwd ? (
            <span className={styles.metaItem}>
              <span className={styles.metaLabel}>{t('workbench:sessionSearch.metaCwd')}:</span>
              <span className={styles.metaValue}>{previewData.cwd}</span>
            </span>
          ) : null}
          {previewData.gitBranch ? (
            <span className={styles.metaItem}>
              <span className={styles.metaLabel}>{t('workbench:sessionSearch.metaGitBranch')}:</span>
              <span className={styles.metaValue}>{previewData.gitBranch}</span>
            </span>
          ) : null}
          <span className={styles.metaItem}>
            <span className={styles.metaLabel}>{t('workbench:sessionSearch.metaMessageCount')}:</span>
            <span className={styles.metaValue}>{previewData.messageCount}</span>
          </span>
          <span className={styles.metaItem}>
            <span className={styles.metaLabel}>{t('workbench:sessionSearch.metaLastActivity')}:</span>
            <span className={styles.metaValue}>{formatRelativeTime(previewData.lastActivityAt, locale)}</span>
          </span>
        </div>
        <div className={styles.bodyPreview}>
          <div className={styles.messageList}>
            {previewData.recentMessages.map((msg, idx) => (
              <div key={idx} className={styles.message} data-role={msg.role}>
                <div className={styles.messageHeader}>
                  <span className={styles.roleLabel} data-role={msg.role}>
                    {msg.role === 'user'
                      ? t('workbench:sessionSearch.roleUser')
                      : t('workbench:sessionSearch.roleAssistant')}
                  </span>
                  <span className={styles.messageTime}>{formatRelativeTime(msg.timestamp, locale)}</span>
                </div>
                <div className={styles.messageText}>{msg.text}</div>
              </div>
            ))}
            {previewData.recentMessages.length === 0 ? (
              <div className={styles.stateHint}>{t('workbench:sessionSearch.emptyHint')}</div>
            ) : null}
          </div>
        </div>
      </>
    );
  };

  const footerHrefs: { label: string; kbd: string }[] = previewHit
    ? [
        { label: t('workbench:sessionSearch.footerResume'), kbd: 'esc' },
      ]
    : [
        { label: t('workbench:sessionSearch.footerNavigate'), kbd: '↑↓' },
        { label: t('workbench:sessionSearch.footerOpen'), kbd: '⏎' },
        { label: t('workbench:sessionSearch.footerClose'), kbd: 'esc' },
      ];

  return (
    <>
      <div className={styles.scrim} onClick={handleScrimClick} aria-hidden="true" />
      <div
        className={styles.palette}
        role="dialog"
        aria-modal="true"
        aria-label={t('workbench:sessionSearch.panelAriaLabel')}
        onClick={stopPropagation}
        onKeyDown={previewHit ? handlePreviewKeyDown : undefined}
      >
        {previewHit ? (
          <div className={styles.previewHeader}>
            <Button
              className={styles.backButton}
              variant="ghost"
              size="sm"
              onClick={() => {
                setPreviewHit(null);
                setPreviewData(null);
                setPreviewError(null);
                setTimeout(() => inputRef.current?.focus(), 30);
              }}
            >
              {t('workbench:sessionSearch.backToList')}
            </Button>
            <div className={styles.previewTitleWrap}>
              <div className={styles.previewTitle}>{previewHit.title}</div>
              <div className={styles.previewSubtitle}>
                {previewHit.messageCount} · {previewHit.sessionId.slice(0, 8)}
              </div>
            </div>
          </div>
        ) : (
          <div className={styles.header}>
            <SearchIcon size={20} className={styles.searchIcon} />
            <input
              ref={inputRef}
              className={styles.input}
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={handleListKeyDown}
              placeholder={t('workbench:sessionSearch.placeholder')}
              spellCheck={false}
              autoComplete="off"
              disabled={isRemote && !projectId}
            />
            <span className={styles.scopeBadge}>{t('workbench:sessionSearch.scopeWorktree')}</span>
          </div>
        )}

        {previewHit ? renderPreviewBody() : renderBody()}

        <div className={styles.footer}>
          <div className={styles.footerHints}>
            {footerHrefs.map((hint) => (
              <span key={hint.label} className={styles.footerHint}>
                <span className={styles.kbd}>{hint.kbd}</span>
                {hint.label}
              </span>
            ))}
          </div>
          {previewHit ? (
            <div className={styles.footerActions}>
              <Button variant="ghost" size="sm" onClick={onClose} disabled={resuming}>
                {t('workbench:sessionSearch.cancelButton')}
              </Button>
              <Button variant="primary" size="sm" loading={resuming} onClick={handleResume}>
                {t('workbench:sessionSearch.resumeButton')}
              </Button>
            </div>
          ) : (
            <span className={styles.footerHint}>
              {isRemote ? t('workbench:sessionSearch.offline') : resultCountText}
            </span>
          )}
        </div>
      </div>
    </>
  );
}
