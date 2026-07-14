/**
 * 版本历史抽屉（Prompt / Scratchpad 共用）
 *
 * Business Logic（为什么需要这个组件）:
 *   同步冲突与历史版本需要可查看、可恢复为新版本、可复制内容的非阻塞入口；
 *   Prompt 与速记本共享同一交互，避免两套相近 UI。
 *
 * Code Logic（这个组件做什么）:
 *   渲染右侧 Drawer + 版本列表（时间/设备/kind/预览）+ 恢复确认 Dialog；
 *   文案走 prompts|scratchpad 命名空间的同名 key；无 API 调用。
 */

import { useCallback, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Drawer, Pill } from '@/components/primitives';
import { CopyIcon, SyncIcon, XIcon } from '@/lib/icons';
import type { ContentVersion } from '@/lib/types';
import styles from './VersionHistoryDrawer.module.css';

export type VersionHistoryNamespace = 'prompts' | 'scratchpad';

export interface VersionHistoryDrawerProps {
  open: boolean;
  onClose: () => void;
  versions: ContentVersion[];
  loading: boolean;
  error: string | null;
  restoringVersionId: string | null;
  i18nNamespace: VersionHistoryNamespace;
  onRestore: (version: ContentVersion) => void;
  onCopy: (version: ContentVersion) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   版本时间需本地可读，非法时间不能空白。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 时间格式化为本地短日期时间；非法时回退原串。
 */
function formatVersionTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * 渲染版本历史抽屉
 *
 * Business Logic（为什么需要这个组件）:
 *   用户打开历史后需要在侧栏浏览、复制与确认恢复，不得提供三方合并编辑器。
 *
 * Code Logic（这个组件做什么）:
 *   hooks 在 early return 前；open=false 时仍可挂载但 Drawer 不 portal。
 */
export function VersionHistoryDrawer(props: VersionHistoryDrawerProps): JSX.Element {
  const {
    open,
    onClose,
    versions,
    loading,
    error,
    restoringVersionId,
    i18nNamespace,
    onRestore,
    onCopy,
  } = props;
  const { t } = useTranslation([i18nNamespace, 'common']);
  const [pendingRestore, setPendingRestore] = useState<ContentVersion | null>(null);
  const busy = restoringVersionId !== null;

  /**
   * Business Logic（为什么需要这个函数）:
   *   恢复是破坏性意图，需二次确认。
   *
   * Code Logic（这个函数做什么）:
   *   记录待恢复版本并打开确认 Dialog。
   */
  const handleRequestRestore = useCallback((version: ContentVersion) => {
    setPendingRestore(version);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可取消恢复确认。
   *
   * Code Logic（这个函数做什么）:
   *   清空 pendingRestore。
   */
  const handleCancelRestore = useCallback(() => {
    if (busy) return;
    setPendingRestore(null);
  }, [busy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   确认后由父级调用 restore API。
   *
   * Code Logic（这个函数做什么）:
   *   调用 onRestore 并关闭确认层（成功/失败由父级状态驱动）。
   */
  const handleConfirmRestore = useCallback(() => {
    if (!pendingRestore || busy) return;
    const target = pendingRestore;
    setPendingRestore(null);
    onRestore(target);
  }, [busy, onRestore, pendingRestore]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭抽屉时应清除未完成的确认态。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时禁止关闭；否则清 pending 并 onClose。
   */
  const handleClose = useCallback(() => {
    if (busy) return;
    setPendingRestore(null);
    onClose();
  }, [busy, onClose]);

  const titleId = `${i18nNamespace}-version-history-title`;

  return (
    <>
      <Drawer
        open={open}
        titleId={titleId}
        onClose={handleClose}
        side="right"
        closeOnEscape={!busy}
        closeOnBackdrop={!busy}
        className={styles.drawer}
      >
        <div className={styles.header}>
          <h2 id={titleId} className={styles.title}>
            {t(`${i18nNamespace}:versionHistory`)}
          </h2>
          <Button
            variant="ghost"
            size="sm"
            icon={<XIcon />}
            onClick={handleClose}
            disabled={busy}
            aria-label={t('common:action.cancel')}
          />
        </div>

        <div className={styles.body} data-testid={`${i18nNamespace}-version-history`}>
          {loading ? (
            <p className={styles.meta} role="status">
              {t('common:loading')}
            </p>
          ) : null}

          {!loading && error ? (
            <p className={styles.error} role="alert" data-testid={`${i18nNamespace}-version-history-error`}>
              {error}
            </p>
          ) : null}

          {!loading && !error && versions.length === 0 ? (
            <p className={styles.meta} data-testid={`${i18nNamespace}-version-history-empty`}>
              {t(`${i18nNamespace}:versionHistoryEmpty`)}
            </p>
          ) : null}

          {!loading && versions.length > 0 ? (
            <ul className={styles.list}>
              {versions.map((version) => {
                const isConflict = version.kind === 'conflict';
                const preview =
                  version.contentPreview?.trim() ||
                  version.content?.trim() ||
                  t(`${i18nNamespace}:versionPreviewEmpty`);
                const restoring = restoringVersionId === version.id;
                return (
                  <li key={version.id} className={styles.item} data-testid={`${i18nNamespace}-version-item-${version.id}`}>
                    <div className={styles.itemHeader}>
                      <span className={styles.itemTime}>{formatVersionTime(version.createdAt)}</span>
                      <Pill tone={isConflict ? 'warn' : 'neutral'} dot data-testid={`${i18nNamespace}-version-kind-${version.id}`}>
                        {isConflict
                          ? t(`${i18nNamespace}:versionKindConflict`)
                          : t(`${i18nNamespace}:versionKindHistory`)}
                      </Pill>
                    </div>
                    <p className={styles.itemDevice}>
                      {t(`${i18nNamespace}:versionSourceDevice`, { device: version.sourceDevice })}
                    </p>
                    {version.title ? <p className={styles.itemTitle}>{version.title}</p> : null}
                    <p className={styles.itemPreview}>{preview}</p>
                    <div className={styles.itemActions}>
                      <Button
                        variant="secondary"
                        size="sm"
                        icon={<CopyIcon />}
                        onClick={() => onCopy(version)}
                        disabled={busy}
                      >
                        {t(`${i18nNamespace}:versionCopy`)}
                      </Button>
                      <Button
                        variant="primary"
                        size="sm"
                        icon={<SyncIcon />}
                        onClick={() => handleRequestRestore(version)}
                        disabled={busy}
                        loading={restoring}
                        aria-busy={restoring || undefined}
                      >
                        {t(`${i18nNamespace}:versionRestore`)}
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          ) : null}
        </div>
      </Drawer>

      <Dialog
        open={Boolean(pendingRestore)}
        titleId={`${i18nNamespace}-version-restore-title`}
        onClose={handleCancelRestore}
        closeOnEscape={!busy}
        closeOnBackdrop={!busy}
        className={styles.modal}
      >
        <Card variant="elevated" className={styles.modalCard}>
          <h3 id={`${i18nNamespace}-version-restore-title`} className={styles.modalTitle}>
            {t(`${i18nNamespace}:versionRestoreConfirmTitle`)}
          </h3>
          <p className={styles.modalText}>{t(`${i18nNamespace}:versionRestoreConfirmText`)}</p>
          <div className={styles.modalActions}>
            <Button variant="secondary" size="sm" onClick={handleCancelRestore} disabled={busy}>
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={handleConfirmRestore}
              disabled={busy}
              data-testid={`${i18nNamespace}-version-restore-confirm`}
            >
              {t(`${i18nNamespace}:versionRestore`)}
            </Button>
          </div>
        </Card>
      </Dialog>
    </>
  );
}
