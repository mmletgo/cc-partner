/**
 * 工作台 API - 通过 Tauri invoke 调用 Rust 后端的本机项目、终端和文件树命令。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台页面需要统一管理项目文件夹、terminal window/pane 和右侧文件树交互。
 *   组件层不应直接拼 invoke 命令名，避免命令参数分散。
 *
 * Code Logic（这个模块做什么）:
 *   按 projects / sessions / files 三个业务分组封装 Rust workbench 命令；
 *   所有参数使用 camelCase，返回类型对齐 `src/lib/types.ts`。
 */

import { invoke } from './client';
import type {
  WorkbenchFileNode,
  WorkbenchGitCommit,
  WorkbenchMergeResult,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSession,
  WorkbenchWorktree,
} from '@/lib/types';

interface WorkbenchTerminalSize {
  cols: number;
  rows: number;
}

export type WorkbenchPaneSplitDirection = 'right' | 'down';

export const workbenchApi = {
  projects: {
    /** 列出工作台最近项目，后端按 lastOpenedAt 倒序返回。 */
    list: () => invoke<WorkbenchProject[]>('list_workbench_projects'),

    /** 添加或重新打开一个项目文件夹，path 为本机或已挂载局域网目录。 */
    add: (path: string) =>
      invoke<WorkbenchProject>('add_workbench_project', { path }),

    /** 从最近项目列表移除项目记录，不删除磁盘文件。 */
    remove: (projectId: string) =>
      invoke<{ ok: boolean; projectId: string }>('remove_workbench_project', { projectId }),

    /** 更新最近打开时间，并返回最新项目 DTO。 */
    touch: (projectId: string) =>
      invoke<WorkbenchProject>('touch_workbench_project', { projectId }),
  },

  worktrees: {
    /** 列出项目下主工作区和功能 worktree。 */
    list: (projectId: string) =>
      invoke<WorkbenchWorktree[]>('list_workbench_worktrees', { projectId }),

    /** 从项目创建一个新的 Git worktree 和分支。 */
    create: (projectId: string, branchName: string, baseBranch?: string | null) =>
      invoke<WorkbenchWorktree>('create_workbench_worktree', {
        projectId,
        branchName,
        baseBranch: baseBranch ?? null,
      }),

    /** 提交当前 worktree 的全部本地改动；message 为空时由后端 Claude Code 生成。 */
    commit: (worktreeId: string, message?: string | null) =>
      invoke<WorkbenchWorktree>('commit_workbench_worktree', {
        worktreeId,
        message: message ?? null,
      }),

    /** 推送当前 worktree 分支到已有 upstream；没有 upstream 时只默认推送到 origin。 */
    push: (worktreeId: string) =>
      invoke<WorkbenchWorktree>('push_workbench_worktree', { worktreeId }),

    /** 合并当前 worktree 分支到主工作区。 */
    merge: (worktreeId: string) =>
      invoke<WorkbenchMergeResult>('merge_workbench_worktree', { worktreeId }),

    /** 删除非主 worktree；force 用于强制移除 Git worktree。 */
    remove: (worktreeId: string, force = false) =>
      invoke<{ ok: boolean; worktreeId: string }>('remove_workbench_worktree', {
        worktreeId,
        force,
      }),
  },

  git: {
    /** 列出当前 worktree 最近 Git 提交历史。 */
    listCommits: (projectId: string, worktreeId?: string | null, limit = 30) =>
      invoke<WorkbenchGitCommit[]>('list_workbench_git_commits', {
        projectId,
        worktreeId: worktreeId ?? null,
        limit,
      }),
  },

  sessions: {
    /** 列出 terminal window；projectId 为空则返回全部。 */
    list: (projectId?: string) =>
      invoke<WorkbenchSession[]>('list_workbench_sessions', {
        projectId: projectId ?? null,
      }),

    /** 在指定项目下创建一个 terminal window。 */
    create: (projectId: string, initialSize?: WorkbenchTerminalSize, worktreeId?: string | null) =>
      invoke<WorkbenchSession>('create_workbench_session', {
        projectId,
        worktreeId: worktreeId ?? null,
        initialCols: initialSize?.cols ?? null,
        initialRows: initialSize?.rows ?? null,
      }),

    /** 向指定 terminal window 的 PTY attach 写入输入数据。 */
    writeInput: (sessionId: string, data: string) =>
      invoke<{ ok: boolean; sessionId: string }>('write_workbench_session_input', {
        sessionId,
        data,
      }),

    /** 调整终端 PTY 行列数。 */
    resize: (sessionId: string, cols: number, rows: number) =>
      invoke<{ ok: boolean; sessionId: string }>('resize_workbench_session', {
        sessionId,
        cols,
        rows,
      }),

    /** 聚焦 terminal window，并同步切换底层 tmux current window。 */
    focus: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('focus_workbench_session', {
        sessionId,
      }),

    /** 读取当前 worktree tmux current window 对应的 terminal session。 */
    focused: (projectId: string, worktreeId?: string | null) =>
      invoke<{ sessionId: string | null }>('get_focused_workbench_session', {
        projectId,
        worktreeId: worktreeId ?? null,
      }),

    /** 在当前 tmux window 内创建一个 pane。 */
    splitPane: (sessionId: string, direction: WorkbenchPaneSplitDirection) =>
      invoke<{ ok: boolean; sessionId: string; direction: WorkbenchPaneSplitDirection }>(
        'split_workbench_pane',
        {
          sessionId,
          direction,
        },
      ),

    /** 关闭当前 tmux pane；最后一个 pane 会关闭所属 terminal window。 */
    closePane: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string; closedWindow: boolean }>('close_workbench_pane', {
        sessionId,
      }),

    /** 关闭终端 tab，并释放后端 PTY 资源。 */
    close: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('close_workbench_session', {
        sessionId,
      }),

    /** 重命名 terminal window。 */
    rename: (sessionId: string, name: string) =>
      invoke<WorkbenchSession>('rename_workbench_session', { sessionId, name }),
  },

  files: {
    /** 列出项目内目录的一级子节点；path 为空表示项目根。 */
    listDir: (projectId: string, path?: string, worktreeId?: string | null) =>
      invoke<WorkbenchFileNode[]>('list_workbench_dir', {
        projectId,
        worktreeId: worktreeId ?? null,
        path: path ?? null,
      }),

    /** 获取项目内路径信息。 */
    info: (projectId: string, path: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('get_workbench_path_info', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }),

    /** 在父目录下创建空文件。 */
    createFile: (projectId: string, parentPath: string, name: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('create_workbench_file', {
        projectId,
        worktreeId: worktreeId ?? null,
        parentPath,
        name,
      }),

    /** 在父目录下创建文件夹。 */
    createDir: (projectId: string, parentPath: string, name: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('create_workbench_dir', {
        projectId,
        worktreeId: worktreeId ?? null,
        parentPath,
        name,
      }),

    /** 重命名项目内文件或文件夹。 */
    renamePath: (projectId: string, path: string, newName: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('rename_workbench_path', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
        newName,
      }),

    /** 删除项目内文件或文件夹。 */
    deletePath: (projectId: string, path: string, worktreeId?: string | null) =>
      invoke<{ ok: boolean; path: string }>('delete_workbench_path', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }),
  },
};
