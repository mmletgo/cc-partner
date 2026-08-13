/**
 * Workbench workspace layout draft builder + autosave coordinator。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端零配置保存最后工作现场；仅稳定 selection 触发，500ms debounce 合并；
 *   不保存 terminal 输出、文件正文、env、token、命令或 provider 配置。
 *
 * Code Logic（这个模块做什么）:
 *   `buildWorkspaceLayoutDraft` 只接收稳定 ID；`WorkspaceLayoutAutosaveCoordinator`
 *   持有 revision 与单一定时器，CAS conflict 后 reread+recompute。
 */

/** auto slot 固定键。 */
export const DESKTOP_AUTO_SLOT_KEY = 'desktop:auto' as const;

/** layout schema 版本。 */
export const WORKSPACE_LAYOUT_SCHEMA_VERSION = 1 as const;

/** 主工作区视图。 */
export type WorkspaceView = 'terminal' | 'files' | 'browser' | 'automation';

/** inspector tab。 */
export type InspectorTab = 'files' | 'git' | 'history' | 'notes' | 'automation';

/** layout 种类。 */
export type WorkspaceLayoutKind = 'auto' | 'named';

/**
 * 写入 layout 的草稿（无 id/revision）。
 */
export interface WorkspaceLayoutDraft {
  slotKey: string;
  kind: WorkspaceLayoutKind;
  name: string | null;
  projectId: string;
  activeWorktreeId: string | null;
  activeSessionId: string | null;
  workspaceView: WorkspaceView;
  inspectorTab: InspectorTab;
  browserTargetUrl: string | null;
}

/**
 * 持久化 layout 行。
 */
export interface WorkspaceLayout extends WorkspaceLayoutDraft {
  schemaVersion: number;
  id: string;
  revision: number;
  createdAt: string;
  updatedAt: string;
}

/**
 * draft 构造入参：仅稳定 selection。
 */
