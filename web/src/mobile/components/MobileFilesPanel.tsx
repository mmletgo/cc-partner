import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import { ChevronRightIcon, FileIcon, FolderIcon } from '@/lib/icons';
import {
  isMobileFileOpenResponseCurrent,
  isMobileFileSaveResponseCurrent,
  shouldBlockMobileFileContextSwitch,
  shouldInvalidateMobileFileOpenOnDirectoryLoad,
  shouldSkipMobileFileContextConfirmForDiscardToken,
  shouldSkipMobileFileContextReload,
  type MobileFileDirtySnapshot,
  type MobileFilePanelContext,
} from '../mobilePanelState';
import type {
  WorkbenchFileNode,
  WorkbenchOpenFile,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileFilesPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  discardContextToken?: number;
  onDirtyContextChange?: (snapshot: MobileFileDirtySnapshot) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件面板需要在用户进入子目录后提供返回上级能力，避免第一版只能浏览根目录。
 *
 * Code Logic（这个函数做什么）:
 *   接收 Workbench 相对路径，去掉末段后返回父目录；根路径或空路径返回空字符串。
 */
function getParentPath(path: string): string {
  const normalized = path.replace(/\/+$/u, '');
  if (!normalized) return '';
  const index = normalized.lastIndexOf('/');
  return index <= 0 ? '' : normalized.slice(0, index);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件元信息来自后端 ISO 字符串，移动端列表需要以用户本地格式展示修改时间。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 字符串转为本地时间文本；空值或无效日期返回 null，让调用方显示空值占位。
 */
function formatModifiedAt(value: string | null): string | null {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;
  return date.toLocaleString();
}

/**
 * MobileFilesPanel（移动端文件面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要能浏览当前 active worktree 的文件、快速查看非文本摘要，并编辑保存可编辑文本文件。
 *
 * Code Logic（这个组件做什么）:
 *   通过 HTTP transport 加载目录、打开文件和保存文本；所有异步请求使用 request id 防止旧项目或旧 worktree 响应覆盖当前 UI。
 */
export function MobileFilesPanel({
  project,
  worktree,
  discardContextToken = 0,
  onDirtyContextChange,
}: MobileFilesPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [currentDir, setCurrentDir] = useState<string>('');
  const [nodes, setNodes] = useState<WorkbenchFileNode[]>([]);
  const [opened, setOpened] = useState<WorkbenchOpenFile | null>(null);
  const [openedContext, setOpenedContext] = useState<MobileFilePanelContext | null>(null);
  const [loadedContext, setLoadedContext] = useState<MobileFilePanelContext | null>(null);
  const [draft, setDraft] = useState<string>('');
  const [dirty, setDirty] = useState<boolean>(false);
  const [loading, setLoading] = useState<boolean>(false);
  const [saving, setSaving] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [contextBlocked, setContextBlocked] = useState<boolean>(false);
  const listRequestIdRef = useRef<number>(0);
  const openRequestIdRef = useRef<number>(0);
  const saveRequestIdRef = useRef<number>(0);
  const dirtyRef = useRef<boolean>(false);
  const openedRef = useRef<WorkbenchOpenFile | null>(null);
  const openedContextRef = useRef<MobileFilePanelContext | null>(null);
  const loadedContextRef = useRef<MobileFilePanelContext | null>(null);
  const discardContextTokenRef = useRef<number>(discardContextToken);
  const contextKey = `${project?.id ?? ''}:${worktree?.id ?? ''}`;

  const canGoUp = currentDir.length > 0;
  const canEditOpenedFile = Boolean(opened?.text && opened.capabilities.canEdit);

  useEffect(() => {
    dirtyRef.current = dirty;
  }, [dirty]);

  useEffect(() => {
    openedRef.current = opened;
  }, [opened]);

  useEffect(() => {
    openedContextRef.current = openedContext;
  }, [openedContext]);

  useEffect(() => {
    onDirtyContextChange?.({
      dirty,
      context: dirty ? openedContext ?? loadedContext : null,
    });
  }, [dirty, loadedContext, onDirtyContextChange, openedContext]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件面板需要同时让内部 stale guard 和父级 dirty snapshot 知道当前已加载的 project/worktree。
   *
   * Code Logic（这个函数做什么）:
   *   将 loaded context 同步写入 ref 与 state；ref 供异步请求校验，state 供 React effect 通知父组件。
   */
  const updateLoadedContext = useCallback((nextContext: MobileFilePanelContext | null): void => {
    loadedContextRef.current = nextContext;
    setLoadedContext(nextContext);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   目录加载要绑定请求发起时的项目和 worktree，避免用户快速切换后旧响应写回文件列表或旧 open 响应覆盖根目录重载。
   *
   * Code Logic（这个函数做什么）:
   *   调用 files.listDir，使用 request id 校验响应顺序；根目录加载会递增 open request id，让未完成的文件打开请求失效。
   */
  const loadDirectory = useCallback(
    async (context: MobileFilePanelContext, path: string): Promise<void> => {
      const requestId = listRequestIdRef.current + 1;
      listRequestIdRef.current = requestId;
      if (shouldInvalidateMobileFileOpenOnDirectoryLoad(path)) {
        openRequestIdRef.current += 1;
      }
      setLoading(true);
      setError(null);

      try {
        const nextNodes = await httpWorkbenchTransport.files.listDir(
          context.projectId,
          path,
          context.worktreeId,
        );
        if (listRequestIdRef.current !== requestId) return;
        updateLoadedContext(context);
        setContextBlocked(false);
        setCurrentDir(path);
        setNodes(nextNodes);
      } catch (reason) {
        if (listRequestIdRef.current !== requestId) return;
        const message = reason instanceof Error ? reason.message : String(reason);
        setError(`${t('workbench:errors.files')}: ${message}`);
      } finally {
        if (listRequestIdRef.current === requestId) {
          setLoading(false);
        }
      }
    },
    [t, updateLoadedContext],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- project/worktree props 变化时需要同步文件面板上下文状态 */
  useEffect(() => {
    const parentDiscardedContext = shouldSkipMobileFileContextConfirmForDiscardToken(
      discardContextTokenRef.current,
      discardContextToken,
    );
    discardContextTokenRef.current = discardContextToken;
    const nextContext = project ? { projectId: project.id, worktreeId: worktree?.id ?? null } : null;

    if (!nextContext) {
      listRequestIdRef.current += 1;
      openRequestIdRef.current += 1;
      updateLoadedContext(null);
      setCurrentDir('');
      setNodes([]);
      setOpened(null);
      setOpenedContext(null);
      setDraft('');
      setDirty(false);
      setContextBlocked(false);
      return;
    }

    if (shouldSkipMobileFileContextReload(loadedContextRef.current, nextContext, openedContext)) {
      setContextBlocked(false);
      return;
    }

    if (
      !parentDiscardedContext &&
      shouldBlockMobileFileContextSwitch(loadedContextRef.current, nextContext, dirtyRef.current)
    ) {
      const shouldDiscard = window.confirm(
        t('workbench:mobile.filesPanel.discardContextConfirm'),
      );
      if (!shouldDiscard) {
        setContextBlocked(true);
        return;
      }
    }

    openRequestIdRef.current += 1;
    setOpened(null);
    setOpenedContext(null);
    setDraft('');
    setDirty(false);
    void loadDirectory(nextContext, '');
  }, [
    contextKey,
    discardContextToken,
    loadDirectory,
    openedContext,
    project,
    t,
    updateLoadedContext,
    worktree?.id,
  ]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户打开另一个文件前若当前文本有未保存改动，需要显式确认，避免误触丢失移动端输入。
   *
   * Code Logic（这个函数做什么）:
   *   检查 dirty 状态；无改动或用户确认丢弃时返回 true，否则返回 false 阻止后续导航。
   */
  const confirmDiscardDirtyFile = useCallback((): boolean => {
    if (!dirtyRef.current) return true;
    return window.confirm(t('workbench:mobile.filesPanel.discardFileConfirm'));
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件列表需要区分目录进入和文件打开，并复用同一 active worktree 根路径。
   *
   * Code Logic（这个函数做什么）:
   *   目录节点触发 listDir；文件节点触发 open，文本载荷进入 textarea 草稿，非文本进入只读预览。
   */
  const handleOpenNode = useCallback(
    async (node: WorkbenchFileNode): Promise<void> => {
      const context = loadedContextRef.current;
      if (!context || contextBlocked) return;
      if (node.kind === 'dir') {
        await loadDirectory(context, node.path);
        return;
      }
      if (!confirmDiscardDirtyFile()) return;

      const requestId = openRequestIdRef.current + 1;
      openRequestIdRef.current = requestId;
      setLoading(true);
      setError(null);
      try {
        const nextOpened = await httpWorkbenchTransport.files.open(
          context.projectId,
          node.path,
          context.worktreeId,
        );
        if (
          !isMobileFileOpenResponseCurrent(
            requestId,
            openRequestIdRef.current,
            context,
            loadedContextRef.current,
          )
        ) {
          return;
        }
        setOpened(nextOpened);
        setOpenedContext(context);
        setDraft(nextOpened.text?.content ?? '');
        setDirty(false);
      } catch (reason) {
        if (openRequestIdRef.current !== requestId) return;
        const message = reason instanceof Error ? reason.message : String(reason);
        setError(`${t('workbench:errors.openFile')}: ${message}`);
      } finally {
        if (openRequestIdRef.current === requestId) {
          setLoading(false);
        }
      }
    },
    [confirmDiscardDirtyFile, contextBlocked, loadDirectory, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端编辑文本文件后需要把内容保存回当前 worktree，并更新后续保存所需的乐观锁基线。
   *
   * Code Logic（这个函数做什么）:
   *   调用 files.saveText，成功后用返回的 baseHash/baseModifiedAt/metadata 更新 opened 状态并清除 dirty。
   */
  const handleSave = useCallback(async (): Promise<void> => {
    if (!opened || !opened.text || !openedContext || !canEditOpenedFile) return;
    const requestId = saveRequestIdRef.current + 1;
    saveRequestIdRef.current = requestId;
    const requestContext = openedContext;
    const requestPath = opened.metadata.path;
    const requestDraft = draft;
    const requestBaseHash = opened.text.baseHash;
    setSaving(true);
    setError(null);
    try {
      const result = await httpWorkbenchTransport.files.saveText(
        requestContext.projectId,
        requestPath,
        requestDraft,
        requestBaseHash,
        requestContext.worktreeId,
      );
      if (
        !isMobileFileSaveResponseCurrent(
          requestId,
          saveRequestIdRef.current,
          requestContext,
          openedContextRef.current,
          requestPath,
          openedRef.current?.metadata.path ?? null,
        )
      ) {
        return;
      }
      setOpened((current) =>
        current
          ? {
              ...current,
              metadata: result.metadata,
              text: current.text
                ? {
                    ...current.text,
                    content: requestDraft,
                    baseHash: result.baseHash,
                    baseModifiedAt: result.baseModifiedAt,
                  }
                : current.text,
            }
          : current,
      );
      setDirty(false);
    } catch (reason) {
      if (saveRequestIdRef.current !== requestId) return;
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(`${t('workbench:errors.saveFile')}: ${message}`);
    } finally {
      if (saveRequestIdRef.current === requestId) {
        setSaving(false);
      }
    }
  }, [canEditOpenedFile, draft, opened, openedContext, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端目录层级浏览需要给用户一个返回上级目录的显式入口。
   *
   * Code Logic（这个函数做什么）:
   *   根据 currentDir 计算父路径，并对当前 loaded context 重新加载目录。
   */
  const handleGoUp = useCallback(async (): Promise<void> => {
    const context = loadedContextRef.current;
    if (!context || !canGoUp) return;
    await loadDirectory(context, getParentPath(currentDir));
  }, [canGoUp, currentDir, loadDirectory]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户拒绝切换上下文时，文件面板需要提供显式放弃当前草稿并加载最新 active worktree 的入口。
   *
   * Code Logic（这个函数做什么）:
   *   重新确认后清空 dirty/opened 状态，并用最新 props 对应 context 加载根目录。
   */
  const handleDiscardAndLoadCurrent = useCallback(async (): Promise<void> => {
    if (!project) return;
    const shouldDiscard = window.confirm(t('workbench:mobile.filesPanel.discardFileConfirm'));
    if (!shouldDiscard) return;
    const nextContext = { projectId: project.id, worktreeId: worktree?.id ?? null };
    openRequestIdRef.current += 1;
    setOpened(null);
    setOpenedContext(null);
    setDraft('');
    setDirty(false);
    setContextBlocked(false);
    await loadDirectory(nextContext, '');
  }, [loadDirectory, project, t, worktree?.id]);

  const metadataText = opened
    ? [
        opened.detectedType,
        opened.metadata.size === null
          ? t('workbench:emptyValue')
          : opened.metadata.size < 1024
            ? t('workbench:mobile.filesPanel.size.bytes', { value: opened.metadata.size })
            : opened.metadata.size < 1024 * 1024
              ? t('workbench:mobile.filesPanel.size.kb', {
                  value: (opened.metadata.size / 1024).toFixed(1),
                })
              : t('workbench:mobile.filesPanel.size.mb', {
                  value: (opened.metadata.size / 1024 / 1024).toFixed(1),
                }),
        formatModifiedAt(opened.metadata.modifiedAt) ?? t('workbench:emptyValue'),
      ].join(' · ')
    : null;

  return (
    <section className={styles.panel} aria-labelledby="mobile-files-panel-title">
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <h1 id="mobile-files-panel-title">{t('workbench:mobile.filesPanel.title')}</h1>
        </div>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={!loadedContext || loading || contextBlocked}
          onClick={() => {
            const context = loadedContextRef.current;
            if (!context) return;
            void loadDirectory(context, currentDir);
          }}
        >
          {t('workbench:refresh')}
        </button>
      </div>

      {!project ? <p className={styles.panelState}>{t('workbench:mobile.filesPanel.noProject')}</p> : null}
      {contextBlocked ? (
        <div className={styles.panelState}>
          <p>{t('workbench:mobile.filesPanel.contextBlocked')}</p>
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={() => void handleDiscardAndLoadCurrent()}
          >
            {t('workbench:mobile.filesPanel.discardAndLoad')}
          </button>
        </div>
      ) : null}
      {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}

      <div className={styles.mobileToolbar}>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={!canGoUp || loading || contextBlocked}
          onClick={() => void handleGoUp()}
        >
          {t('workbench:mobile.filesPanel.up')}
        </button>
        <span className={styles.mobilePathCrumb}>{currentDir || t('workbench:rootPath')}</span>
      </div>

      <div className={styles.mobileSplitPanel}>
        <div className={styles.mobileList} aria-label={t('workbench:mobile.filesPanel.listAriaLabel')}>
          {nodes.length === 0 && project && !loading ? (
            <p className={styles.panelState}>{t('workbench:mobile.filesPanel.empty')}</p>
          ) : null}
          {nodes.map((node) => (
            <button
              key={node.path}
              type="button"
              className={styles.mobileListItem}
              disabled={loading || contextBlocked}
              onClick={() => void handleOpenNode(node)}
            >
              <span className={styles.mobileListTitleRow}>
                <span className={styles.mobileListTitleWithIcon}>
                  {node.kind === 'dir' ? (
                    <FolderIcon size={16} aria-hidden="true" />
                  ) : (
                    <FileIcon size={16} aria-hidden="true" />
                  )}
                  <strong className={styles.mobileListTitle}>{node.name}</strong>
                </span>
                {node.kind === 'dir' ? <ChevronRightIcon size={16} aria-hidden="true" /> : null}
              </span>
              <span className={styles.mobileListMeta}>
                {node.kind === 'dir'
                  ? t('workbench:pathKinds.dir')
                  : node.size === null
                    ? t('workbench:pathKinds.file')
                    : t('workbench:mobile.filesPanel.size.bytes', { value: node.size })}
              </span>
            </button>
          ))}
        </div>

        <div className={styles.mobilePreviewPane}>
          {!opened ? (
            <div className={styles.placeholder}>{t('workbench:mobile.filesPanel.noFile')}</div>
          ) : (
            <article className={styles.mobileFilePreview}>
              <header className={styles.mobilePreviewHeader}>
                <div className={styles.panelHeader}>
                  <h2>{opened.metadata.name}</h2>
                  {metadataText ? <p className={styles.mobileListMeta}>{metadataText}</p> : null}
                </div>
                {canEditOpenedFile ? (
                  <button
                    type="button"
                    className={styles.mobileTerminalPrimaryButton}
                    disabled={!dirty || saving}
                    onClick={() => void handleSave()}
                  >
                    {saving
                      ? t('workbench:mobile.filesPanel.saving')
                      : t('workbench:mobile.filesPanel.save')}
                  </button>
                ) : null}
              </header>

              {opened.notice ? <p className={styles.panelState}>{opened.notice}</p> : null}
              {opened.truncated ? (
                <p className={styles.panelState}>{t('workbench:mobile.filesPanel.truncated')}</p>
              ) : null}

              {canEditOpenedFile ? (
                <textarea
                  className={styles.mobileTextarea}
                  value={draft}
                  disabled={saving}
                  aria-label={t('workbench:mobile.filesPanel.editorAriaLabel')}
                  onChange={(event) => {
                    setDraft(event.target.value);
                    setDirty(true);
                  }}
                />
              ) : opened.image ? (
                <div className={styles.mobileReadonlyPreview}>
                  <img
                    className={styles.mobileImagePreview}
                    src={opened.image.dataUrl}
                    alt={opened.metadata.name}
                  />
                  <p>
                    {t('workbench:mobile.filesPanel.imageSummary', {
                      mime: opened.image.mime,
                      width: opened.image.width ?? t('workbench:emptyValue'),
                      height: opened.image.height ?? t('workbench:emptyValue'),
                    })}
                  </p>
                </div>
              ) : opened.csv ? (
                <div className={styles.mobileReadonlyPreview}>
                  <p>{t('workbench:mobile.filesPanel.csvSummary')}</p>
                  <div className={styles.mobileTableScroller}>
                    <table className={styles.mobilePreviewTable}>
                      <thead>
                        <tr>
                          {opened.csv.columns.map((column, columnIndex) => (
                            <th key={`${opened.metadata.path}-csv-column-${columnIndex}`}>
                              {column}
                            </th>
                          ))}
                        </tr>
                      </thead>
                      <tbody>
                        {opened.csv.rows.slice(0, 12).map((row, rowIndex) => (
                          <tr key={`${opened.metadata.path}-${rowIndex}`}>
                            {row.map((cell, cellIndex) => (
                              <td key={`${opened.metadata.path}-${rowIndex}-${cellIndex}`}>
                                {cell}
                              </td>
                            ))}
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              ) : opened.sqlite ? (
                <div className={styles.mobileReadonlyPreview}>
                  <p>
                    {t('workbench:mobile.filesPanel.sqliteSummary', {
                      table: opened.sqlite.selectedTable ?? t('workbench:emptyValue'),
                      count: opened.sqlite.tables.length,
                    })}
                  </p>
                  <div className={styles.mobileTableScroller}>
                    <table className={styles.mobilePreviewTable}>
                      <thead>
                        <tr>
                          {opened.sqlite.columns.map((column, columnIndex) => (
                            <th key={`${opened.metadata.path}-sqlite-column-${columnIndex}`}>
                              {column}
                            </th>
                          ))}
                        </tr>
                      </thead>
                      <tbody>
                        {opened.sqlite.rows.slice(0, 12).map((row, rowIndex) => (
                          <tr key={`${opened.metadata.path}-sqlite-${rowIndex}`}>
                            {row.map((cell, cellIndex) => (
                              <td key={`${opened.metadata.path}-sqlite-${rowIndex}-${cellIndex}`}>
                                {cell}
                              </td>
                            ))}
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              ) : (
                <div className={styles.mobileReadonlyPreview}>
                  {t('workbench:mobile.filesPanel.readonlyUnsupported', {
                    type: opened.detectedType,
                  })}
                </div>
              )}
            </article>
          )}
        </div>
      </div>
    </section>
  );
}
