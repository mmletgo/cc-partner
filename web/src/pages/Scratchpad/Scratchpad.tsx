/**
 * 速记本页面
 *
 * Business Logic（为什么需要这个页面）:
 *   用户在日常工作中需要快速记录多组临时想法、片段文字、待办事项等。
 *   多页面速记本让不同主题可以分开保存，并继续支持自动保存与局域网同步。
 *
 * Code Logic（这个页面做什么）:
 *   - 从 Rust/SQLite 加载页面摘要列表和当前页面正文
 *   - 左侧展示页面列表，右侧编辑标题与正文
 *   - 正文 500ms debounce 自动保存，并在切页/删除/同步前 flush 当前页待保存内容
 *   - 支持创建、重命名、删除、复制、清空和同步操作
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { scratchpadApi } from '@/api/scratchpad';
import { Button, Card, Input } from '@/components/primitives';
import { CopyIcon, PlusIcon, SyncIcon, TrashIcon, XIcon } from '@/lib/icons';
import type { ScratchpadPage, ScratchpadPageSummary } from '@/lib/types';
import styles from './Scratchpad.module.css';

const AUTOSAVE_DELAY_MS = 500;

interface PendingSave {
  pageId: string;
  content: string;
}

/**
 * Business Logic（为什么需要）:
 *   用户需要在页面列表中快速辨认空页面，避免只有标题时无法判断内容状态。
 *
 * Code Logic（做什么）:
 *   接收页面正文，压缩空白并截断为短预览；空内容返回 fallback 文案。
 */
function buildPreview(content: string, fallback: string): string {
  const compact = content.replace(/\s+/g, ' ').trim();
  if (!compact) return fallback;
  return compact.length > 72 ? `${compact.slice(0, 72)}...` : compact;
}

/**
 * Business Logic（为什么需要）:
 *   错误提示需要统一转换为用户可读文案。
 *
 * Code Logic（做什么）:
 *   将 unknown 错误规整为 Error.message；非 Error 值使用 fallback。
 */
function getErrorMessage(err: unknown, fallback: string): string {
  return err instanceof Error ? err.message : fallback;
}

/**
 * Business Logic（为什么需要）:
 *   用户需要看到页面保存/更新时间，辅助判断同步和编辑状态。
 *
 * Code Logic（做什么）:
 *   将 ISO 时间格式化为本地短日期时间；非法时间回退为原字符串。
 */
function formatLocalDateTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Business Logic（为什么需要）:
 *   用户需要一个可随手记录、自动保留内容的多页面速记空间。
 *
 * Code Logic（做什么）:
 *   管理页面列表、当前页、标题草稿、正文草稿和保存/同步状态；
 *   所有持久化与同步都通过 Tauri invoke 交给 Rust 后端。
 */