export interface WorkspaceLayoutSelection {
  projectId: string | null;
  activeWorktreeId: string | null;
  activeSessionId: string | null;
  workspaceView: WorkspaceView;
  inspectorTab: InspectorTab;
  browserTargetUrl: string | null;
  /** named snapshot 时提供 */
  named?: { slotKey: string; name: string };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   autosave 与命名 snapshot 只允许结构引用进入 draft。
 *
 * Code Logic（这个函数做什么）:
 *   无 project 返回 null；否则构造 auto 或 named draft，去掉空字符串。
 */
export function buildWorkspaceLayoutDraft(
  selection: WorkspaceLayoutSelection,
): WorkspaceLayoutDraft | null {
  const projectId = selection.projectId?.trim() ?? '';
  if (!projectId) {
    return null;
  }
  const named = selection.named;
  return {
    slotKey: named?.slotKey ?? DESKTOP_AUTO_SLOT_KEY,
    kind: named ? 'named' : 'auto',
    name: named ? named.name.trim() : null,
    projectId,
    activeWorktreeId: emptyToNull(selection.activeWorktreeId),
    activeSessionId: emptyToNull(selection.activeSessionId),
    workspaceView: selection.workspaceView,
    inspectorTab: selection.inspectorTab,
    browserTargetUrl: emptyToNull(selection.browserTargetUrl),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   空字符串不得当作有效 ID 持久化。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后空则 null。
 */
function emptyToNull(value: string | null | undefined): string | null {
  if (value == null) return null;
  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
}

/** save API 形态。 */
export type SaveWorkspaceLayoutFn = (
  draft: WorkspaceLayoutDraft,
  expectedRevision: number | null,
) => Promise<WorkspaceLayout>;

/** get API 形态。 */
export type GetWorkspaceLayoutFn = (slotKey: string) => Promise<WorkspaceLayout | null>;

/**
 * Autosave 依赖的选择器。
 */
export type WorkspaceLayoutSelector = () => WorkspaceLayoutSelection;

/**
 * Autosave coordinator 选项。
 */
export interface WorkspaceLayoutAutosaveOptions {
  save: SaveWorkspaceLayoutFn;
  get: GetWorkspaceLayoutFn;
  select: WorkspaceLayoutSelector;
  debounceMs?: number;
  now?: () => number;
  schedule?: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearSchedule?: (id: ReturnType<typeof setTimeout>) => void;
}

/**
 * 稳定 selection 变化的 500ms debounce autosave。
 *
 * Business Logic（为什么需要这个类）:
 *   连续切换 project/worktree/session 时合并写库；conflict 后从当前 UI 重算。
 *
 * Code Logic（这个类做什么）:
 *   单 timer；revision 本地缓存；outcome unknown 时 get/revision 对账。
 */
export class WorkspaceLayoutAutosaveCoordinator {
  private revision: number | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly debounceMs: number;
  private readonly save: SaveWorkspaceLayoutFn;
  private readonly get: GetWorkspaceLayoutFn;
  private readonly select: WorkspaceLayoutSelector;
  private readonly schedule: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  private readonly clearSchedule: (id: ReturnType<typeof setTimeout>) => void;
  private readonly savedDrafts: WorkspaceLayoutDraft[] = [];
  private saving = false;
  /** restore / named snapshot apply 期间禁止写 layout，避免冲掉 preflight revision。 */
  private paused = false;

  /**
   * Business Logic（为什么需要这个函数）:
   *   页面注入 transport 与 selector。
   *
   * Code Logic（这个函数做什么）:
   *   保存依赖，默认 500ms debounce。
   */
  constructor(options: WorkspaceLayoutAutosaveOptions) {
    this.save = options.save;
    this.get = options.get;
    this.select = options.select;
    this.debounceMs = options.debounceMs ?? 500;
    this.schedule = options.schedule ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearSchedule = options.clearSchedule ?? ((id) => clearTimeout(id));
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   测试断言已保存 draft。
   *
   * Code Logic（这个函数做什么）:
   *   返回已成功 save 的 draft 列表。
   */
  saved(): WorkspaceLayoutDraft[] {
    return [...this.savedDrafts];
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   启动时同步远端 revision，避免盲 create。
   *
   * Code Logic（这个函数做什么）:
   *   get desktop:auto 并缓存 revision。
   */
  async hydrateRevision(slotKey: string = DESKTOP_AUTO_SLOT_KEY): Promise<void> {
    const layout = await this.get(slotKey);
    this.revision = layout?.revision ?? null;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   selection 稳定变化时调度保存；内容类事件不得调用。
   *
   * Code Logic（这个函数做什么）:
   *   重置 500ms timer；无 project 时不写不删。
   */
  /**
   * Business Logic（为什么需要这个函数）:
   *   启动 restore / 命名 snapshot apply 不得被 500ms autosave 抢写 revision。
   *
   * Code Logic（这个函数做什么）:
   *   置 paused 并取消未触发的 debounce。
   */
  pause(): void {
    this.paused = true;
    this.clearPendingTimer();
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   restore 结束后恢复正常 autosave。
   *
   * Code Logic（这个函数做什么）:
   *   清 paused；不自动 flush，由调用方 notify。
   */
  resume(): void {
    this.paused = false;
  }

  notifySelectionChanged(): void {
    this.clearPendingTimer();
    if (this.paused) return;
    this.timer = this.schedule(() => {
      this.timer = null;
      void this.flush();
    }, this.debounceMs);
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   terminal output / pane resize / agent phase 不得触发保存。
   *
   * Code Logic（这个函数做什么）:
   *   空实现；调用方应显式忽略这些事件。
   */
  notifyContentNoise(): void {
    // intentionally no-op
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   debounce 到期或页面卸载时立即保存。
   *
   * Code Logic（这个函数做什么）:
   *   build draft → save_cas；conflict 则 reread 并从当前 select 重算。
   */
  async flush(): Promise<void> {
    if (this.paused || this.saving) return;
    const draft = buildWorkspaceLayoutDraft(this.select());
    if (!draft) {
      // 无 project：不写空 layout，不删除旧 layout
      return;
    }
    this.saving = true;
    try {
      await this.saveWithCas(draft);
    } finally {
      this.saving = false;
    }
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   CAS 冲突与 outcome unknown 的对账。
   *
   * Code Logic（这个函数做什么）:
   *   try save；conflict → get + recompute from select；unknown → get revision。
   */
  private async saveWithCas(draft: WorkspaceLayoutDraft): Promise<void> {
    try {
      const saved = await this.save(draft, this.revision);
      this.revision = saved.revision;
      this.savedDrafts.push({ ...draft });
    } catch (error) {
      const code = extractErrorCode(error);
      if (code === 'workspace_layout_conflict') {
        const latest = await this.get(draft.slotKey);
        this.revision = latest?.revision ?? null;
        const recomputed = buildWorkspaceLayoutDraft(this.select());
        if (!recomputed) return;
        const saved = await this.save(recomputed, this.revision);
        this.revision = saved.revision;
        this.savedDrafts.push({ ...recomputed });
        return;
      }
      if (code === 'unknown' || code === 'timeout' || code === 'network') {
        const latest = await this.get(draft.slotKey);
        this.revision = latest?.revision ?? null;
        return;
      }
      throw error;
    }
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   卸载时清理 timer。
   *
   * Code Logic（这个函数做什么）:
   *   clearTimeout。
   */
  dispose(): void {
    this.clearPendingTimer();
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   pause / notify / dispose 共用取消 debounce。
   *
   * Code Logic（这个函数做什么）:
   *   清 timer。
   */
  private clearPendingTimer(): void {
    if (this.timer == null) return;
    this.clearSchedule(this.timer);
    this.timer = null;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一解析 invoke/HTTP 错误 code。
 *
 * Code Logic（这个函数做什么）:
 *   读 error.code / error.message / string。
 */
function extractErrorCode(error: unknown): string {
  if (error && typeof error === 'object') {
    const record = error as { code?: unknown; message?: unknown };
    if (typeof record.code === 'string') return record.code;
    if (typeof record.message === 'string') {
      if (record.message.includes('workspace_layout_conflict')) {
        return 'workspace_layout_conflict';
      }
      return record.message;
    }
  }
  if (typeof error === 'string') return error;
  return 'unknown';
}
