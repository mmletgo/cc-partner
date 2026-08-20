/**
 * 原生提示词文件编辑视图（项目级 / 用户级共用）。
 *
 * Business Logic（为什么需要）:
 *   按真实文件编辑；路径相同的 AGENTS.md 只显示一份编辑器，并标明共用 Agent。
 *
 * Code Logic（做什么）:
 *   只消费 labels/state/callbacks；禁止 @/api。
 */

import { type JSX } from 'react';
import { Button, Pill, StatusMessage } from '@/components/primitives';
import type { AgentTarget } from '@/lib/types/agentHub';
import styles from './ProjectInstructionFilesView.module.css';

/** 原生提示词文件编辑文案。 */
export interface ProjectInstructionFilesViewLabels {
  title: string;
  loading: string;
  retry: string;
  save: string;
  unsaved: string;
  missing: string;
  editorAria: string;
  placeholder: string;
  pathLabel: string;
  filesAria: string;
  truncated: string;
  empty?: string;
  sharedBy: (agents: string) => string;
  exclusiveTo: (agent: string) => string;
  agentSeparator: string;
  agentName: (agent: AgentTarget) => string;
}

/** 视图只消费的文件状态（id 可以是 basename 或绝对路径）。 */
export interface NativeInstructionFileViewState {
  spec: {
    id: string;
    path: string;
    consumers: readonly AgentTarget[];
  };
  diskPath: string | null;
  exists: boolean;
  draft: string;
  dirty: boolean;
  truncated: boolean;
  notice: string | null;
}

/** 视图只消费的控制器子集。 */
export interface NativeInstructionFilesViewController {
  files: NativeInstructionFileViewState[];
  activeFile: NativeInstructionFileViewState | null;
  activeFileId: string | null;
  loading: boolean;
  actionBusy: boolean;
  busyAction: 'load' | 'save' | null;
  error: string | null;
  actionError: string | null;
  selectFile(id: string): void;
  editActiveFile(value: string): void;
  saveActiveFile(): Promise<boolean>;
  refresh(): Promise<void>;
}

export interface ProjectInstructionFilesViewProps {
  labels: ProjectInstructionFilesViewLabels;
  controller: NativeInstructionFilesViewController;
  agent: AgentTarget;
  /** 测试 id 前缀；用户级传 `user-instruction-file`。 */
  testIdPrefix?: string;
}

/**
 * Business Logic: 一份共用文件只渲染一个编辑器；多文件 Agent 用 radiogroup 切换。
 * Code Logic: 文件 tab 仅在可见文件 > 1 时出现；保存挂在当前文件。
 */
/**
 * Business Logic: 用户级路径是绝对路径，tab 只需要文件名。
 * Code Logic: 按 / 或 \\ 取最后一段。
 */
function displayFileName(path: string): string {
  const parts = path.split(/[\\/]/u);
  return parts[parts.length - 1] ?? path;
}

export function ProjectInstructionFilesView(
  props: ProjectInstructionFilesViewProps,
): JSX.Element {
  const { labels, controller, agent, testIdPrefix = 'project-instruction' } = props;
  const activeFile = controller.activeFile;
  const consumers = activeFile?.spec.consumers ?? [];
  const consumerNames = consumers.map((target) => labels.agentName(target)).join(labels.agentSeparator);
  const sharedHint =
    consumers.length > 1
      ? labels.sharedBy(consumerNames)
      : consumers[0]
        ? labels.exclusiveTo(labels.agentName(consumers[0]))
        : null;

  return (
    <section
      className={styles.root}
      data-testid={`${testIdPrefix}-files`}
      data-agent={agent}
    >
      <div className={styles.toolbar}>
        <div className={styles.toolbarMeta}>
          <h2 className={styles.title}>{labels.title}</h2>
          {activeFile ? (
            <p className={styles.pathRow}>
              <span className={styles.pathLabel}>{labels.pathLabel}</span>
              <span className={styles.pathValue} data-testid={`${testIdPrefix}-path`}>
                {activeFile.diskPath ?? activeFile.spec.path}
              </span>
              {activeFile.dirty ? (
                <Pill tone="warn">{labels.unsaved}</Pill>
              ) : null}
            </p>
          ) : null}
          {sharedHint ? (
            <p className={styles.sharedBy} data-testid={`${testIdPrefix}-shared-by`}>
              {sharedHint}
            </p>
          ) : null}
        </div>
        <div className={styles.toolbarActions}>
          <Button
            variant="primary"
            size="sm"
            disabled={!activeFile || controller.actionBusy || !activeFile.dirty}
            loading={controller.busyAction === 'save'}
            onClick={() => {
              void controller.saveActiveFile();
            }}
            data-testid={`${testIdPrefix}-save`}
          >
            {labels.save}
          </Button>
        </div>
      </div>

      {controller.files.length > 1 ? (
        <div
          className={styles.fileTabs}
          role="radiogroup"
          aria-label={labels.filesAria}
          data-testid={`${testIdPrefix}-file-tabs`}
        >
          {controller.files.map((file) => {
            const selected = file.spec.id === controller.activeFileId;
            return (
              <Button
                key={file.spec.id}
                variant={selected ? 'primary' : 'ghost'}
                size="sm"
                role="radio"
                aria-checked={selected}
                onClick={() => controller.selectFile(file.spec.id)}
                data-testid={`${testIdPrefix}-file-tab-${file.spec.id}`}
              >
                {displayFileName(file.spec.path)}
                {file.dirty ? ` · ${labels.unsaved}` : ''}
              </Button>
            );
          })}
        </div>
      ) : null}

      {controller.loading ? (
        <StatusMessage tone="info" data-testid={`${testIdPrefix}-loading`}>
          {labels.loading}
        </StatusMessage>
      ) : null}

      {controller.error ? (
        <StatusMessage
          tone="danger"
          action={(
            <Button size="sm" variant="ghost" onClick={() => void controller.refresh()}>
              {labels.retry}
            </Button>
          )}
          data-testid={`${testIdPrefix}-error`}
        >
          {controller.error}
        </StatusMessage>
      ) : null}

      {controller.actionError ? (
        <StatusMessage tone="danger" data-testid={`${testIdPrefix}-action-error`}>
          {controller.actionError}
        </StatusMessage>
      ) : null}

      {!controller.loading && !controller.error && controller.files.length === 0 && labels.empty ? (
        <StatusMessage tone="info" data-testid={`${testIdPrefix}-empty`}>
          {labels.empty}
        </StatusMessage>
      ) : null}

      {activeFile && !activeFile.exists ? (
        <StatusMessage tone="info" data-testid={`${testIdPrefix}-missing`}>
          {labels.missing}
        </StatusMessage>
      ) : null}

      {activeFile?.truncated ? (
        <StatusMessage tone="warn" data-testid={`${testIdPrefix}-truncated`}>
          {labels.truncated}
        </StatusMessage>
      ) : null}

      {activeFile?.notice ? (
        <StatusMessage tone="warn" data-testid={`${testIdPrefix}-notice`}>
          {activeFile.notice}
        </StatusMessage>
      ) : null}

      {activeFile ? (
        <textarea
          className={styles.editor}
          value={activeFile.draft}
          aria-label={labels.editorAria}
          placeholder={labels.placeholder}
          disabled={controller.loading}
          onChange={(event) => controller.editActiveFile(event.currentTarget.value)}
          data-testid={`${testIdPrefix}-editor`}
        />
      ) : null}
    </section>
  );
}
