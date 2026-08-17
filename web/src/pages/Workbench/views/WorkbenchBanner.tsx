/**
 * WorkbenchBanner — 工作台顶栏 owning-device 标语。
 *
 * Business Logic（为什么需要）:
 *   用户要在顶栏中间写一句设备标语（轻量 Markdown + emoji），
 *   本机项目写本机、远端项目写对端；离线只读。
 *
 * Code Logic（做什么）:
 *   单击预览进入 textarea；debounce / 失焦 / ⌘Enter 经 hook 写 owning device；
 *   Esc 丢草稿；ResizeObserver + 离屏测量二分字号。
 */

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import type { KeyboardEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  BANNER_LINE_HEIGHT,
  BANNER_MAX_CHARS,
  BANNER_MIN_FONT_PX,
  BANNER_SAVE_DEBOUNCE_MS,
  fitBannerFontSize,
  parseBannerMarkdown,
  type BannerInline,
} from '../workbenchBanner';
import { useWorkbenchBanner } from '../useWorkbenchBanner';
import styles from './WorkbenchBanner.module.css';

export interface WorkbenchBannerProps {
  deviceId?: string;
  remoteWriteDisabled?: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   预览要把解析后的节点画成可点链接与强调，而不是源码。
 *
 * Code Logic（这个函数做什么）:
 *   按节点类型输出 span/code/a/br；链接拦截冒泡以免点进编辑。
 */
function renderBannerNodes(nodes: BannerInline[]): ReactElement[] {
  return nodes.map((node, index) => {
    const key = `${node.type}-${index}`;
    switch (node.type) {
      case 'break':
        return <br key={key} />;
      case 'strong':
        return <strong key={key}>{node.value}</strong>;
      case 'em':
        return <em key={key}>{node.value}</em>;
      case 'del':
        return <del key={key}>{node.value}</del>;
      case 'code':
        return (
          <code key={key} className={styles.code}>
            {node.value}
          </code>
        );
      case 'link':
        return (
          <a
            key={key}
            className={styles.link}
            href={node.href}
            target="_blank"
            rel="noopener noreferrer"
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => event.stopPropagation()}
          >
            {node.text}
          </a>
        );
      default:
        return <span key={key}>{node.value}</span>;
    }
  });
}

/**
 * Business Logic（为什么需要这个组件）:
 *   顶栏中间空档要变成可编辑、可缩放的本机标语，且不能膨胀 Workbench.tsx。
 *
 * Code Logic（这个函数做什么）:
 *   读/写 localStorage；预览/编辑两态；容器尺寸变化后重算字号。
 */
