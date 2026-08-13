/**
 * 工作台项目笔记窄 hook（非第 8 个业务 controller）。
 *
 * Business Logic（为什么需要这个模块）:
 *   右侧「项目笔记」按项目 ID 自动保存到本机 SQLite；切项目与关 GUI 必须 flush，
 *   且不得把 Tiptap 打进未打开 notes tab 的初始包。
 *
 * Code Logic（这个模块做什么）:
 *   仅 notes tab 拉取正文；复用速记本 debounce 队列按 projectId 保存；
 *   登记 pendingWrites；requestSeq 防 stale 回写。
 */

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { workbenchApi } from '@/api/workbench';
import { pendingWrites } from '@/lib/pendingWrites';
import {
  createScratchpadAutosaveQueue,
  SCRATCHPAD_AUTOSAVE_DELAY_MS,
} from '@/hooks/scratchpadAutosave';
import { displayErrorMessage } from './workbenchPageHelpers';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';

/** pendingWrites 登记 id。 */
export const WORKBENCH_PROJECT_NOTES_PENDING_WRITE_ID = 'workbench-project-notes';

/**
 * hook 入参。
 *
 * Business Logic（为什么需要这个类型）:
 *   组合层只注入当前项目、tab 与错误文案，避免 hook 再读页面其它域。
 */
export interface UseWorkbenchProjectNotesParams {
  activeProjectId: string | null;
  inspectorTab: WorkbenchInspectorTab;
  desktopUnavailableMessage: string;
  loadFailedFallback: string;
}

/**
 * hook 输出。
 *
 * Business Logic（为什么需要这个类型）:
 *   Notes 叶子只需正文、加载/保存/错误态与编辑/重试回调。
 */
export interface UseWorkbenchProjectNotesResult {
  content: string;
  loading: boolean;
  saving: boolean;
  error: string | null;
  onChange: (next: string) => void;
  onRetry: () => void;
}

interface ProjectNotesState {
  projectId: string | null;
  phase: 'idle' | 'ready' | 'error';
  content: string;
  error: string | null;
}

/** 按项目创建未加载的笔记视图状态，切项目时旧正文不会短暂泄露。 */
function createProjectNotesState(projectId: string | null): ProjectNotesState {
  return {
    projectId,
    phase: 'idle',
    content: '',
    error: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 组合层需要项目笔记状态，但不能再增第 8 个 controller。
 *
 * Code Logic（这个函数做什么）:
 *   打开 notes 时按 projectId 加载；编辑走 debounce；切项目先 flush。
 */
export function useWorkbenchProjectNotes(
  params: UseWorkbenchProjectNotesParams,
): UseWorkbenchProjectNotesResult {
  const { activeProjectId, inspectorTab, desktopUnavailableMessage, loadFailedFallback } = params;
  const notesOpen = inspectorTab === 'notes';

  const [notesState, setNotesState] = useState<ProjectNotesState>(() =>
    createProjectNotesState(activeProjectId),
  );

  if (notesState.projectId !== activeProjectId) {
    setNotesState(createProjectNotesState(activeProjectId));
  }

  const requestSeqRef = useRef(0);
  const previousProjectIdRef = useRef<string | null>(activeProjectId);

  const [queue] = useState(() =>
    createScratchpadAutosaveQueue(
      async (projectId, nextContent) => {
        await workbenchApi.notes.save(projectId, nextContent);
      },
      { delayMs: SCRATCHPAD_AUTOSAVE_DELAY_MS },
    ),
  );

  const snapshot = useSyncExternalStore(queue.subscribe, queue.getSnapshot, queue.getSnapshot);
  const projectSnapshot = activeProjectId ? snapshot.pages[activeProjectId] : undefined;
  const saving = Boolean(projectSnapshot?.inFlight || (projectSnapshot && projectSnapshot.pendingVersion > projectSnapshot.savedVersion));

  useEffect(() => {
    return pendingWrites.register(WORKBENCH_PROJECT_NOTES_PENDING_WRITE_ID, () => queue.flushAll());
  }, [queue]);

  useEffect(() => {
    const previousId = previousProjectIdRef.current;
    if (previousId && previousId !== activeProjectId) {
      void queue.flushPage(previousId);
    }
    previousProjectIdRef.current = activeProjectId;
  }, [activeProjectId, queue]);

  useEffect(() => {
    if (
      !notesOpen ||
      !activeProjectId ||
      notesState.projectId !== activeProjectId ||
      notesState.phase !== 'idle'
    ) {
      return undefined;
    }

    const seq = requestSeqRef.current + 1;
    requestSeqRef.current = seq;
    const projectId = activeProjectId;
    let cancelled = false;

    void workbenchApi.notes
      .get(projectId)
      .then((note) => {
        if (cancelled || requestSeqRef.current !== seq) return;
        setNotesState({
          projectId,
          phase: 'ready',
          content: note.content,
          error: null,
        });
      })
      .catch((reason: unknown) => {
        if (cancelled || requestSeqRef.current !== seq) return;
        setNotesState({
          projectId,
          phase: 'error',
          content: '',
          error: displayErrorMessage(reason, loadFailedFallback, desktopUnavailableMessage),
        });
      });

    return () => {
      cancelled = true;
    };
  }, [activeProjectId, desktopUnavailableMessage, loadFailedFallback, notesOpen, notesState]);

  const onChange = useCallback(
    (next: string) => {
      if (
        !activeProjectId ||
        notesState.projectId !== activeProjectId ||
        notesState.phase !== 'ready'
      ) {
        return;
      }
      setNotesState((previous) => ({ ...previous, content: next }));
      queue.schedule(activeProjectId, next);
    },
    [activeProjectId, notesState.phase, notesState.projectId, queue],
  );

  const onRetry = useCallback(() => {
    if (!activeProjectId) return;
    setNotesState(createProjectNotesState(activeProjectId));
  }, [activeProjectId]);

  const stateMatchesProject = notesState.projectId === activeProjectId;
  const content = stateMatchesProject && notesState.phase === 'ready' ? notesState.content : '';
  const loading = Boolean(
    notesOpen && activeProjectId && (!stateMatchesProject || notesState.phase === 'idle'),
  );
  const error = stateMatchesProject && notesState.phase === 'error' ? notesState.error : null;

  return {
    content,
    loading,
    saving,
    error,
    onChange,
    onRetry,
  };
}
