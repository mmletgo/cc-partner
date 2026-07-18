// web/src/pages/Welcome/welcomePermissionFlow.ts
/**
 * Welcome 权限同步纯状态机（无 React / 无副作用）。
 *
 * Business Logic: 去设置不得 relaunch；回前台 recheck；仍未授权才 needs_reopen。
 * Code Logic: reduceWelcomePermPhase + hint key + sticky 判定。
 */

export type WelcomePermPhase = 'idle' | 'awaiting' | 'syncing' | 'needs_reopen';

export type StickyPermission = 'screenCapture' | 'accessibility' | 'inputMonitoring';

export const STICKY_PERMISSIONS: ReadonlySet<string> = new Set([
  'screenCapture',
  'accessibility',
  'inputMonitoring',
]);

/** 回前台 / 去设置后多轮 recheck 间隔（可测）。 */
export const SYNC_DELAYS_MS: readonly number[] = [0, 400, 1200, 2500];

/**
 * sticky 去设置结束后主动启动同步的延迟序列。
 * 首轮尽早进入 syncing；后续轮次覆盖用户在系统设置内拨开关的时序。
 * 与 SYNC_DELAYS 叠加后仍保证能到达 needs_reopen。
 */
export const POST_SETTINGS_SYNC_SCHEDULE_MS: readonly number[] = [500, 3000, 7000];

export type StickyStatusSlice = {
  screenCapture: { granted: boolean };
  accessibility: { granted: boolean };
  inputMonitoring: { granted: boolean };
};

export type WelcomePermEvent =
  | { type: 'GO_SETTINGS'; permission: string }
  | { type: 'FOREGROUND' }
  /** 用户点「重新检查」：即使当前 idle，只要组件确认 sticky 仍 denied 也可进入 syncing */
  | { type: 'USER_RECHECK' }
  | { type: 'SYNC_TICK'; status: StickyStatusSlice }
  | { type: 'SYNC_EXHAUSTED'; status: StickyStatusSlice }
  | { type: 'ALL_REQUIRED_GRANTED' }
  | { type: 'REOPEN_CLICKED' };

export function isStickyPermission(type: string): boolean {
  return STICKY_PERMISSIONS.has(type);
}

export function hasStickyDenied(status: StickyStatusSlice): boolean {
  return (
    !status.screenCapture.granted ||
    !status.accessibility.granted ||
    !status.inputMonitoring.granted
  );
}

/**
 * Business Logic: 权限同步相位转移，无 IO。
 * Code Logic:
 *   - GO_SETTINGS(sticky) → awaiting
 *   - FOREGROUND: awaiting → syncing；needs_reopen 保持（避免 recheck 时按钮消失）
 *   - USER_RECHECK: idle|awaiting → syncing；needs_reopen 保持
 *   - SYNC_TICK / SYNC_EXHAUSTED: sticky 全齐 → idle；否则 syncing 耗尽 → needs_reopen，
 *     已在 needs_reopen 则保持直到 sticky 全齐
 */
export function reduceWelcomePermPhase(
  phase: WelcomePermPhase,
  event: WelcomePermEvent,
): WelcomePermPhase {
  if (event.type === 'ALL_REQUIRED_GRANTED') {
    return 'idle';
  }
  switch (event.type) {
    case 'GO_SETTINGS':
      if (isStickyPermission(event.permission)) {
        // 已在 needs_reopen 时再次去设置：保持 needs_reopen（按钮不丢），否则 awaiting
        return phase === 'needs_reopen' ? 'needs_reopen' : 'awaiting';
      }
      return phase;
    case 'FOREGROUND':
      if (phase === 'awaiting') return 'syncing';
      // needs_reopen / syncing 保持：多轮 recheck 不卸掉「重新打开应用」
      return phase;
    case 'USER_RECHECK':
      if (phase === 'needs_reopen') return 'needs_reopen';
      if (phase === 'syncing') return 'syncing';
      // idle / awaiting → 进入同步，确保能耗尽到 needs_reopen
      return 'syncing';
    case 'SYNC_TICK':
      if (phase !== 'syncing' && phase !== 'needs_reopen') return phase;
      if (!hasStickyDenied(event.status)) return 'idle';
      // sticky 仍 denied：syncing 继续；needs_reopen 保持
      return phase;
    case 'SYNC_EXHAUSTED':
      if (phase !== 'syncing' && phase !== 'needs_reopen') return phase;
      return hasStickyDenied(event.status) ? 'needs_reopen' : 'idle';
    case 'REOPEN_CLICKED':
      return phase;
    default:
      return phase;
  }
}

export function welcomeHintKey(
  phase: WelcomePermPhase,
  allRequiredGranted: boolean,
): 'permissionReady' | 'waitingPermission' | 'syncingPermission' | 'reopenToApply' {
  if (allRequiredGranted) return 'permissionReady';
  if (phase === 'syncing') return 'syncingPermission';
  if (phase === 'needs_reopen') return 'reopenToApply';
  return 'waitingPermission';
}
