/**
 * 用户级原生提示词文件控制器。
 *
 * Business Logic（为什么需要）:
 *   用户级提示词直接读写各 CLI 配置目录里的真实文件；路径相同才共用草稿。
 *
 * Code Logic（做什么）:
 *   inspect workspace 得到路径/共用关系；按路径缓存；缺正文时 read；保存走 CAS write。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi } from '@/api/agentHub';
import type { AgentTarget, UserInstructionWorkspaceDto } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import {
  filesFromUserWorkspace,
  resolveActiveUserFileId,
  shouldGuardUserNativeInstructionContextChange,
  userFilesForAgent,
  type UserNativeFileSpec,
} from './userInstructionFiles';

/** 单份文件的编辑缓存。 */
export interface UserNativeFileState {
  spec: UserNativeFileSpec;
  diskPath: string | null;
  exists: boolean;
  draft: string;
  savedContent: string;
  baseHash: string | null;
  truncated: boolean;
  notice: string | null;
  dirty: boolean;
}

export type UserNativeBusyAction = 'load' | 'save' | null;

export interface UseUserInstructionFilesControllerArgs {
  deviceId: string | null;
  agent: AgentTarget;
  enabled: boolean;
  active: boolean;
  t: TFunction<['agentHub', 'common']>;
}

export interface UseUserInstructionFilesControllerResult {
  files: UserNativeFileState[];
  activeFile: UserNativeFileState | null;
  activeFileId: string | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  busyAction: UserNativeBusyAction;
  dirty: boolean;
  selectFile: (id: string) => void;
  editActiveFile: (value: string) => void;
  saveActiveFile: () => Promise<boolean>;
  saveAllDirty: () => Promise<boolean>;
  refresh: () => Promise<void>;
  discardDraftForContextChange: () => void;
  shouldGuardContextChange: (next: AgentHubContext) => boolean;
}

interface FileCacheEntry {
  spec: UserNativeFileSpec;
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

function toFileState(entry: FileCacheEntry): UserNativeFileState {
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

function requestContext(deviceId: string | null) {
  return { deviceId, projectRef: null };
}

/**
 * Business Logic: 用户级提示词只编辑真实文件；路径相同的 Agent 共用草稿。
 * Code Logic: 缓存按规范化路径；load generation 丢弃过期响应。
 */
export function useUserInstructionFilesController(
  args: UseUserInstructionFilesControllerArgs,
): UseUserInstructionFilesControllerResult {
  const { deviceId, agent, enabled, active, t } = args;
  const [workspace, setWorkspace] = useState<UserInstructionWorkspaceDto | null>(null);
  const [cache, setCache] = useState<Record<string, FileCacheEntry>>({});
  const [activeFileId, setActiveFileId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<UserNativeBusyAction>(null);
  const mountedRef = useRef(true);
  const loadGenerationRef = useRef(0);
  const cacheRef = useRef(cache);
  const workspaceRef = useRef(workspace);
  const deviceIdRef = useRef(deviceId);
  const previousDeviceRef = useRef<string | null | undefined>(undefined);

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
    workspaceRef.current = workspace;
  }, [workspace]);

  useEffect(() => {
    deviceIdRef.current = deviceId;
  }, [deviceId]);

  const catalog = useMemo(() => filesFromUserWorkspace(workspace), [workspace]);
  const catalogSpecs = useMemo(() => catalog.map((item) => item.spec), [catalog]);
  const visibleSpecs = useMemo(
    () => userFilesForAgent(catalogSpecs, agent),
    [agent, catalogSpecs],
  );

  const loadFiles = useCallback(
    async (options?: { force?: boolean }) => {
      if (!enabled || !active) return;
      const generation = loadGenerationRef.current + 1;
      loadGenerationRef.current = generation;
      const force = options?.force === true;
      const isInitial = workspaceRef.current === null;
      if (isInitial) setLoading(true);
      else setRefreshing(true);
      setError(null);
      setBusyAction('load');
      try {
        const nextWorkspace = await agentHubApi.inspectUserInstructionWorkspace(
          requestContext(deviceId),
        );
        if (!mountedRef.current || loadGenerationRef.current !== generation) return;
        if (deviceIdRef.current !== deviceId) return;
        setWorkspace(nextWorkspace);
        const nextCatalog = filesFromUserWorkspace(nextWorkspace);
        const nextEntries: Record<string, FileCacheEntry> = {};
        for (const item of nextCatalog) {
          const cached = cacheRef.current[item.spec.id];
          if (cached && cached.draft !== cached.savedContent && !force) {
            nextEntries[item.spec.id] = { ...cached, spec: item.spec };
            continue;
          }
          const source = item.source;
          if (source && typeof source.content === 'string') {
            nextEntries[item.spec.id] = {
              spec: item.spec,
              diskPath: source.path,
              exists: source.exists,
              draft: source.content,
              savedContent: source.content,
              baseHash: source.hash,
              truncated: Boolean(source.contentTruncated),
              notice: source.reasonCode,
            };
            continue;
          }
          if (source?.exists) {
            const read = await agentHubApi.readUserNativeInstructionFile({
              path: source.path,
              ...requestContext(deviceId),
            });
            if (!mountedRef.current || loadGenerationRef.current !== generation) return;
            if (deviceIdRef.current !== deviceId) return;
            nextEntries[item.spec.id] = {
              spec: item.spec,
              diskPath: read.path,
              exists: read.exists,
              draft: read.content,
              savedContent: read.content,
              baseHash: read.hash,
              truncated: read.truncated,
              notice: null,
            };
            continue;
          }
          nextEntries[item.spec.id] = {
            spec: item.spec,
            diskPath: item.spec.path,
            exists: false,
            draft: cached?.draft ?? '',
            savedContent: '',
            baseHash: null,
            truncated: false,
            notice: null,
          };
        }
        setCache(nextEntries);
      } catch (caught) {
        if (!mountedRef.current || loadGenerationRef.current !== generation) return;
        setError(errorMessage(caught) || t('agentHub:instructions.userFiles.loadFailed'));
      } finally {
        if (mountedRef.current && loadGenerationRef.current === generation) {
          setLoading(false);
          setRefreshing(false);
          setBusyAction(null);
        }
      }
    },
    [active, deviceId, enabled, t],
  );

  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- 换设备清缓存；打开提示词 tab 后 hydrate。 */
    const previous = previousDeviceRef.current;
    previousDeviceRef.current = deviceId;
    if (previous !== undefined && previous !== deviceId) {
      loadGenerationRef.current += 1;
      setCache({});
      setWorkspace(null);
      setError(null);
      setActionError(null);
      setActiveFileId(null);
    }
    if (!enabled || !active) return;
    void loadFiles();
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [active, deviceId, enabled, loadFiles]);

