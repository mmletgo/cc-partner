/**
 * 用户级原生提示词文件路径（各 CLI 配置目录里实际加载的 AGENTS.md / CLAUDE.md / GEMINI.md）。
 *
 * Business Logic（为什么需要）:
 *   用户级主编辑面仍是三槽；原始栏 / 写入原生文件应对准各 Agent **自己配置目录**里的约定文件。
 *   Codex `~/.codex/AGENTS.md` 与 OpenCode 配置根 `AGENTS.md` 是不同文件；
 *   只有绝对路径相同才共用原始草稿（例如 OpenCode 回退到 Claude 的 CLAUDE.md）。
 *   不得把 Hub 独有槽、override、rules 当成原始文件。
 *
 * Code Logic（做什么）:
 *   从 inspect workspace 挑 native 约定文件；按规范化路径判断是否共用。
 */

import { allHubTargets } from '@/lib/agentCatalog';
import type {
  AgentTarget,
  UserInstructionSourceDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';
import type { AgentHubScope, AgentHubTab } from '../context/agentHubContext';

/** 用户级约定文件种类。 */
export type UserNativeFileKind = 'agents' | 'claude' | 'gemini';

/** 一份用户级原生文件（按规范化绝对路径识别）。 */
export interface UserNativeFileSpec {
  id: string;
  kind: UserNativeFileKind;
  /** 展示 / 读写用的绝对路径。 */
  path: string;
  consumers: readonly AgentTarget[];
}

/** 各 Agent 在自己配置根下会加载的约定文件（不含 Hub 独有槽）。 */
export const USER_NATIVE_KINDS: Record<AgentTarget, readonly UserNativeFileKind[]> = {
  claude: ['claude'],
  codex: ['agents'],
  opencode: ['agents'],
  gemini: ['gemini'],
  grok: ['agents', 'claude'],
  cursor: [],
  pi: ['agents', 'claude'],
};

/**
 * Business Logic: 文件名是约定，目录是各 Agent 自己的 home。
 * Code Logic: kind → 规范文件名。
 */
export function basenameForKind(kind: UserNativeFileKind): string {
  if (kind === 'agents') return 'AGENTS.md';
  if (kind === 'claude') return 'CLAUDE.md';
  return 'GEMINI.md';
}

/**
 * Business Logic: 大小写不敏感磁盘上同一份文件必须合成一个 id。
 * Code Logic: 反斜杠改正斜杠后小写。
 */
export function canonicalUserFileId(path: string): string {
  return path.replace(/\\/gu, '/').toLowerCase();
}

/**
 * Business Logic: 配置根与文件名拼接必须保留原分隔符。
 * Code Logic: Windows 根用反斜杠，其余用正斜杠。
 */
export function joinHomeFile(configRoot: string, basename: string): string {
  const trimmed = configRoot.replace(/[\\/]+$/u, '');
  if (trimmed.includes('\\') && !trimmed.includes('/')) {
    return `${trimmed}\\${basename}`;
  }
  return `${trimmed}/${basename}`;
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/u);
  return parts[parts.length - 1] ?? path;
}

function parentDir(path: string): string {
  const normalized = path.replace(/\\/gu, '/').replace(/\/+$/u, '');
  const index = normalized.lastIndexOf('/');
  return index <= 0 ? normalized : normalized.slice(0, index);
}

function kindFromBasename(name: string): UserNativeFileKind | null {
  const lower = name.toLowerCase();
  if (lower === 'agents.md') return 'agents';
  if (lower === 'claude.md') return 'claude';
  if (lower === 'gemini.md') return 'gemini';
  return null;
}

/**
 * Business Logic: 原始栏只展示 Agent 真正加载的约定文件，不展示 Hub 投影/独有槽。
 * Code Logic: override、cc-partner.exclusive/adapted、Cursor rules 都排除。
 */
export function isHubProjectionInstructionPath(path: string): boolean {
  const normalized = path.replace(/\\/gu, '/').toLowerCase();
  const name = fileName(path).toLowerCase();
  if (name === 'agents.override.md') return true;
  if (name.includes('cc-partner.exclusive') || name.includes('cc-partner.adapted')) return true;
  if (normalized.includes('/rules/') && (name.endsWith('.mdc') || name.includes('cc-partner'))) {
    return true;
  }
  return false;
}

