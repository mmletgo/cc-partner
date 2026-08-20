/**
 * 项目级原生提示词文件控制器。
 *
 * Business Logic（为什么需要）:
 *   项目 Agent 直接读写各 CLI 实际加载的仓库根文件；共用 AGENTS.md 必须共用一份草稿。
 *
 * Code Logic（做什么）:
 *   按项目缓存文件草稿；listDir 探测存在性后 open；缺失文件可在保存时 create；
 *   generation + mounted 防止项目切换 stale 写入。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { workbenchApi } from '@/api/workbench';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import {
  filesForAgent,
  matchProjectInstructionNodeName,
  resolveActiveFileId,
  shouldGuardProjectInstructionContextChange,
  type ProjectInstructionFileId,
  type ProjectInstructionFileSpec,
} from './projectInstructionFiles';

/** 单份文件的编辑缓存。 */
export interface ProjectInstructionFileState {
  spec: ProjectInstructionFileSpec;
  /** 磁盘上的相对路径（可能与规范文件名大小写不同）。 */
  diskPath: string | null;
  exists: boolean;
  draft: string;
  savedContent: string;
  baseHash: string | null;
  truncated: boolean;
  notice: string | null;
  dirty: boolean;
}

export type ProjectInstructionBusyAction = 'load' | 'save' | null;

export interface UseProjectInstructionFilesControllerArgs {
  projectKey: string | null;
  agent: AgentTarget;
  /** 有项目身份即挂载缓存；false 时保留同 projectKey 草稿。 */
  enabled: boolean;
  /** 提示词 tab 可见时才向磁盘加载。 */
  active: boolean;
  t: TFunction<['agentHub', 'common']>;
}

export interface UseProjectInstructionFilesControllerResult {
  files: ProjectInstructionFileState[];
  activeFile: ProjectInstructionFileState | null;
  activeFileId: ProjectInstructionFileId | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  busyAction: ProjectInstructionBusyAction;
  dirty: boolean;
  selectFile: (id: ProjectInstructionFileId) => void;
  editActiveFile: (value: string) => void;
  saveActiveFile: () => Promise<boolean>;
  saveAllDirty: () => Promise<boolean>;
  refresh: () => Promise<void>;
  discardDraftForContextChange: () => void;
  shouldGuardContextChange: (next: AgentHubContext) => boolean;
}