export function WorkbenchBanner(props: WorkbenchBannerProps = {}): ReactElement {
  const { deviceId, remoteWriteDisabled = false } = props;
  const { t } = useTranslation(['workbench']);
  const { markdown, persist: persistRemote, readOnly } = useWorkbenchBanner({
    deviceId,
    remoteWriteDisabled,
  });
  const limitId = useId();
  const frameRef = useRef<HTMLDivElement | null>(null);
  const previewRef = useRef<HTMLDivElement | null>(null);
  const measureRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const ignoreBlurRef = useRef(false);
  const saveTimerRef = useRef<number | null>(null);
  const [draft, setDraft] = useState(markdown);
  const [editing, setEditing] = useState(false);
  const [fontPx, setFontPx] = useState(BANNER_MIN_FONT_PX);
  const draftRef = useRef(markdown);
  const savedRef = useRef(markdown);

  if (!editing && draft !== markdown) {
    setDraft(markdown);
  }

  useEffect(() => {
    if (editing) return;
    savedRef.current = markdown;
    draftRef.current = markdown;
  }, [editing, markdown]);

  const persist = useCallback((next: string) => {
    void persistRemote(next).then(() => {
      savedRef.current = next;
      draftRef.current = next;
      setDraft(next);
    });
  }, [persistRemote]);

  const schedulePersist = useCallback(
    (next: string) => {
      if (saveTimerRef.current !== null) {
        window.clearTimeout(saveTimerRef.current);
      }
      saveTimerRef.current = window.setTimeout(() => {
        saveTimerRef.current = null;
        persist(next);
      }, BANNER_SAVE_DEBOUNCE_MS);
    },
    [persist],
  );

  useEffect(() => {
    return () => {
      if (saveTimerRef.current === null) return;
      window.clearTimeout(saveTimerRef.current);
      if (!readOnly) {
        void persistRemote(draftRef.current);
      }
    };
  }, [persistRemote, readOnly]);

  const refit = useCallback(() => {
    const preview = previewRef.current;
    const measure = measureRef.current;
    if (!preview || !measure) return;
    const styles = window.getComputedStyle(preview);
    const padX =
      (Number.parseFloat(styles.paddingLeft) || 0) +
      (Number.parseFloat(styles.paddingRight) || 0);
    const padY =
      (Number.parseFloat(styles.paddingTop) || 0) +
      (Number.parseFloat(styles.paddingBottom) || 0);
    const maxWidth = Math.max(0, preview.clientWidth - padX);
    const maxHeight = Math.max(0, preview.clientHeight - padY);
    const next = fitBannerFontSize({
      maxWidth,
      maxHeight,
      measure: (px) => {
        measure.style.fontSize = `${px}px`;
        return {
          width: measure.scrollWidth,
          height: measure.scrollHeight,
        };
      },
    });
    measure.style.fontSize = `${next}px`;
    setFontPx(next);
  }, []);

  useLayoutEffect(() => {
    if (editing) return;
    refit();
    const frame = frameRef.current;
    if (!frame || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(() => {
      refit();
    });
    observer.observe(frame);
    return () => observer.disconnect();
  }, [editing, markdown, refit]);

  useEffect(() => {
    if (!editing) return undefined;
    const frame = window.requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.focus();
      const end = el.value.length;
      el.setSelectionRange(end, end);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editing]);

  const beginEdit = useCallback(() => {
    if (readOnly) return;
    const saved = savedRef.current;
    draftRef.current = saved;
    setDraft(saved);
    setEditing(true);
  }, [readOnly]);

  const commitEdit = useCallback(() => {
    if (saveTimerRef.current !== null) {
      window.clearTimeout(saveTimerRef.current);
      saveTimerRef.current = null;
    }
    persist(draftRef.current);
    setEditing(false);
  }, [persist]);

  const cancelEdit = useCallback(() => {
    ignoreBlurRef.current = true;
    if (saveTimerRef.current !== null) {
      window.clearTimeout(saveTimerRef.current);
      saveTimerRef.current = null;
    }
    const saved = savedRef.current;
    draftRef.current = saved;
    setDraft(saved);
    setEditing(false);
  }, []);

  const handleEditorBlur = useCallback(() => {
    if (ignoreBlurRef.current) {
      ignoreBlurRef.current = false;
      return;
    }
    commitEdit();
  }, [commitEdit]);

  const handleEditorKeyDown = useCallback(
    (event: KeyboardEvent<HTMLTextAreaElement>) => {
      if (event.nativeEvent.isComposing) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        cancelEdit();
        return;
      }
      if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        commitEdit();
      }
    },
    [cancelEdit, commitEdit],
  );

  const handlePreviewKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.nativeEvent.isComposing) return;
      if (event.target instanceof HTMLAnchorElement) return;
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        beginEdit();
      }
    },
    [beginEdit],
  );

  const handleDraftChange = useCallback(
    (value: string) => {
      const next = value.length > BANNER_MAX_CHARS ? value.slice(0, BANNER_MAX_CHARS) : value;
      draftRef.current = next;
      setDraft(next);
      schedulePersist(next);
    },
    [schedulePersist],
  );

  const nodes = parseBannerMarkdown(markdown);
  const isEmpty = markdown.trim().length === 0;
  const atLimit = draft.length >= BANNER_MAX_CHARS;

  return (
    <div ref={frameRef} className={styles.frame} data-testid="workbench-banner">
      {editing ? (
        <>
          <textarea
            ref={textareaRef}
            className={styles.editor}
            style={{ fontSize: `${fontPx}px`, lineHeight: BANNER_LINE_HEIGHT }}
            value={draft}
            spellCheck={false}
            aria-label={t('workbench:banner.editorAriaLabel')}
            aria-describedby={atLimit ? limitId : undefined}
            data-testid="workbench-banner-editor"
            onChange={(event) => handleDraftChange(event.target.value)}
            onBlur={handleEditorBlur}
            onKeyDown={handleEditorKeyDown}
          />
          {atLimit ? (
            <p id={limitId} className={styles.limit} role="status">
              {t('workbench:banner.limitReached', { max: BANNER_MAX_CHARS })}
            </p>
          ) : null}
        </>
      ) : (
        <div
          ref={previewRef}
          className={styles.preview}
          role="group"
          tabIndex={0}
          data-empty={isEmpty || undefined}
          data-readonly={readOnly || undefined}
          data-testid="workbench-banner-preview"
          aria-label={t('workbench:banner.previewAriaLabel')}
          onClick={beginEdit}
          onKeyDown={handlePreviewKeyDown}
        >
          <div
            ref={measureRef}
            className={styles.measure}
            style={{ fontSize: `${fontPx}px`, lineHeight: BANNER_LINE_HEIGHT }}
          >
            {isEmpty ? t('workbench:banner.placeholder') : renderBannerNodes(nodes)}
          </div>
        </div>
      )}
    </div>
  );
}