/**
 * Business Logic: 用户级约定文件名是 AGENTS.md / CLAUDE.md / GEMINI.md。
 * Code Logic: 只看 basename，忽略目录。
 */
export function isNativeInstructionBasename(path: string): boolean {
  return kindFromBasename(fileName(path)) !== null;
}

/** 三槽原始栏选用的原生文件。 */
export interface NativeOriginalForAgent {
  path: string | null;
  source: UserInstructionSourceDto | null;
}

/**
 * Business Logic: 当前 Agent 的原始栏对准它配置目录里会加载的那份文件。
 * Code Logic: 先 target.sources 里的 native 约定文件；OpenCode 缺 AGENTS.md 时回退 Claude CLAUDE.md；
 *   再 managedTargetPath / 配置根声明路径。永不返回 Hub exclusive/override。
 */
export function nativeOriginalForAgent(
  workspace: UserInstructionWorkspaceDto,
  agent: AgentTarget,
): NativeOriginalForAgent {
  const target = workspace.targets.find((item) => item.target === agent) ?? null;
  if (!target) return { path: null, source: null };
  const kinds = USER_NATIVE_KINDS[agent] ?? [];
  const nativeSources = target.sources.filter(
    (source) =>
      isNativeInstructionBasename(source.path) && !isHubProjectionInstructionPath(source.path),
  );

  for (const kind of kinds) {
    const basename = basenameForKind(kind);
    const match = nativeSources.find(
      (source) => fileName(source.path).toLowerCase() === basename.toLowerCase(),
    );
    if (match) return { path: match.path, source: match };
  }

  if (agent === 'opencode') {
    const agentsExists = nativeSources.some((source) => {
      return fileName(source.path).toLowerCase() === 'agents.md' && source.exists;
    });
    if (!agentsExists) {
      const fallback =
        target.sources.find(
          (source) =>
            source.role === 'fallback' &&
            isNativeInstructionBasename(source.path) &&
            !isHubProjectionInstructionPath(source.path),
        ) ??
        workspace.targets
          .find((item) => item.target === 'claude')
          ?.sources.find((source) => kindFromBasename(fileName(source.path)) === 'claude') ??
        null;
      if (fallback) return { path: fallback.path, source: fallback };
    }
  }

  if (
    target.managedTargetPath &&
    isNativeInstructionBasename(target.managedTargetPath) &&
    !isHubProjectionInstructionPath(target.managedTargetPath)
  ) {
    return {
      path: target.managedTargetPath,
      source: matchUserNativeSource(
        target.sources,
        target.managedTargetPath,
        fileName(target.managedTargetPath),
      ),
    };
  }

  for (const kind of kinds) {
    if (!target.cli.configRoot.trim()) continue;
    const basename = basenameForKind(kind);
    const declared = joinHomeFile(target.cli.configRoot, basename);
    return {
      path: declared,
      source: matchUserNativeSource(target.sources, declared, basename),
    };
  }

  return { path: null, source: null };
}

/**
 * Business Logic: inspect 可能返回 Claude.md；要当成 CLAUDE.md。
 * Code Logic: 路径相等或同目录且文件名忽略大小写。
 */
export function matchUserNativeSource(
  sources: readonly UserInstructionSourceDto[],
  declaredPath: string,
  basename: string,
): UserInstructionSourceDto | null {
  const declaredId = canonicalUserFileId(declaredPath);
  const exact = sources.find((source) => canonicalUserFileId(source.path) === declaredId);
  if (exact) return exact;
  const declaredParent = parentDir(declaredPath).toLowerCase();
  return (
    sources.find((source) => {
      return (
        parentDir(source.path).toLowerCase() === declaredParent &&
        fileName(source.path).toLowerCase() === basename.toLowerCase()
      );
    }) ?? null
  );
}

/** inspect 源与规格的配对，供控制器 hydrate。 */
export interface UserNativeFileFromWorkspace {
  spec: UserNativeFileSpec;
  source: UserInstructionSourceDto | null;
}

/**
 * Business Logic: 把 workspace 收成「路径 → 共用 Agent」表。
 * Code Logic: 先按各 Agent 配置根声明文件；OpenCode 缺 AGENTS.md 时并入 Claude CLAUDE.md。
 */
