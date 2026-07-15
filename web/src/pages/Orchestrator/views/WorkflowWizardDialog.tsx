/**
 * WORKFLOW.md 就绪向导视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要检测 WORKFLOW 文档状态、从模板创建、查看诊断并安全保存，
 *   且不能通过向导启用/改变 delivery。
 *
 * Code Logic（这个组件做什么）:
 *   渲染共享 Dialog：missing 展示模板预览/创建、valid 展示摘要与打开文件、
 *   invalid 聚焦诊断行、hash conflict 保留草稿并提供重新加载；无 API import。
 */
import type { JSX, RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Pill } from '@/components/primitives';
import { SyncIcon, XIcon } from '@/lib/icons';
import type {
  WorkflowDiagnostic,
  WorkflowDocumentLoadState,
  WorkflowDocumentStatus,
} from '@/lib/types';
import styles from './WorkflowWizardDialog.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   向导状态与保存逻辑归 controller，视图只绑定受控字段与回调。
 *
 * Code Logic（这个类型做什么）:
 *   描述 open/busy/load/document/draft/diagnostics 与 create/validate/save/reload/open 回调。
 */
export interface WorkflowWizardDialogProps {
  open: boolean;
  loadState: WorkflowDocumentLoadState;
  documentStatus: WorkflowDocumentStatus | null;
  draft: string;
  expectedHash: string;
  diagnostics: WorkflowDiagnostic[];
  preview: string | null;
  loadError: string | null;
  saveError: string | null;
  conflict: boolean;
  busy: boolean;
  focusedDiagnosticLine: number | null;
  draftTextareaRef: RefObject<HTMLTextAreaElement | null>;
  onClose: () => void;
  onDraftChange: (value: string) => void;
  onCreateFromTemplate: () => void;
  onValidate: () => void;
  onSave: () => void;
  onReload: () => void;
  onOpenFile: () => void;
  onFocusDiagnostic: (diagnostic: WorkflowDiagnostic) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   向导入口必须是独立弹窗，不能占用看板主文档流。
 *
 * Code Logic（这个函数做什么）:
 *   用共享 Dialog（busy 时 closeOnEscape/closeOnBackdrop=false）渲染状态、草稿、诊断与动作。
 */
export function WorkflowWizardDialog(props: WorkflowWizardDialogProps): JSX.Element {
  const {
    open,
    loadState,
    documentStatus,
    draft,
    expectedHash,
    diagnostics,
    preview,
    loadError,
    saveError,
    conflict,
    busy,
    focusedDiagnosticLine,
    draftTextareaRef,
    onClose,
    onDraftChange,
    onCreateFromTemplate,
    onValidate,
    onSave,
    onReload,
    onOpenFile,
    onFocusDiagnostic,
  } = props;
  const { t } = useTranslation(['orchestrator', 'common']);
  const statusLabel =
    documentStatus === null
      ? t('orchestrator:workflowWizard.status.unknown')
      : t(`orchestrator:workflowWizard.status.${documentStatus}`);
  const showTemplate = documentStatus === 'missing' || (loadState === 'ready' && !draft && Boolean(preview));
  const canEdit = loadState === 'ready' || conflict;
  const canSave = canEdit && !busy && draft.trim().length > 0;

  return (
    <Dialog
      open={open}
      titleId="orchestrator-workflow-wizard-title"
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={draftTextareaRef}
      className={styles.dialog}
    >
      <Card variant="elevated" padding="md">
        <Card.Header className={styles.header}>
          <div>
            <h2 id="orchestrator-workflow-wizard-title" className={styles.sectionTitle}>
              {t('orchestrator:workflowWizard.title')}
            </h2>
            <p className={styles.sectionLead}>{t('orchestrator:workflowWizard.subtitle')}</p>
          </div>
          <Button
            variant="icon"
            aria-label={t('orchestrator:workflowWizard.close')}
            icon={<XIcon />}
            disabled={busy}
            onClick={onClose}
          />
        </Card.Header>
        <Card.Body className={styles.body}>
          <div className={styles.statusRow}>
            <Pill
              tone={
                documentStatus === 'valid'
                  ? 'success'
                  : documentStatus === 'invalid' || documentStatus === 'readError'
                    ? 'danger'
                    : 'warn'
              }
              dot
              data-testid="workflow-wizard-status"
            >
              {statusLabel}
            </Pill>
            {expectedHash ? (
              <span className={styles.notice} data-testid="workflow-wizard-hash">
                {t('orchestrator:workflowWizard.hash', { hash: expectedHash.slice(0, 12) })}
              </span>
            ) : null}
          </div>

          {loadState === 'loading' ? (
            <p className={styles.notice} role="status">
              {t('orchestrator:workflowWizard.loading')}
            </p>
          ) : null}

          {loadError ? (
            <p className={styles.alert} role="alert" data-testid="workflow-wizard-load-error">
              {loadError}
            </p>
          ) : null}

          {conflict ? (
            <p className={styles.alert} role="alert" data-testid="workflow-wizard-conflict">
              {t('orchestrator:workflowWizard.conflict')}
            </p>
          ) : null}

          {saveError && !conflict ? (
            <p className={styles.alert} role="alert" data-testid="workflow-wizard-save-error">
              {saveError}
            </p>
          ) : null}

          {showTemplate && preview ? (
            <label className={styles.field} data-testid="workflow-wizard-template-preview">
              <span>{t('orchestrator:workflowWizard.templatePreview')}</span>
              <pre className={styles.previewBox}>{preview}</pre>
            </label>
          ) : null}

          {documentStatus === 'valid' && !conflict ? (
            <ul className={styles.summaryList} data-testid="workflow-wizard-summary">
              <li>{t('orchestrator:workflowWizard.summaryValid')}</li>
              <li>{t('orchestrator:workflowWizard.summaryNoDelivery')}</li>
            </ul>
          ) : null}

          {diagnostics.length > 0 ? (
            <div data-testid="workflow-wizard-diagnostics">
              <span className={styles.notice}>{t('orchestrator:workflowWizard.diagnostics')}</span>
              <ul className={styles.diagnostics}>
                {diagnostics.map((item, index) => {
                  const focused = item.line != null && item.line === focusedDiagnosticLine;
                  return (
                    <li key={`${item.code}-${item.line ?? 'x'}-${index}`}>
                      <button
                        type="button"
                        className={styles.diagnosticItem}
                        data-testid={`workflow-wizard-diagnostic-${index}`}
                        data-focused={focused ? 'true' : 'false'}
                        onClick={() => onFocusDiagnostic(item)}
                      >
                        <span>{item.message}</span>
                        <span className={styles.diagnosticMeta}>
                          {t('orchestrator:workflowWizard.diagnosticMeta', {
                            path: item.path,
                            line: item.line ?? '-',
                            column: item.column ?? '-',
                            code: item.code,
                          })}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          ) : null}

          <label className={styles.field}>
            <span>{t('orchestrator:workflowWizard.draft')}</span>
            <textarea
              ref={draftTextareaRef}
              className={styles.textarea}
              value={draft}
              onChange={(event) => onDraftChange(event.target.value)}
              placeholder={t('orchestrator:workflowWizard.draftPlaceholder')}
              aria-label={t('orchestrator:workflowWizard.draft')}
              data-testid="workflow-wizard-draft"
              data-focused-line={focusedDiagnosticLine ?? undefined}
              rows={12}
              disabled={!canEdit || busy}
            />
          </label>

          <div className={styles.actions}>
            {documentStatus === 'missing' ? (
              <Button
                variant="secondary"
                size="sm"
                disabled={busy}
                onClick={onCreateFromTemplate}
                data-testid="workflow-wizard-create-template"
              >
                {t('orchestrator:workflowWizard.createFromTemplate')}
              </Button>
            ) : null}
            {documentStatus === 'valid' || documentStatus === 'invalid' ? (
              <Button
                variant="secondary"
                size="sm"
                disabled={busy}
                onClick={onOpenFile}
                data-testid="workflow-wizard-open-file"
              >
                {t('orchestrator:workflowWizard.openFile')}
              </Button>
            ) : null}
            {conflict ? (
              <Button
                variant="secondary"
                size="sm"
                icon={<SyncIcon />}
                disabled={busy}
                onClick={onReload}
                data-testid="workflow-wizard-reload"
              >
                {t('orchestrator:workflowWizard.reload')}
              </Button>
            ) : null}
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || !draft.trim()}
              onClick={onValidate}
              data-testid="workflow-wizard-validate"
            >
              {t('orchestrator:workflowWizard.validate')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              loading={busy}
              disabled={!canSave}
              onClick={onSave}
              data-testid="workflow-wizard-save"
            >
              {t('orchestrator:workflowWizard.save')}
            </Button>
          </div>
        </Card.Body>
      </Card>
    </Dialog>
  );
}
