import { describe, test } from 'vitest';
import type { WorkbenchDependencyStatus } from './types';
import {
  canInstallWorkbenchDependency,
  canRecheckWorkbenchDependency,
  dependencyStatusFromError,
  dependencyStatusTone,
  formatInstallCommandPreview,
  shouldShowWorkbenchDependencyNotice,
} from './workbenchDependency';

/**
 * Business Logic（为什么需要这个函数）:
 *   依赖状态测试需要快速构造后端返回的 tmux dependency DTO，避免依赖真实系统环境。
 *
 * Code Logic（这个函数做什么）:
 *   合并默认 missing 状态和调用方 patch，返回完整 WorkbenchDependencyStatus。
 */
function dependency(
  patch: Partial<WorkbenchDependencyStatus>,
): WorkbenchDependencyStatus {
  return {
    status: 'missing',
    available: false,
    version: null,
    backend: 'native',
    path: null,
    installable: true,
    installCommandPreview: ['brew', 'install', 'tmux'],
    error: null,
    output: [],
    statusChangedAt: '2026-07-12T00:00:00.000Z',
    ...patch,
  };
}

describe('workbenchDependency', () => {
  test('maps tones and install/recheck action gates', () => {
    if (dependencyStatusTone(dependency({ status: 'ready', available: true })) !== 'success') {
      throw new Error('ready dependency should use success tone');
    }

    if (!canInstallWorkbenchDependency(dependency({ status: 'missing', installable: true }))) {
      throw new Error('missing installable dependency should allow install action');
    }

    if (canInstallWorkbenchDependency(dependency({ status: 'installing', installable: true }))) {
      throw new Error('installing dependency must not start another install');
    }

    if (!canRecheckWorkbenchDependency(dependency({ status: 'failed' }))) {
      throw new Error('failed dependency should allow recheck action');
    }

    if (formatInstallCommandPreview(['wsl.exe', '--exec', 'sh', '-lc', 'sudo apt-get install -y tmux']) !== 'wsl.exe --exec sh -lc "sudo apt-get install -y tmux"') {
      throw new Error('install preview should quote shell command arguments');
    }

    if (shouldShowWorkbenchDependencyNotice('ready') || shouldShowWorkbenchDependencyNotice('checking')) {
      throw new Error('ready/checking tmux must not occupy the terminal workbench');
    }
    if (!shouldShowWorkbenchDependencyNotice('missing') || !shouldShowWorkbenchDependencyNotice('failed')) {
      throw new Error('blocked tmux states must still show the workbench notice');
    }
    if (
      !shouldShowWorkbenchDependencyNotice('installing')
      || !shouldShowWorkbenchDependencyNotice('unsupported')
      || !shouldShowWorkbenchDependencyNotice('installedNeedsRecheck')
    ) {
      throw new Error('installing/unsupported/needs-recheck must still show the workbench notice');
    }

    const unsupported = dependencyStatusFromError(
      new Error('capability_unsupported:workbench.dependency-install.v1'),
    );
    if (unsupported.status !== 'unsupported' || unsupported.installable) {
      throw new Error('missing capability must map to unsupported and not installable');
    }
  });
});