export function Scratchpad() {
  const { t } = useTranslation(['scratchpad', 'common']);
  const [pages, setPages] = useState<ScratchpadPageSummary[]>([]);
  const [currentPage, setCurrentPage] = useState<ScratchpadPage | null>(null);
  const [titleDraft, setTitleDraft] = useState('');
  const [text, setText] = useState('');
  const [pendingClear, setPendingClear] = useState(false);
  const [pendingDelete, setPendingDelete] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const saveTimerRef = useRef<number | null>(null);
  const pendingSaveRef = useRef<PendingSave | null>(null);
  const textRef = useRef('');
  const currentPageIdRef = useRef<string | null>(null);

  const charCount = text.length;
  const currentPageId = currentPage?.id ?? null;

  /**
   * Business Logic（为什么需要）:
   *   切页、删除、同步前必须保存用户尚未落库的正文，避免内容丢失。
   *
   * Code Logic（做什么）:
   *   清除 debounce timer，使用 pendingSaveRef 内捕获的旧 pageId 和 content 调后端保存。
   */
  const flushPendingSave = useCallback(async () => {
    const pending = pendingSaveRef.current;
    if (!pending) return;

    if (saveTimerRef.current !== null) {
      window.clearTimeout(saveTimerRef.current);
      saveTimerRef.current = null;
    }

    pendingSaveRef.current = null;
    setSaving(true);
    try {
      const saved = await scratchpadApi.updatePageContent(pending.pageId, pending.content);
      setStatus(t('scratchpad:savedAt', { time: new Date(saved.updatedAt).toLocaleTimeString() }));
      const latestPages = await scratchpadApi.listPages();
      setPages(latestPages);
      if (currentPageIdRef.current === saved.id) {
        setCurrentPage(saved);
        setTitleDraft(saved.title);
      }
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:saveFailed')));
      throw err;
    } finally {
      setSaving(false);
    }
  }, [t]);

  /**
   * Business Logic（为什么需要）:
   *   正文编辑应自动保存，但不能每次按键都立即写库。
   *
   * Code Logic（做什么）:
   *   用 pageId 和 content 写入 pendingSaveRef，并启动 500ms debounce；
   *   timer 触发时调用 flushPendingSave，确保保存目标页不受后续切页影响。
   */
  const scheduleContentSave = useCallback(
    (pageId: string, content: string) => {
      pendingSaveRef.current = { pageId, content };
      setSaving(true);
      setStatus(null);
      if (saveTimerRef.current !== null) {
        window.clearTimeout(saveTimerRef.current);
      }
      saveTimerRef.current = window.setTimeout(() => {
        saveTimerRef.current = null;
        void flushPendingSave().catch(() => undefined);
      }, AUTOSAVE_DELAY_MS);
    },
    [flushPendingSave],
  );

  /**
   * Business Logic（为什么需要）:
   *   页面切换或刷新列表后，编辑区必须展示指定页面的最新正文。
   *
   * Code Logic（做什么）:
   *   读取页面详情并同步 currentPage/titleDraft/text/ref 状态。
   */
  const openPage = useCallback(
    async (pageId: string) => {
      const page = await scratchpadApi.getPage(pageId);
      currentPageIdRef.current = page.id;
      textRef.current = page.content;
      setCurrentPage(page);
      setTitleDraft(page.title);
      setText(page.content);
      setPendingClear(false);
      setPendingDelete(false);
      return page;
    },
    [],
  );

  /**
   * Business Logic（为什么需要）:
   *   用户首次进入或删除最后一页后仍需要可编辑页面。
   *
   * Code Logic（做什么）:
   *   创建默认标题页面，刷新列表，并打开新页面。
   */
  const createAndOpenDefaultPage = useCallback(async () => {
    const page = await scratchpadApi.createPage(t('scratchpad:newPage'));
    const latestPages = await scratchpadApi.listPages();
    setPages(latestPages);
    currentPageIdRef.current = page.id;
    textRef.current = page.content;
    setCurrentPage(page);
    setTitleDraft(page.title);
    setText(page.content);
    setPendingClear(false);
    setPendingDelete(false);
    return page;
  }, [t]);

  /**
   * Business Logic（为什么需要）:
   *   用户进入速记本时应看到最近更新的页面；同步后也需要保持列表最新。
   *
   * Code Logic（做什么）:
   *   拉取页面列表，优先打开 preferPageId；不存在则打开最新页；列表为空时创建默认页。
   */
  const reloadPages = useCallback(
    async (preferPageId?: string | null) => {
      const latestPages = await scratchpadApi.listPages();
      if (latestPages.length === 0) {
        setPages([]);
        return createAndOpenDefaultPage();
      }

      setPages(latestPages);
      const targetPage = latestPages.find((page) => page.id === preferPageId) ?? latestPages[0];
      return openPage(targetPage.id);
    },
    [createAndOpenDefaultPage, openPage],
  );

  useEffect(() => {
    let cancelled = false;

    /**
     * Business Logic（为什么需要）:
     *   页面挂载后需要初始化最近编辑的速记本页面。
     *
     * Code Logic（做什么）:
     *   异步加载列表和当前页；组件卸载后不再写 loading/error 状态。
     */
    const loadInitialPage = async () => {
      setLoading(true);
      setError(null);
      try {
        await reloadPages(null);
      } catch (err) {
        if (!cancelled) {
          setError(getErrorMessage(err, t('scratchpad:loadFailed')));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    void loadInitialPage();

    return () => {
      cancelled = true;
      if (saveTimerRef.current !== null) {
        window.clearTimeout(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
  }, [reloadPages, t]);

  /**
   * Business Logic（为什么需要）:
   *   用户输入正文时应立即看到文本变化，并由系统自动保存。
   *
   * Code Logic（做什么）:
   *   更新本地正文状态，记录 ref，并按当前 pageId 调度 debounce 保存。
   */
  const handleContentChange = useCallback(
    (value: string) => {
      setText(value);
      textRef.current = value;
      if (currentPageId) {
        scheduleContentSave(currentPageId, value);
      }
    },
    [currentPageId, scheduleContentSave],
  );

  /**
   * Business Logic（为什么需要）:
   *   用户需要在多页速记本之间切换上下文。
   *
   * Code Logic（做什么）:
   *   切页前 flush 当前页待保存内容，再读取目标页面详情。
   */
  const handleSelectPage = useCallback(
    async (pageId: string) => {
      if (pageId === currentPageId || loading) return;
      setError(null);
      try {
        await flushPendingSave();
        await openPage(pageId);
      } catch (err) {
        setError(getErrorMessage(err, t('scratchpad:loadFailed')));
      }
    },
    [currentPageId, flushPendingSave, loading, openPage, t],
  );

  /**
   * Business Logic（为什么需要）:
   *   用户需要快速新增一个独立记录页面。
   *
   * Code Logic（做什么）:
   *   新增前 flush 当前页，创建默认标题页面，刷新列表后选中新页面。
   */
  const handleCreatePage = useCallback(async () => {
    setError(null);
    try {
      await flushPendingSave();
      await createAndOpenDefaultPage();
      setStatus(t('scratchpad:pageCreated'));
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:createFailed')));
    }
  }, [createAndOpenDefaultPage, flushPendingSave, t]);

  /**
   * Business Logic（为什么需要）:
   *   用户需要通过标题区整理页面主题，空标题也应有可识别兜底名。
   *
   * Code Logic（做什么）:
   *   trim 标题，空值替换为“未命名”，必要时调用 rename_scratchpad_page。
   */
  const commitTitle = useCallback(async () => {
    if (!currentPage) return;
    const nextTitle = titleDraft.trim() || t('scratchpad:untitledPage');
    if (nextTitle === currentPage.title) {
      setTitleDraft(nextTitle);
      return;
    }

    setError(null);
    setSaving(true);
    try {
      const renamed = await scratchpadApi.renamePage(currentPage.id, nextTitle);
      const latestPages = await scratchpadApi.listPages();
      setPages(latestPages);
      if (currentPageIdRef.current === renamed.id) {
        setCurrentPage(renamed);
        setTitleDraft(renamed.title);
      }
      setStatus(t('scratchpad:renamedAt', { time: new Date(renamed.updatedAt).toLocaleTimeString() }));
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:renameFailed')));
    } finally {
      setSaving(false);
    }
  }, [currentPage, t, titleDraft]);

  /**
   * Business Logic（为什么需要）:
   *   标题输入按 Enter 应与失焦一致完成保存，符合文本字段习惯。
   *
   * Code Logic（做什么）:
   *   拦截 Enter 默认行为并提交标题。
   */
  const handleTitleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      event.currentTarget.blur();
    },
    [],
  );

  /**
   * Business Logic（为什么需要）:
   *   用户需要复制当前页面正文以粘贴到其他工具。
   *
   * Code Logic（做什么）:
   *   调用 Clipboard API 写入当前正文；失败时显示通用错误。
   */
  const handleCopyAll = useCallback(async () => {
    if (!textRef.current) return;
    try {
      await navigator.clipboard.writeText(textRef.current);
      setStatus(t('scratchpad:copied'));
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:copyFailed')));
    }
  }, [t]);

  /**
   * Business Logic（为什么需要）:
   *   清空正文是破坏性操作，需要二次确认。
   *
   * Code Logic（做什么）:
   *   仅在当前页有正文时打开确认弹层。
   */
  const handleClearRequest = useCallback(() => {
    if (!textRef.current) return;
    setPendingClear(true);
  }, []);

  /**
   * Business Logic（为什么需要）:
   *   用户确认清空后应立即看到空白页面，并由自动保存持久化。
   *
   * Code Logic（做什么）:
   *   将正文置空，按当前 pageId 调度保存。
   */
  const confirmClear = useCallback(() => {
    if (!currentPageId) return;
    setText('');
    textRef.current = '';
    setPendingClear(false);
    scheduleContentSave(currentPageId, '');
  }, [currentPageId, scheduleContentSave]);

  /**
   * Business Logic（为什么需要）:
   *   用户可能误触清空操作，需要可取消。
   *
   * Code Logic（做什么）:
   *   关闭清空确认弹层，不修改正文。
   */
  const cancelClear = useCallback(() => {
    setPendingClear(false);
  }, []);

  /**
   * Business Logic（为什么需要）:
   *   删除页面会移除一组记录，需要二次确认。
   *
   * Code Logic（做什么）:
   *   当前页存在时打开删除确认弹层。
   */
  const handleDeleteRequest = useCallback(() => {
    if (!currentPageId) return;
    setPendingDelete(true);
  }, [currentPageId]);

  /**
   * Business Logic（为什么需要）:
   *   删除当前页后用户应继续停留在可编辑页面上。
   *
   * Code Logic（做什么）:
   *   删除前 flush 当前页；删除后打开后端列表最新剩余页，没有剩余页则创建新页面。
   */
  const confirmDelete = useCallback(async () => {
    if (!currentPageId) return;
    setError(null);
    setSaving(true);
    try {
      await flushPendingSave();
      const deleted = await scratchpadApi.deletePage(currentPageId);
      if (!deleted.ok) {
        throw new Error(t('scratchpad:deleteFailed'));
      }
      await reloadPages(null);
      setStatus(t('scratchpad:pageDeleted'));
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:deleteFailed')));
    } finally {
      setSaving(false);
      setPendingDelete(false);
    }
  }, [currentPageId, flushPendingSave, reloadPages, t]);

  /**
   * Business Logic（为什么需要）:
   *   用户可能误触删除操作，需要可取消。
   *
   * Code Logic（做什么）:
   *   关闭删除确认弹层，不调用后端。
   */
  const cancelDelete = useCallback(() => {
    setPendingDelete(false);
  }, []);

  /**
   * Business Logic（为什么需要）:
   *   用户需要把多页面速记本同步到局域网其他设备和 GitHub 云端。
   *
   * Code Logic（做什么）:
   *   同步前 flush 当前页待保存内容，并发调用 LAN 与 GitHub 云同步；
   *   汇总两路结果，最后刷新列表和当前页，避免其中一路失败阻断另一路。
   */
  const handleSync = useCallback(async () => {
    setSyncing(true);
    setError(null);
    setStatus(null);
    try {
      await flushPendingSave();
      const [lanResult, cloudResult] = await Promise.allSettled([
        scratchpadApi.sync(),
        configApi.triggerCloudSync(),
      ]);
      await reloadPages(currentPageIdRef.current);

      const statusParts: string[] = [];
      const failureParts: string[] = [];

      if (lanResult.status === 'fulfilled') {
        statusParts.push(t('scratchpad:lanSyncDone', { count: lanResult.value.synced }));
      } else {
        const reason = getErrorMessage(lanResult.reason, t('scratchpad:lanSyncFailed'));
        statusParts.push(t('scratchpad:lanSyncFailed'));
        failureParts.push(t('scratchpad:lanSyncFailedWithReason', { reason }));
      }

      if (cloudResult.status === 'fulfilled' && cloudResult.value.ok) {
        statusParts.push(t('scratchpad:cloudSyncDone', { note: cloudResult.value.note }));
      } else if (cloudResult.status === 'fulfilled') {
        statusParts.push(t('scratchpad:cloudSyncFailed'));
        failureParts.push(t('scratchpad:cloudSyncFailedWithReason', { reason: cloudResult.value.note }));
      } else {
        const reason = getErrorMessage(cloudResult.reason, t('scratchpad:cloudSyncFailed'));
        statusParts.push(t('scratchpad:cloudSyncFailed'));
        failureParts.push(t('scratchpad:cloudSyncFailedWithReason', { reason }));
      }

      setStatus(statusParts.join(t('scratchpad:syncSummarySeparator')));
      if (failureParts.length > 0) {
        setError(t('scratchpad:syncFailed', { detail: failureParts.join(t('scratchpad:syncFailureSeparator')) }));
      }
    } catch (err) {
      setError(getErrorMessage(err, t('scratchpad:syncFailed', { detail: t('scratchpad:saveFailed') })));
    } finally {
      setSyncing(false);
    }
  }, [flushPendingSave, reloadPages, t]);

  return (
    <div className={styles.page}>
      <header className={styles.pageHeader}>
        <div>
          <span className={styles.eyebrow}>{t('scratchpad:eyebrow')}</span>
          <h1 className={styles.title}>{t('scratchpad:title')}</h1>
          <p className={styles.lead}>{t('scratchpad:desc')}</p>
        </div>
      </header>

      <div className={styles.statusRow} aria-live="polite">
        {loading ? <span>{t('scratchpad:loading')}</span> : null}
        {!loading && saving ? <span>{t('scratchpad:saving')}</span> : null}
        {!loading && !saving && status ? <span>{status}</span> : null}
        {error ? <span className={styles.error}>{error}</span> : null}
      </div>

      <div className={styles.workspace}>
        <aside className={styles.sidebar} aria-label={t('scratchpad:pageListAriaLabel')}>
          <div className={styles.sidebarHeader}>
            <span className={styles.sidebarTitle}>{t('scratchpad:pagesTitle')}</span>
            <span className={styles.pageCount}>{t('scratchpad:pageCount', { count: pages.length })}</span>
          </div>
          <div className={styles.pageList}>
            {pages.map((page) => {
              const isActive = page.id === currentPageId;
              const preview =
                page.id === currentPageId
                  ? buildPreview(text, t('scratchpad:emptyPreview'))
                  : t('scratchpad:pageUpdatedAt', {
                      time: formatLocalDateTime(page.updatedAt),
                    });
              return (
                <button
                  key={page.id}
                  type="button"
                  className={`${styles.pageItem} ${isActive ? styles.pageItemActive : ''}`}
                  onClick={() => void handleSelectPage(page.id)}
                  aria-current={isActive ? 'page' : undefined}
                  disabled={loading}
                >
                  <span className={styles.pageItemTitle}>{page.title || t('scratchpad:untitledPage')}</span>
                  <span className={styles.pageItemMeta}>{preview}</span>
                </button>
              );
            })}
            <Button
              variant="ghost"
              size="sm"
              icon={<PlusIcon />}
              onClick={handleCreatePage}
              disabled={loading}
              className={styles.createPageButton}
            >
              {t('scratchpad:newPage')}
            </Button>
          </div>
        </aside>

        <section className={styles.editorPane} aria-label={t('scratchpad:editorAriaLabel')}>
          <Card variant="outlined" padding="none" className={styles.editorCard}>
            <Card.Header className={styles.editorHeader}>
              <Input
                value={titleDraft}
                onChange={(event) => setTitleDraft(event.target.value)}
                onBlur={() => void commitTitle()}
                onKeyDown={handleTitleKeyDown}
                placeholder={t('scratchpad:titlePlaceholder')}
                aria-label={t('scratchpad:titleAriaLabel')}
                disabled={loading || !currentPage}
                className={styles.titleInput}
              />
              <div className={styles.actions}>
                <Button variant="secondary" size="sm" icon={<CopyIcon />} onClick={handleCopyAll} disabled={!text}>
                  {t('scratchpad:copyAll')}
                </Button>
                <Button variant="secondary" size="sm" icon={<TrashIcon />} onClick={handleClearRequest} disabled={!text}>
                  {t('scratchpad:clear')}
                </Button>
                <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={handleDeleteRequest} disabled={!currentPage}>
                  {t('scratchpad:deletePage')}
                </Button>
                <Button variant="secondary" size="sm" icon={<SyncIcon />} loading={syncing} onClick={handleSync} disabled={!currentPage}>
                  {syncing ? t('scratchpad:syncing') : t('scratchpad:syncAll')}
                </Button>
              </div>
            </Card.Header>
            <Card.Body className={styles.editorBody}>
              <textarea
                className={styles.editor}
                value={text}
                onChange={(event) => handleContentChange(event.target.value)}
                placeholder={t('scratchpad:placeholder')}
                aria-label={t('scratchpad:contentAriaLabel')}
                disabled={loading || !currentPage}
              />
            </Card.Body>
            <Card.Footer className={styles.editorFooter}>
              <span>{currentPage ? t('scratchpad:currentUpdatedAt', { time: formatLocalDateTime(currentPage.updatedAt) }) : null}</span>
              <span className={styles.charCount}>{t('scratchpad:charCount', { count: charCount })}</span>
            </Card.Footer>
          </Card>
        </section>
      </div>

      {pendingClear ? (
        <div className={styles.modalMask} role="dialog" aria-modal="true" aria-labelledby="clear-title">
          <Card variant="elevated" className={styles.modal}>
            <h3 id="clear-title" className={styles.modalTitle}>
              {t('scratchpad:clearConfirmTitle')}
            </h3>
            <p className={styles.modalText}>{t('scratchpad:clearConfirmText')}</p>
            <div className={styles.modalActions}>
              <Button variant="secondary" size="sm" icon={<XIcon />} onClick={cancelClear}>
                {t('common:action.cancel')}
              </Button>
              <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={confirmClear}>
                {t('scratchpad:clear')}
              </Button>
            </div>
          </Card>
        </div>
      ) : null}

      {pendingDelete ? (
        <div className={styles.modalMask} role="dialog" aria-modal="true" aria-labelledby="delete-title">
          <Card variant="elevated" className={styles.modal}>
            <h3 id="delete-title" className={styles.modalTitle}>
              {t('scratchpad:deleteConfirmTitle')}
            </h3>
            <p className={styles.modalText}>
              {t('scratchpad:deleteConfirmText', {
                title: currentPage?.title ?? t('scratchpad:untitledPage'),
              })}
            </p>
            <div className={styles.modalActions}>
              <Button variant="secondary" size="sm" icon={<XIcon />} onClick={cancelDelete}>
                {t('common:action.cancel')}
              </Button>
              <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={() => void confirmDelete()}>
                {t('scratchpad:deletePage')}
              </Button>
            </div>
          </Card>
        </div>
      ) : null}
    </div>
  );
}