interface FileCacheEntry {
  spec: ProjectInstructionFileSpec;
  diskPath: string | null;
  exists: boolean;
  draft: string;
  savedContent: string;
  baseHash: string | null;
  truncated: boolean;
  notice: string | null;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function toFileState(entry: FileCacheEntry): ProjectInstructionFileState {
  return {
    spec: entry.spec,
    diskPath: entry.diskPath,
    exists: entry.exists,
    draft: entry.draft,
    savedContent: entry.savedContent,
    baseHash: entry.baseHash,
    truncated: entry.truncated,
    notice: entry.notice,
    dirty: entry.draft !== entry.savedContent,
  };
}

/**
 * Business Logic: 项目 Agent 提示词只编辑真实文件；切共用 Agent 不丢草稿。
 * Code Logic: 缓存按 file id；load generation 丢弃过期响应。
 */
export function useProjectInstructionFilesController(
  args: UseProjectInstructionFilesControllerArgs,
): UseProjectInstructionFilesControllerResult {
  const { projectKey, agent, enabled, active, t } = args;
  const [cache, setCache] = useState<Partial<Record<ProjectInstructionFileId, FileCacheEntry>>>(
    {},
  );
  const [activeFileId, setActiveFileId] = useState<ProjectInstructionFileId | null>(() =>
    resolveActiveFileId(agent, null),
  );
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<ProjectInstructionBusyAction>(null);
  const mountedRef = useRef(true);
  const loadGenerationRef = useRef(0);
  const cacheRef = useRef(cache);
  const projectKeyRef = useRef(projectKey);
  const previousProjectKeyRef = useRef<string | null | undefined>(undefined);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    cacheRef.current = cache;
  }, [cache]);

  useEffect(() => {
    projectKeyRef.current = projectKey;
  }, [projectKey]);

  const visibleSpecs = useMemo(() => filesForAgent(agent), [agent]);

  const loadFiles = useCallback(
    async (options?: { force?: boolean }) => {
      if (!enabled || !active || !projectKey) return;
      const generation = loadGenerationRef.current + 1;
      loadGenerationRef.current = generation;
      const force = options?.force === true;
      const specsToLoad = visibleSpecs.filter((spec) => {
        const existing = cacheRef.current[spec.id];
        if (!existing) return true;
        if (force && existing.draft === existing.savedContent) return true;
        return false;
      });
      if (specsToLoad.length === 0) {
        if (mountedRef.current && loadGenerationRef.current === generation) {
          setLoading(false);
          setRefreshing(false);
        }
        return;
      }
      const isInitial = visibleSpecs.some((spec) => !cacheRef.current[spec.id]);
      if (isInitial) setLoading(true);
      else setRefreshing(true);
      setError(null);
      setBusyAction('load');
      try {
        const nodes = await workbenchApi.files.listDir(projectKey, '', null);
        if (!mountedRef.current || loadGenerationRef.current !== generation) return;
        if (projectKeyRef.current !== projectKey) return;
        const names = nodes.filter((node) => node.kind === 'file').map((node) => node.name);
        const nextEntries: Partial<Record<ProjectInstructionFileId, FileCacheEntry>> = {};
        for (const spec of visibleSpecs) {
          const cached = cacheRef.current[spec.id];
          const skipReload = cached && cached.draft !== cached.savedContent;
          if (skipReload) {
            nextEntries[spec.id] = cached;
            continue;
          }
          if (!force && cached && !specsToLoad.some((item) => item.id === spec.id)) {
            nextEntries[spec.id] = cached;
            continue;
          }
          const diskName = matchProjectInstructionNodeName(names, spec.path);
          if (!diskName) {
            nextEntries[spec.id] = {
              spec,
              diskPath: null,
              exists: false,
              draft: cached?.draft ?? '',
              savedContent: '',
              baseHash: null,
              truncated: false,
              notice: null,
            };
            continue;
          }
          const opened = await workbenchApi.files.open(projectKey, diskName, null);
          if (!mountedRef.current || loadGenerationRef.current !== generation) return;
          if (projectKeyRef.current !== projectKey) return;
          const content = opened.text?.content ?? '';
          nextEntries[spec.id] = {
            spec,
            diskPath: diskName,
            exists: true,
            draft: content,
            savedContent: content,
            baseHash: opened.text?.baseHash ?? null,
            truncated: opened.truncated,
            notice: opened.notice,
          };
        }
        setCache((current) => ({ ...current, ...nextEntries }));
      } catch (caught) {
        if (!mountedRef.current || loadGenerationRef.current !== generation) return;
        setError(errorMessage(caught) || t('agentHub:instructions.projectFiles.loadFailed'));
      } finally {
        if (mountedRef.current && loadGenerationRef.current === generation) {
          setLoading(false);
          setRefreshing(false);
          setBusyAction(null);
        }
      }
    },
    [active, enabled, projectKey, t, visibleSpecs],
  );

  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- 换项目清缓存；打开提示词 tab 后从磁盘 hydrate。 */
    const previous = previousProjectKeyRef.current;
    previousProjectKeyRef.current = projectKey;
    if (previous !== undefined && previous !== projectKey) {
      loadGenerationRef.current += 1;
      setCache({});
      setError(null);
      setActionError(null);
    }
    if (!enabled || !active || !projectKey) return;
    void loadFiles();
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [active, enabled, loadFiles, projectKey]);

  const files = useMemo(
    () =>
      visibleSpecs.map((spec) => {
        const entry = cache[spec.id];
        if (entry) return toFileState(entry);
        return toFileState({
          spec,
          diskPath: null,
          exists: false,
          draft: '',
          savedContent: '',
          baseHash: null,
          truncated: false,
          notice: null,
        });
      }),
    [cache, visibleSpecs],
  );

  const resolvedActiveId = resolveActiveFileId(agent, activeFileId);
  const activeFile = files.find((file) => file.spec.id === resolvedActiveId) ?? files[0] ?? null;

  const dirty = useMemo(
    () => Object.values(cache).some((entry) => entry && entry.draft !== entry.savedContent),
    [cache],
  );

  const dirtyFileIds = useMemo(
    () =>
      (Object.entries(cache) as Array<[ProjectInstructionFileId, FileCacheEntry | undefined]>)
        .filter(([, entry]) => entry && entry.draft !== entry.savedContent)
        .map(([id]) => id),
    [cache],
  );

  const selectFile = useCallback((id: ProjectInstructionFileId) => {
    setActiveFileId(id);
    setActionError(null);
  }, []);

  const editActiveFile = useCallback(
    (value: string) => {
      const id = resolveActiveFileId(agent, activeFileId);
      if (!id) return;
      const spec = filesForAgent(agent).find((file) => file.id === id);
      if (!spec) return;
      setActionError(null);
      setCache((current) => {
        const previous = current[id];
        return {
          ...current,
          [id]: {
            spec,
            diskPath: previous?.diskPath ?? null,
            exists: previous?.exists ?? false,
            draft: value,
            savedContent: previous?.savedContent ?? '',
            baseHash: previous?.baseHash ?? null,
            truncated: previous?.truncated ?? false,
            notice: previous?.notice ?? null,
          },
        };
      });
    },
    [activeFileId, agent],
  );

  const persistFile = useCallback(
    async (id: ProjectInstructionFileId): Promise<boolean> => {
      if (!projectKey) return false;
      const entry = cacheRef.current[id];
      if (!entry) return true;
      if (entry.draft === entry.savedContent && entry.exists) return true;
      setBusyAction('save');
      setActionError(null);
      try {
        let diskPath = entry.diskPath ?? entry.spec.path;
        let baseHash = entry.baseHash;
        if (!entry.exists || !baseHash) {
          try {
            await workbenchApi.files.createFile(projectKey, '', entry.spec.path, null);
          } catch {
            // 磁盘上已有该文件时改为打开，避免 create_new 挡住保存。
          }
          const opened = await workbenchApi.files.open(projectKey, entry.spec.path, null);
          diskPath = opened.metadata.path || entry.spec.path;
          baseHash = opened.text?.baseHash ?? null;
          if (!baseHash) {
            throw new Error(t('agentHub:instructions.projectFiles.saveFailed'));
          }
        }
        const saved = await workbenchApi.files.saveText(
          projectKey,
          diskPath,
          entry.draft,
          baseHash,
          null,
        );
        if (!mountedRef.current || projectKeyRef.current !== projectKey) return false;
        setCache((current) => {
          const latest = current[id];
          if (!latest) return current;
          return {
            ...current,
            [id]: {
              ...latest,
              exists: true,
              diskPath,
              savedContent: latest.draft,
              baseHash: saved.baseHash,
              notice: null,
            },
          };
        });
        return true;
      } catch (caught) {
        if (!mountedRef.current) return false;
        setActionError(errorMessage(caught) || t('agentHub:instructions.projectFiles.saveFailed'));
        return false;
      } finally {
        if (mountedRef.current) setBusyAction(null);
      }
    },
    [projectKey, t],
  );

  const saveActiveFile = useCallback(async () => {
    const id = resolveActiveFileId(agent, activeFileId);
    if (!id) return false;
    return persistFile(id);
  }, [activeFileId, agent, persistFile]);

  const saveAllDirty = useCallback(async () => {
    const ids = dirtyFileIds;
    for (const id of ids) {
      const ok = await persistFile(id);
      if (!ok) return false;
    }
    return true;
  }, [dirtyFileIds, persistFile]);

  const refresh = useCallback(async () => {
    await loadFiles({ force: true });
  }, [loadFiles]);

  const discardDraftForContextChange = useCallback(() => {
    setCache((current) => {
      const next: Partial<Record<ProjectInstructionFileId, FileCacheEntry>> = {};
      for (const [id, entry] of Object.entries(current) as Array<
        [ProjectInstructionFileId, FileCacheEntry | undefined]
      >) {
        if (!entry) continue;
        next[id] = {
          ...entry,
          draft: entry.savedContent,
        };
      }
      return next;
    });
    setActionError(null);
  }, []);

  const shouldGuardContextChange = useCallback(
    (next: AgentHubContext) =>
      shouldGuardProjectInstructionContextChange({
        dirtyFileIds,
        currentProjectKey: projectKey,
        nextTab: next.tab,
        nextAgent: next.agent,
        nextScope: next.scope,
        nextProjectKey: next.projectKey,
      }),
    [dirtyFileIds, projectKey],
  );

  return {
    files,
    activeFile,
    activeFileId: resolvedActiveId,
    loading,
    refreshing,
    error,
    actionError,
    actionBusy: busyAction !== null,
    busyAction,
    dirty,
    selectFile,
    editActiveFile,
    saveActiveFile,
    saveAllDirty,
    refresh,
    discardDraftForContextChange,
    shouldGuardContextChange,
  };
}
