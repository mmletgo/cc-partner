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

export const SYNC_DELAYS_MS: readonly number[] = [0, 400, 1200, 2500];

export type StickyStatusSlice = {
  screenCapture: { granted: boolean };
  accessibility: { granted: boolean };
  inputMonitoring: { granted: boolean };
};

export type WelcomePermEvent =
  | { type: 'GO_SETTINGS'; permission: string }
  | { type: 'FOREGROUND' }
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
 * Code Logic: 按下表转移；未知事件保持 phase。
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
        return 'awaiting';
      }
      return phase;
    case 'FOREGROUND':
      return phase === 'awaiting' || phase === 'needs_reopen' ? 'syncing' : phase;
    case 'SYNC_TICK':
      if (phase !== 'syncing') return phase;
      return hasStickyDenied(event.status) ? 'syncing' : 'idle';
    case 'SYNC_EXHAUSTED':
      if (phase !== 'syncing') return phase;
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
