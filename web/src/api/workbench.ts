/**
 * 工作台 API - 通过 Tauri invoke 调用 Rust 后端的本机项目、终端和文件树命令。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台页面需要统一管理项目文件夹、多个 Claude Code 终端和右侧文件树交互。
 *   组件层不应直接拼 invoke 命令名，避免命令参数分散。
 *
 * Code Logic（这个模块做什么）:
 *   按 projects / sessions / files 三个业务分组封装 Rust workbench 命令；
 *   所有参数使用 camelCase，返回类型对齐 `src/lib/types.ts`。
 */

import { invoke } from './client';
import type {
  WorkbenchFileNode,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSession,
} from '@/lib/types';

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

  sessions: {
    /** 列出运行期终端会话；projectId 为空则返回全部。 */
    list: (projectId?: string) =>
      invoke<WorkbenchSession[]>('list_workbench_sessions', {
        projectId: projectId ?? null,
      }),

    /** 在指定项目根目录创建一个 Claude Code PTY 会话。 */
    create: (projectId: string) =>
      invoke<WorkbenchSession>('create_workbench_session', { projectId }),

    /** 向指定终端会话写入输入数据。 */
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

    /** 停止终端进程，但保留会话 tab。 */
    stop: (sessionId: string) =>
      invoke<WorkbenchSession>('stop_workbench_session', { sessionId }),

    /** 重启终端进程，并返回新建的继承命名会话。 */
    restart: (sessionId: string) =>
      invoke<WorkbenchSession>('restart_workbench_session', { sessionId }),

    /** 关闭终端 tab，并释放后端 PTY 资源。 */
    close: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('close_workbench_session', {
        sessionId,
      }),

    /** 重命名终端 tab。 */
    rename: (sessionId: string, name: string) =>
      invoke<WorkbenchSession>('rename_workbench_session', { sessionId, name }),
  },

  files: {
    /** 列出项目内目录的一级子节点；path 为空表示项目根。 */
    listDir: (projectId: string, path?: string) =>
      invoke<WorkbenchFileNode[]>('list_workbench_dir', {
        projectId,
        path: path ?? null,
      }),

    /** 获取项目内路径信息。 */
    info: (projectId: string, path: string) =>
      invoke<WorkbenchPathInfo>('get_workbench_path_info', { projectId, path }),

    /** 在父目录下创建空文件。 */
    createFile: (projectId: string, parentPath: string, name: string) =>
      invoke<WorkbenchPathInfo>('create_workbench_file', {
        projectId,
        parentPath,
        name,
      }),

    /** 在父目录下创建文件夹。 */
    createDir: (projectId: string, parentPath: string, name: string) =>
      invoke<WorkbenchPathInfo>('create_workbench_dir', {
        projectId,
        parentPath,
        name,
      }),

    /** 重命名项目内文件或文件夹。 */
    renamePath: (projectId: string, path: string, newName: string) =>
      invoke<WorkbenchPathInfo>('rename_workbench_path', {
        projectId,
        path,
        newName,
      }),

    /** 删除项目内文件或文件夹。 */
    deletePath: (projectId: string, path: string) =>
      invoke<{ ok: boolean; path: string }>('delete_workbench_path', {
        projectId,
        path,
      }),
  },
};