export function filesFromUserWorkspace(
  workspace: UserInstructionWorkspaceDto | null,
): UserNativeFileFromWorkspace[] {
  if (!workspace) return [];
  const byId = new Map<string, UserNativeFileFromWorkspace>();

  function add(
    agent: AgentTarget,
    path: string,
    kind: UserNativeFileKind,
    source: UserInstructionSourceDto | null,
  ): void {
    const id = canonicalUserFileId(path);
    const existing = byId.get(id);
    if (existing) {
      if (!existing.spec.consumers.includes(agent)) {
        existing.spec = {
          ...existing.spec,
          consumers: [...existing.spec.consumers, agent],
        };
      }
      if (!existing.source && source) existing.source = source;
      return;
    }
    byId.set(id, {
      spec: { id, kind, path, consumers: [agent] },
      source,
    });
  }

  for (const target of workspace.targets) {
    const kinds = USER_NATIVE_KINDS[target.target] ?? [];
    for (const kind of kinds) {
      const basename = basenameForKind(kind);
      const declared = joinHomeFile(target.cli.configRoot, basename);
      const source = matchUserNativeSource(target.sources, declared, basename);
      add(target.target, source?.path ?? declared, kind, source);
    }
    if (target.target !== 'opencode') continue;
    const agentsKind = kinds.includes('agents');
    if (!agentsKind) continue;
    const agentsDeclared = joinHomeFile(target.cli.configRoot, 'AGENTS.md');
    const agentsSource = matchUserNativeSource(target.sources, agentsDeclared, 'AGENTS.md');
    const agentsExists = Boolean(agentsSource?.exists);
    if (agentsExists) continue;
    const fallback = target.sources.find((source) => {
      return (
        source.role === 'fallback' &&
        kindFromBasename(fileName(source.path)) === 'claude'
      );
    });
    if (fallback) add('opencode', fallback.path, 'claude', fallback);
  }

  const order = allHubTargets();
  return [...byId.values()].sort((left, right) => {
    const leftAgent = order.indexOf(left.spec.consumers[0] ?? 'claude');
    const rightAgent = order.indexOf(right.spec.consumers[0] ?? 'claude');
    if (leftAgent !== rightAgent) return leftAgent - rightAgent;
    return left.spec.path.localeCompare(right.spec.path);
  });
}

/**
 * Business Logic: 当前 Agent 只展示它会加载的用户级文件。
 * Code Logic: consumers 包含该 Agent。
 */
export function userFilesForAgent(
  files: readonly UserNativeFileSpec[],
  agent: AgentTarget,
): UserNativeFileSpec[] {
  return files.filter((file) => file.consumers.includes(agent));
}

/**
 * Business Logic: Codex → OpenCode 若仍是同一绝对路径则留在该文件上。
 * Code Logic: 当前 id 仍可见则保留，否则落到该 Agent 第一份文件。
 */
export function resolveActiveUserFileId(
  files: readonly UserNativeFileSpec[],
  agent: AgentTarget,
  currentId: string | null,
): string | null {
  const visible = userFilesForAgent(files, agent);
  if (currentId && visible.some((file) => file.id === currentId)) return currentId;
  return visible[0]?.id ?? null;
}

/** 脏稿守卫入参。 */
export interface UserNativeInstructionGuardInput {
  dirtyFileIds: readonly string[];
  currentDeviceId: string | null;
  nextTab: AgentHubTab;
  nextAgent: AgentTarget;
  nextScope: AgentHubScope;
  nextDeviceId: string | null;
  visibleFileIdsForAgent: (agent: AgentTarget) => readonly string[];
}

/**
 * Business Logic: 路径相同才不算换文件；Codex 与 OpenCode 的 AGENTS.md 通常不同。
 * Code Logic: 无脏稿放行；换设备/离提示词 tab 拦截；下一 Agent 仍能看到全部脏文件则放行。
 */
export function shouldGuardUserNativeInstructionContextChange(
  input: UserNativeInstructionGuardInput,
): boolean {
  if (input.dirtyFileIds.length === 0) return false;
  if (input.nextScope !== 'user' || input.nextDeviceId !== input.currentDeviceId) {
    return true;
  }
  if (input.nextTab !== 'instructions') return true;
  const nextIds = new Set(input.visibleFileIdsForAgent(input.nextAgent));
  return input.dirtyFileIds.some((id) => !nextIds.has(id));
}
