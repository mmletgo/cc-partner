/**
 * WorkbenchHtmlPreview 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 打开 HTML 文件时，用户需要像 Markdown 一样在源码、渲染预览和分屏之间切换，
 *   同时源码仍然保持普通文本保存语义，避免把 HTML 当成单一代码编辑器处理。
 *
 * Code Logic（这个组件做什么）:
 *   复用 WorkbenchCodeEditor 渲染 HTML 源码，预览前把项目内相对资源改写为 data URL，
 *   再使用 sandboxed iframe 的 srcDoc 渲染；mode=wysiwyg 在 HTML 语境中显示为 Preview。
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { WorkbenchCodeEditor } from '../WorkbenchCodeEditor';
import { rewriteHtmlPreviewAssets } from './htmlAssets';
import type { WorkbenchHtmlAssetLoader } from './htmlAssets';
import styles from './WorkbenchHtmlPreview.module.css';

export type WorkbenchHtmlMode = 'wysiwyg' | 'source' | 'split';

export interface WorkbenchHtmlPreviewProps {
  value: string;
  documentPath?: string;
  mode: WorkbenchHtmlMode;
  readOnly?: boolean;
  loadAsset?: WorkbenchHtmlAssetLoader;
  onModeChange: (mode: WorkbenchHtmlMode) => void;
  onChange: (value: string) => void;
}

const HTML_MODE_OPTIONS = [
  { mode: 'source', labelKey: 'workbench:htmlPreview.modes.source' },
  { mode: 'wysiwyg', labelKey: 'workbench:htmlPreview.modes.wysiwyg' },
  { mode: 'split', labelKey: 'workbench:htmlPreview.modes.split' },
] as const satisfies ReadonlyArray<{ mode: WorkbenchHtmlMode; labelKey: string }>;

/**
 * 渲染 HTML 文件源码和 sandbox 预览
 *
 * Business Logic（为什么需要这个组件）:
 *   用户编辑 HTML 时需要一边查看源码，一边在应用内确认基础渲染效果。
 *
 * Code Logic（这个组件做什么）:
 *   使用按钮组切换 mode；source 渲染 CodeMirror，wysiwyg 渲染改写后的 sandbox iframe，split 双列同时渲染。
 */
export function WorkbenchHtmlPreview({
  value,
  documentPath,
  mode,
  readOnly = false,
  loadAsset,
  onModeChange,
  onChange,
}: WorkbenchHtmlPreviewProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [previewResult, setPreviewResult] = useState<{ source: string; html: string } | null>(null);

  /**
   * Business Logic（为什么需要这个副作用）:
   *   HTML 源码变化、文件路径变化或 worktree 切换后，iframe 预览需要重新内联项目内相对资源。
   *
   * Code Logic（这个副作用做什么）:
   *   调用资源重写 helper 生成带 source 标记的 previewResult；异步期间若组件更新或卸载，则忽略旧结果。
   */
  useEffect(() => {
    let cancelled = false;

    if (!documentPath || !loadAsset) {
      return () => {
        cancelled = true;
      };
    }

    void rewriteHtmlPreviewAssets(value, { documentPath, loadAsset })
      .then((rewritten) => {
        if (!cancelled) {
          setPreviewResult({ source: value, html: rewritten });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setPreviewResult(null);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [documentPath, loadAsset, value]);
  const iframeSrcDoc =
    documentPath && loadAsset && previewResult?.source === value ? previewResult.html : value;

  /**
   * Business Logic（为什么需要这个函数）:
   *   HTML 文件的源码、预览和分屏切换需要交给父级 tab 状态持久保存。
   *
   * Code Logic（这个函数做什么）:
   *   接收按钮对应的 mode 并透传给 onModeChange，避免按钮层直接知道 tab id。
   */
  const handleModeClick = useCallback(
    (nextMode: WorkbenchHtmlMode) => {
      onModeChange(nextMode);
    },
    [onModeChange],
  );

  return (
    <div className={styles.htmlShell}>
      <div className={styles.modeBar} role="group" aria-label={t('workbench:htmlPreview.modeBar')}>
        {HTML_MODE_OPTIONS.map((option) => (
          <button
            key={option.mode}
            type="button"
            className={styles.modeButton}
            data-active={option.mode === mode}
            aria-pressed={option.mode === mode}
            onClick={() => handleModeClick(option.mode)}
          >
            {t(option.labelKey)}
          </button>
        ))}
      </div>

      <div className={styles.htmlBody} data-mode={mode}>
        {mode === 'source' || mode === 'split' ? (
          <div className={styles.sourcePane}>
            <WorkbenchCodeEditor
              value={value}
              language="html"
              readOnly={readOnly}
              onChange={onChange}
            />
          </div>
        ) : null}

        {mode === 'wysiwyg' || mode === 'split' ? (
          <div className={styles.previewPane}>
            <iframe
              className={styles.previewFrame}
              title={t('workbench:htmlPreview.frameTitle')}
              sandbox=""
              referrerPolicy="no-referrer"
              srcDoc={iframeSrcDoc}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}
