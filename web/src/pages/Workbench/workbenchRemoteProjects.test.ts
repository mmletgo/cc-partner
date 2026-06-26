import type {
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
} from '../../lib/types';
import {
  canOpenRemoteProjectSelection,
  insertWorkbenchProjectAtTop,
  remoteParentPath,
  sortRemoteDirectoryEntries,
} from '../../lib/workbenchRemoteProjects';
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:process 类型缺失,运行时 tsx 正常
import { exit } from 'node:process';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 远端项目 helper 使用轻量脚本测试，需要在没有测试框架时也能快速定位失败原因。
 *
 * Code Logic（这个函数做什么）:
 *   接收断言条件和失败消息；条件为 false 时抛出 Error 让 tsx 进程失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const baseProject: WorkbenchProject = {
  id: 'local-1',
  name: 'local',
  kind: 'local',
  deviceId: 'self',
  deviceName: 'Mac',
  path: '/Users/hans/local',
  lastOpenedAt: '2026-06-26T00:00:00Z',
  createdAt: '2026-06-26T00:00:00Z',
  updatedAt: '2026-06-26T00:00:00Z',
};

/**
 * Business Logic（为什么需要这个测试）:
 *   用户打开远端项目后，侧栏项目列表应立即把它放到顶部；重复打开同一项目不能出现重复卡片。
 *
 * Code Logic（这个测试做什么）:
 *   构造一个本地项目和一个远端项目，断言 helper 插入后置顶，并在重复插入时按 id 去重。
 */
function testInsertRemoteProjectMovesToTopWithoutDuplicates(): void {
  const remoteProject: WorkbenchProject = {
    ...baseProject,
    id: 'remote:device-a:abc',
    name: 'remote-app',
    kind: 'remote',
    deviceId: 'device-a',
    deviceName: 'Studio Mac',
    path: '/Users/hans/app',
  };

  const firstInsert = insertWorkbenchProjectAtTop([baseProject], remoteProject);
  assert(firstInsert[0]?.id === remoteProject.id, 'remote project should be inserted at top');
  assert(firstInsert.length === 2, 'remote project should be added to list');

  const secondInsert = insertWorkbenchProjectAtTop(firstInsert, {
    ...remoteProject,
    name: 'remote-app-updated',
  });
  assert(secondInsert[0]?.name === 'remote-app-updated', 'duplicate insert should keep latest project payload');
  assert(
    secondInsert.filter((project) => project.id === remoteProject.id).length === 1,
    'duplicate remote project should be de-duplicated by id',
  );
}

/**
 * Business Logic（为什么需要这个测试）:
 *   远端选择器需要支持 Unix 根目录、普通 Unix 路径和 Windows 盘符路径的上级导航。
 *
 * Code Logic（这个测试做什么）:
 *   断言 parent path helper 对 `/`、`/Users/hans/app`、`C:\\Users\\hans\\app` 返回稳定结果。
 */
function testRemoteParentPathHandlesUnixAndWindowsPaths(): void {
  assert(remoteParentPath('/') === null, 'root path should not have parent');
  assert(remoteParentPath('/Users/hans/app') === '/Users/hans', 'unix nested path should return parent');
  assert(
    remoteParentPath('C:\\Users\\hans\\app') === 'C:\\Users\\hans',
    'windows nested path should return parent',
  );
}

/**
 * Business Logic（为什么需要这个测试）:
 *   远端目录浏览应先展示文件夹，便于用户继续向下选择项目目录，再展示普通文件作为上下文参考。
 *
 * Code Logic（这个测试做什么）:
 *   构造乱序目录项，断言排序结果为目录优先且同类按名称排序。
 */
function testSortRemoteDirectoryEntriesPutsDirsBeforeFiles(): void {
  const entries: WorkbenchRemoteDirectoryEntry[] = [
    { name: 'zeta.txt', path: '/repo/zeta.txt', kind: 'file', modifiedAt: null, isGitRepo: false },
    { name: 'app', path: '/repo/app', kind: 'dir', modifiedAt: null, isGitRepo: true },
    { name: 'README.md', path: '/repo/README.md', kind: 'file', modifiedAt: null, isGitRepo: false },
    { name: 'bin', path: '/repo/bin', kind: 'dir', modifiedAt: null, isGitRepo: false },
  ];

  const sorted = sortRemoteDirectoryEntries(entries);
  assert(
    sorted.map((entry) => entry.name).join(',') === 'app,bin,README.md,zeta.txt',
    'entries should sort directories before files and names ascending',
  );
  assert(entries[0]?.name === 'zeta.txt', 'sorting should not mutate the original entries');
}

/**
 * Business Logic（为什么需要这个测试）:
 *   远端项目打开按钮必须等待当前选中路径的信息加载完成，避免用户打开旧路径或不可读文件。
 *
 * Code Logic（这个测试做什么）:
 *   构造当前路径信息和 stale/文件/不可读/pending 状态，断言 helper 只允许当前可读目录打开。
 */
function testCanOpenRemoteProjectSelectionRequiresCurrentReadableDirectory(): void {
  const info: WorkbenchRemotePathInfo = {
    name: 'app',
    path: '/Users/hans/app',
    kind: 'dir',
    readable: true,
    isGitRepo: true,
    suggestedProjectName: 'app',
  };

  assert(
    canOpenRemoteProjectSelection('device-a', '/Users/hans/app', info, 'device-a', false, false),
    'current readable directory should be openable',
  );
  assert(
    !canOpenRemoteProjectSelection('device-a', '/Users/hans/other', info, 'device-a', false, false),
    'stale path info should block open',
  );
  assert(
    !canOpenRemoteProjectSelection('device-b', '/Users/hans/app', info, 'device-a', false, false),
    'stale device info should block open',
  );
  assert(
    !canOpenRemoteProjectSelection('device-a', '/Users/hans/app', { ...info, kind: 'file' }, 'device-a', false, false),
    'file path should block open',
  );
  assert(
    !canOpenRemoteProjectSelection('device-a', '/Users/hans/app', { ...info, readable: false }, 'device-a', false, false),
    'unreadable directory should block open',
  );
  assert(
    !canOpenRemoteProjectSelection('device-a', '/Users/hans/app', info, 'device-a', true, false),
    'pending path info request should block open',
  );
  assert(
    !canOpenRemoteProjectSelection('device-a', '/Users/hans/app', info, 'device-a', false, true),
    'in-flight open request should block open',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目选择器 helper 覆盖多个独立 UI 契约，需要一个顺序执行入口便于 npm/tsx 调用。
 *
 * Code Logic（这个函数做什么）:
 *   逐个执行纯 helper 测试，任一失败会抛出并让进程返回非零状态。
 */
async function main(): Promise<void> {
  testInsertRemoteProjectMovesToTopWithoutDuplicates();
  testRemoteParentPathHandlesUnixAndWindowsPaths();
  testSortRemoteDirectoryEntriesPutsDirsBeforeFiles();
  testCanOpenRemoteProjectSelectionRequiresCurrentReadableDirectory();
}

void main()
  .then(() => {
    exit(0);
  })
  .catch((error: unknown) => {
    console.error(error);
    exit(1);
  });
