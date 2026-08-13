/**
 * Workbench 项目笔记叶子视图。
 *
 * Business Logic（为什么需要这个组件）:
 *   右侧检查器第三 tab 需要所见即所得 Markdown 编辑，且不得把 Tiptap 打进 inspector 外壳。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：加载/错误/空项目态 + lazy WorkbenchMarkdownEditor（wysiwyg，隐藏 mode bar）。
 */

import { lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, StatusMessage } from '@/components/primitives';
import type { WorkbenchMarkdownMode } from '@/components/domain/WorkbenchMarkdownEditor';
import styles from './Workbench.module.css';

const WorkbenchMarkdownEditor = lazy(() =>
  import('@/components/domain/WorkbenchMarkdownEditor').then((module) => ({
    default: module.WorkbenchMarkdownEditor,
  })),
);

/**
 * 项目笔记叶子 props。
 *
 * Business Logic（为什么需要这个类型）:
 *   inspector 只转发 hook 结果，不持有 API。
 */
export interface WorkbenchNotesInspectorProps {
  activeProjectId: string | null;
  content: string;
  loading: boolean;
  saving: boolean;
  error: string | null;
  onChange: (next: string) => void;
  onRetry: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户在当前项目下随手记 Markdown，需与文件夹/Git 历史并列。
 *
 * Code Logic（这个组件做什么）:
 *   无项目/加载/错误早退；有正文时 lazy 挂载 wysiwyg 编辑器。
 */
export function WorkbenchNotesInspector(props: WorkbenchNotesInspectorProps) {
  const { t } = useTranslation(['workbench']);
  const { activeProjectId, content, loading, saving, error, onChange, onRetry } = props;

  if (!activeProjectId) {
    return <p className={styles.muted}>{t('workbench:notesNoProject')}</p>;
  }

  if (loading && !content) {
    return <p className={styles.muted}>{t('workbench:notesLoading')}</p>;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   笔记 tab 固定 wysiwyg，不提供分屏。
   *
   * Code Logic（这个函数做什么）:
   *   no-op，满足编辑器必填 onModeChange。
   */
  const handleModeChange: (mode: WorkbenchMarkdownMode) => void = () => undefined;

  return (
    <div className={styles.notesInspector} data-testid="workbench-notes-inspector">
      {error ? (
        <StatusMessage
          tone="danger"
          action={
            <Button type="button" variant="ghost" onClick={onRetry}>
              {t('workbench:notesRetry')}
            </Button>
          }
        >
          {error}
        </StatusMessage>
      ) : null}
      {saving ? <p className={styles.muted}>{t('workbench:notesSaving')}</p> : null}
      <Suspense fallback={<p className={styles.muted}>{t('workbench:notesEditorLoading')}</p>}>
        <WorkbenchMarkdownEditor
          value={content}
          mode="wysiwyg"
          showModeBar={false}
          onModeChange={handleModeChange}
          onChange={onChange}
        />
      </Suspense>
    </div>
  );
}
