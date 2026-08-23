import { describe, test } from 'vitest';
import type {
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
} from '../../lib/types';
import {
  canOpenHostProjectSelection,
  canOpenRemoteProjectSelection,
  isValidBrowseChildName,
  peerSupportsBrowseMkdir,
  WORKBENCH_FS_CREATE_DIR_CAPABILITY,
  isRemoteWorkbenchOfflineError,
  isRemoteWorkbenchProjectOffline,
  remoteParentPath,
  sortRemoteDirectoryEntries,
  upsertWorkbenchProjectInPlace,
  moveProjectId,
  orderProjectsByIds,
} from '../../lib/workbenchRemoteProjects';

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
 *   首次打开远端项目应出现在侧栏顶部；但重新打开已在列表中的项目必须保持原位置，
 *   否则用户每次点击项目都要重新寻找相邻项目（选中即置顶的空间记忆问题）。
 *
 * Code Logic（这个测试做什么）:
 *   构造一个本地项目和一个远端项目，断言首次插入置顶，再次 upsert 时就地更新且不改变索引。
 */
function testUpsertRemoteProjectKeepsPositionWithoutDuplicates(): void {
  const remoteProject: WorkbenchProject = {
    ...baseProject,
    id: 'remote:device-a:abc',
    name: 'remote-app',
    kind: 'remote',
    deviceId: 'device-a',
    deviceName: 'Studio Mac',
    path: '/Users/hans/app',
  };

  const firstInsert = upsertWorkbenchProjectInPlace([baseProject], remoteProject);
  assert(firstInsert[0]?.id === remoteProject.id, 'new remote project should be inserted at top');
  assert(firstInsert.length === 2, 'remote project should be added to list');

  // 再插入一个新项目，把 remoteProject 挤到索引 1。
  const withNewer = upsertWorkbenchProjectInPlace(firstInsert, {
    ...baseProject,
    id: 'local-2',
    name: 'newest',
    path: '/Users/hans/newest',
  });
  assert(withNewer[1]?.id === remoteProject.id, 'existing remote project should shift to index 1');

  const secondUpsert = upsertWorkbenchProjectInPlace(withNewer, {
    ...remoteProject,
    name: 'remote-app-updated',
  });
  assert(
    secondUpsert[1]?.id === remoteProject.id,
    'reopening an existing project must not move it to the top',
  );
  assert(
    secondUpsert[1]?.name === 'remote-app-updated',
    'upsert should keep latest project payload',
  );
  assert(
    secondUpsert.filter((project) => project.id === remoteProject.id).length === 1,
    'duplicate remote project should be de-duplicated by id',
  );
  assert(secondUpsert.length === 3, 'upsert must not change list length for existing project');
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
 * Business Logic（为什么需要这个测试）:
 *   手机添加本机项目没有 deviceId，打开门闩必须只校验当前可读目录。
 *
 * Code Logic（这个测试做什么）:
 *   对 canOpenHostProjectSelection 覆盖可读目录、stale path、文件、不可读、loading、openBusy。
 */
function testCanOpenHostProjectSelectionRequiresCurrentReadableDirectory(): void {
  const info: WorkbenchRemotePathInfo = {
    name: 'app',
    path: '/Users/hans/app',
    kind: 'dir',
    readable: true,
    isGitRepo: true,
    suggestedProjectName: 'app',
  };

  assert(
    canOpenHostProjectSelection('/Users/hans/app', info, '/Users/hans/app', false, false),
    'current readable host directory should be openable',
  );
  assert(
    !canOpenHostProjectSelection('/Users/hans/other', info, '/Users/hans/app', false, false),
    'stale host path info should block open',
  );
  assert(
    !canOpenHostProjectSelection('/Users/hans/app', { ...info, kind: 'file' }, '/Users/hans/app', false, false),
    'host file path should block open',
  );
  assert(
    !canOpenHostProjectSelection('/Users/hans/app', { ...info, readable: false }, '/Users/hans/app', false, false),
    'unreadable host directory should block open',
  );
  assert(
    !canOpenHostProjectSelection('/Users/hans/app', info, '/Users/hans/app', true, false),
    'pending host path info should block open',
  );
  assert(
    !canOpenHostProjectSelection('/Users/hans/app', info, '/Users/hans/app', false, true),
    'in-flight host open should block open',
  );
}

/**
 * Business Logic（为什么需要这个测试）:
 *   远端设备离线后，Workbench 只应禁用当前离线远端项目的写操作，不应影响本机项目或其他远端项目。
 *
 * Code Logic（这个测试做什么）:
 *   校验离线错误文本识别，以及 project/offlineProjectId 匹配逻辑。
 */
function testRemoteOfflineStateOnlyMatchesCurrentRemoteProject(): void {
  const remoteProject: WorkbenchProject = {
    ...baseProject,
    id: 'remote:device-a:abc',
    kind: 'remote',
    deviceId: 'device-a',
    deviceName: 'Studio Mac',
    path: '/Users/hans/app',
  };
  const otherRemoteProject: WorkbenchProject = {
    ...remoteProject,
    id: 'remote:device-b:def',
    deviceId: 'device-b',
  };

  assert(isRemoteWorkbenchOfflineError(new Error('远端设备不在线')), 'offline backend error should be detected');
  assert(
    isRemoteWorkbenchOfflineError('读取终端失败: 远端设备不在线'),
    'composed UI error should still be detected',
  );
  assert(
    isRemoteWorkbenchOfflineError(Object.assign(new Error('network offline'), { code: 'NETWORK_OFFLINE' })),
    'typed NETWORK_OFFLINE code should mark offline',
  );
  assert(
    isRemoteWorkbenchOfflineError(new Error('Failed to fetch')),
    'networkOffline classification from message should mark offline',
  );
  assert(
    !isRemoteWorkbenchOfflineError(new Error('读取终端失败')),
    'unrelated errors should not mark the project offline',
  );
  assert(
    isRemoteWorkbenchProjectOffline(remoteProject, 'remote:device-a:abc'),
    'matching remote project should be offline',
  );
  assert(
    !isRemoteWorkbenchProjectOffline(baseProject, 'remote:device-a:abc'),
    'local project should not be treated as remote offline',
  );
  assert(
    !isRemoteWorkbenchProjectOffline(otherRemoteProject, 'remote:device-a:abc'),
    'other remote project should remain enabled',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目选择器 helper 覆盖多个独立 UI 契约，需要一个顺序执行入口便于 npm/tsx 调用。
 *
 * Code Logic（这个函数做什么）:
 *   逐个执行纯 helper 测试，任一失败会抛出并让进程返回非零状态。
 */

/**
 * Business Logic（为什么需要这个测试）:
 *   拖拽排序依赖 source→target before/after 重排，错误插入会破坏用户顺序。
 */
function testMoveProjectIdBeforeAndAfter(): void {
  assert(
    moveProjectId(['a', 'b', 'c'], 'a', 'c', 'before').join(',') === 'b,a,c',
    'move before target should insert immediately above target',
  );
  assert(
    moveProjectId(['a', 'b', 'c'], 'a', 'c', 'after').join(',') === 'b,c,a',
    'move after target should insert immediately below target',
  );
  assert(
    moveProjectId(['a', 'b', 'c'], 'a', 'a', 'after').join(',') === 'a,b,c',
    'same source/target should keep order',
  );
}

/**
 * Business Logic（为什么需要这个测试）:
 *   乐观更新需要按 id 列表投影项目对象且不丢项。
 */
function testOrderProjectsByIds(): void {
  const projects = [
    { ...baseProject, id: 'a', name: 'A' },
    { ...baseProject, id: 'b', name: 'B' },
    { ...baseProject, id: 'c', name: 'C' },
  ];
  const ordered = orderProjectsByIds(projects, ['c', 'a']);
  assert(ordered.map((p) => p.id).join(',') === 'c,a,b', 'ordered ids first then remainder');
}

/**
 * Business Logic（为什么需要这个测试）:
 *   浏览层 mkdir 前端预检必须与后端单段名称规则对齐。
 */
function testIsValidBrowseChildName(): void {
  assert(isValidBrowseChildName('new-studio'), 'plain name should pass');
  assert(!isValidBrowseChildName(''), 'empty should fail');
  assert(!isValidBrowseChildName('.'), 'dot should fail');
  assert(!isValidBrowseChildName('..'), 'dotdot should fail');
  assert(!isValidBrowseChildName('a/b'), 'slash should fail');
  assert(!isValidBrowseChildName('a\\b'), 'backslash should fail');
}

/**
 * Business Logic（为什么需要这个测试）:
 *   旧对端缺 token 时不得展示新建文件夹。
 */
function testPeerSupportsBrowseMkdir(): void {
  assert(!peerSupportsBrowseMkdir(undefined), 'missing list is unsupported');
  assert(!peerSupportsBrowseMkdir([]), 'empty list is unsupported');
  assert(
    peerSupportsBrowseMkdir([WORKBENCH_FS_CREATE_DIR_CAPABILITY]),
    'exact token should enable mkdir',
  );
}


describe('workbenchRemoteProjects', () => {
  test('upsert, parent path, sort, open gate and offline detection helpers', async () => {
    testUpsertRemoteProjectKeepsPositionWithoutDuplicates();
    testRemoteParentPathHandlesUnixAndWindowsPaths();
    testSortRemoteDirectoryEntriesPutsDirsBeforeFiles();
    testCanOpenRemoteProjectSelectionRequiresCurrentReadableDirectory();
    testCanOpenHostProjectSelectionRequiresCurrentReadableDirectory();
    testRemoteOfflineStateOnlyMatchesCurrentRemoteProject();
    testMoveProjectIdBeforeAndAfter();
    testOrderProjectsByIds();
    testIsValidBrowseChildName();
    testPeerSupportsBrowseMkdir();
  });
});