  const files = useMemo(
    () =>
      visibleSpecs.map((spec) => {
        const entry = cache[spec.id];
        if (entry) return toFileState(entry);
        return toFileState({
          spec,
          diskPath: spec.path,
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

  const resolvedActiveId = resolveActiveUserFileId(catalogSpecs, agent, activeFileId);
  const activeFile = files.find((file) => file.spec.id === resolvedActiveId) ?? files[0] ?? null;

  useEffect(() => {
    if (resolvedActiveId && resolvedActiveId !== activeFileId) {
      setActiveFileId(resolvedActiveId);
    }
  }, [activeFileId, resolvedActiveId]);

  const dirty = useMemo(
    () => Object.values(cache).some((entry) => entry.draft !== entry.savedContent),
    [cache],
  );

  const dirtyFileIds = useMemo(
    () =>
      Object.entries(cache)
        .filter(([, entry]) => entry.draft !== entry.savedContent)
        .map(([id]) => id),
    [cache],
  );

  const selectFile = useCallback((id: string) => {
    setActiveFileId(id);
    setActionError(null);
  }, []);

  const editActiveFile = useCallback(
    (value: string) => {
      const id = resolveActiveUserFileId(catalogSpecs, agent, activeFileId);
      if (!id) return;
      const spec = catalogSpecs.find((file) => file.id === id);
      if (!spec) return;
      setActionError(null);
      setCache((current) => {
        const previous = current[id];
        return {
          ...current,
          [id]: {
            spec,
            diskPath: previous?.diskPath ?? spec.path,
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
    [activeFileId, agent, catalogSpecs],
  );

  const persistFile = useCallback(
    async (id: string): Promise<boolean> => {
      const entry = cacheRef.current[id];
      if (!entry) return true;
      if (entry.draft === entry.savedContent && entry.exists) return true;
      if (entry.truncated) {
        setActionError(t('agentHub:instructions.userFiles.truncated'));
        return false;
      }
      setBusyAction('save');
      setActionError(null);
      try {
        const saved = await agentHubApi.writeUserNativeInstructionFile({
          path: entry.diskPath ?? entry.spec.path,
          content: entry.draft,
          expectedHash: entry.exists ? entry.baseHash : null,
          ...requestContext(deviceId),
        });
        if (!mountedRef.current || deviceIdRef.current !== deviceId) return false;
        setCache((current) => {
          const latest = current[id];
          if (!latest) return current;
          return {
            ...current,
            [id]: {
              ...latest,
              exists: true,
              diskPath: saved.path,
              savedContent: latest.draft,
              baseHash: saved.hash,
              truncated: saved.truncated,
              notice: null,
            },
          };
        });
        return true;
      } catch (caught) {
        if (!mountedRef.current) return false;
        setActionError(errorMessage(caught) || t('agentHub:instructions.userFiles.saveFailed'));
        return false;
      } finally {
        if (mountedRef.current) setBusyAction(null);
      }
    },
    [deviceId, t],
  );

  const saveActiveFile = useCallback(async () => {
    const id = resolveActiveUserFileId(catalogSpecs, agent, activeFileId);
    if (!id) return false;
    return persistFile(id);
  }, [activeFileId, agent, catalogSpecs, persistFile]);

  const saveAllDirty = useCallback(async () => {
    const ids = Object.entries(cacheRef.current)
      .filter(([, entry]) => entry.draft !== entry.savedContent)
      .map(([id]) => id);
    for (const id of ids) {
      const ok = await persistFile(id);
      if (!ok) return false;
    }
    return true;
  }, [persistFile]);

  const refresh = useCallback(async () => {
    await loadFiles({ force: true });
  }, [loadFiles]);

  const discardDraftForContextChange = useCallback(() => {
    setCache((current) => {
      const next: Record<string, FileCacheEntry> = {};
      for (const [id, entry] of Object.entries(current)) {
        next[id] = { ...entry, draft: entry.savedContent };
      }
      return next;
    });
    setActionError(null);
  }, []);

  const shouldGuardContextChange = useCallback(
    (next: AgentHubContext) =>
      shouldGuardUserNativeInstructionContextChange({
        dirtyFileIds,
        currentDeviceId: deviceId,
        nextTab: next.tab,
        nextAgent: next.agent,
        nextScope: next.scope,
        nextDeviceId: next.deviceId,
        visibleFileIdsForAgent: (nextAgent) =>
          userFilesForAgent(catalogSpecs, nextAgent).map((file) => file.id),
      }),
    [catalogSpecs, deviceId, dirtyFileIds],
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
