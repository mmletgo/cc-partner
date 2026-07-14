/**
 * Operational Notification Coordinator。
 *
 * Business Logic（为什么需要这个模块）:
 *   Orchestrator 的 Human Review / Blocked / remote outbox failed / Done 需要可配置的
 *   系统通知；通知只提醒，不执行动作，正文必须隐私安全。冷启动与 gap 不能刷屏旧状态，
 *   且在前台权威页（Attention/Workbench）时只更新 Inbox badge、不重复发 OS 通知。
 *
 * Code Logic（这个模块做什么）:
 *   1) 先注册 Tauri live/gap listener 并按 (ownerId,sequence) 缓冲
 *   2) 拉 snapshot 作为 no-notify baseline（seed dedupe）
 *   3) 丢弃 sequence<=asOfCursor 的缓冲，顺序 drain 更大 cursor
 *   4) live 模式消费新事件；Gap/owner change 暂停并重 handshake，不卸载 listener
 *   5) 偏好/权限/前台抑制决定是否 sendOperationalNotification；会通知时始终
 *      requestAttentionInvalidation()
 */

import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation } from 'react-router-dom';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  BACKEND_RUNTIME_GAP_EVENT,
  OPERATIONAL_NOTIFICATION_EVENT,
  operationalNotificationsApi,
  type OperationalNotificationEvent,
  type OperationalNotificationKind,
  type OperationalNotificationSnapshot,
} from '@/api/operationalNotifications';
import { orchestratorConfigApi } from '@/api/orchestratorConfig';
import {
  checkNotificationGranted,
  sendOperationalNotification,
} from '@/lib/notification';
import { requestAttentionInvalidation } from './attentionInvalidation';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/** 运营通知偏好（与 OrchestratorAutomationConfig 四字段对齐）。 */
export interface OperationalNotificationPreferences {
  notifyHumanReview: boolean;
  notifyBlocked: boolean;
  notifyRemoteOutboxFailed: boolean;
  notifyTaskDone: boolean;
}

/** 默认偏好：Human Review/Blocked/outbox failed 开，Done 关。 */
export const DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES: OperationalNotificationPreferences =
  {
    notifyHumanReview: true,
    notifyBlocked: true,
    notifyRemoteOutboxFailed: true,
    notifyTaskDone: false,
  };

type HandshakePhase = 'pending' | 'live';

/**
 * Business Logic（为什么需要这个函数）:
 *   普通 Vite/Playwright 浏览器环境没有 Tauri event internals，直接 listen 会白屏。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否为函数。
 */
function canListenToTauriEvents(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同一状态 revision 不得重复弹系统通知（含断线 replay）。
 *
 * Code Logic（这个函数做什么）:
 *   拼接 `${kind}:${opaqueSourceId}:${stateVersion}`。
 */
export function operationalNotificationDedupeKey(
  event: Pick<
    OperationalNotificationEvent,
    'kind' | 'opaqueSourceId' | 'stateVersion'
  >,
): string {
  return `${event.kind}:${event.opaqueSourceId}:${event.stateVersion}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   偏好按 kind 开关；Done 默认关，其它默认开。
 *
 * Code Logic（这个函数做什么）:
 *   将 kind 映射到 preferences 布尔字段。
 */
export function isOperationalNotificationKindEnabled(
  kind: OperationalNotificationKind,
  preferences: OperationalNotificationPreferences,
): boolean {
  switch (kind) {
    case 'humanReview':
      return preferences.notifyHumanReview;
    case 'blocked':
      return preferences.notifyBlocked;
    case 'remoteOutboxFailed':
      return preferences.notifyRemoteOutboxFailed;
    case 'taskDone':
      return preferences.notifyTaskDone;
    default:
      return false;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户已在 Attention/Workbench 权威界面且窗口可见时，OS 通知会打扰且多余；
 *   Inbox/badge 仍需失效刷新。
 *
 * Code Logic（这个函数做什么）:
 *   document.visibilityState==='visible' 且 pathname 为 /attention 或 /workbench 时返回 true。
 */
export function shouldSuppressOperationalOsNotification(
  pathname: string,
  visibilityState: DocumentVisibilityState | string,
): boolean {
  if (visibilityState !== 'visible') return false;
  return pathname === '/attention' || pathname === '/workbench';
}

/** 运营通知 i18n key 对（必须是字面量以便 t() 类型校验）。 */
type OperationalNotificationI18nKeys = {
  titleKey:
    | 'orchestrator:notifications.humanReview.title'
    | 'orchestrator:notifications.blocked.title'
    | 'orchestrator:notifications.remoteOutboxFailed.title'
    | 'orchestrator:notifications.taskDone.title';
  bodyKey:
    | 'orchestrator:notifications.humanReview.body'
    | 'orchestrator:notifications.blocked.body'
    | 'orchestrator:notifications.remoteOutboxFailed.body'
    | 'orchestrator:notifications.taskDone.body';
};

/**
 * Business Logic（为什么需要这个函数）:
 *   系统通知 title/body 只能是固定隐私安全文案，禁止塞任务标题/goal。
 *
 * Code Logic（这个函数做什么）:
 *   按 kind 返回 i18n key 字面量对，供 t() 编译期校验。
 */
function i18nKeysForKind(kind: OperationalNotificationKind): OperationalNotificationI18nKeys {
  switch (kind) {
    case 'humanReview':
      return {
        titleKey: 'orchestrator:notifications.humanReview.title',
        bodyKey: 'orchestrator:notifications.humanReview.body',
      };
    case 'blocked':
      return {
        titleKey: 'orchestrator:notifications.blocked.title',
        bodyKey: 'orchestrator:notifications.blocked.body',
      };
    case 'remoteOutboxFailed':
      return {
        titleKey: 'orchestrator:notifications.remoteOutboxFailed.title',
        bodyKey: 'orchestrator:notifications.remoteOutboxFailed.body',
      };
    case 'taskDone':
      return {
        titleKey: 'orchestrator:notifications.taskDone.title',
        bodyKey: 'orchestrator:notifications.taskDone.body',
      };
    default:
      return {
        titleKey: 'orchestrator:notifications.blocked.title',
        bodyKey: 'orchestrator:notifications.blocked.body',
      };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live event payload 可能缺字段，必须 fail-closed 丢弃，避免坏数据写 dedupe 或弹通知。
 *
 * Code Logic（这个函数做什么）:
 *   校验 kind/opaqueSourceId/stateVersion/occurredAt，可选 owner/sequence。
 */
function normalizeOperationalEvent(
  raw: unknown,
): OperationalNotificationEvent | null {
  if (!raw || typeof raw !== 'object') return null;
  const obj = raw as Record<string, unknown>;
  const kind = obj.kind;
  if (
    kind !== 'humanReview' &&
    kind !== 'blocked' &&
    kind !== 'remoteOutboxFailed' &&
    kind !== 'taskDone'
  ) {
    return null;
  }
  if (typeof obj.opaqueSourceId !== 'string' || obj.opaqueSourceId.length === 0) {
    return null;
  }
  if (typeof obj.stateVersion !== 'number' || !Number.isFinite(obj.stateVersion)) {
    return null;
  }
  if (typeof obj.occurredAt !== 'string') return null;

  const event: OperationalNotificationEvent = {
    kind,
    opaqueSourceId: obj.opaqueSourceId,
    stateVersion: obj.stateVersion,
    occurredAt: obj.occurredAt,
  };
  if (typeof obj.ownerInstanceId === 'string') {
    event.ownerInstanceId = obj.ownerInstanceId;
  }
  if (typeof obj.sequence === 'number' && Number.isFinite(obj.sequence)) {
    event.sequence = obj.sequence;
  }
  return event;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   缓冲事件需按 sequence 顺序 drain，保证与 owner 事件序一致。
 *
 * Code Logic（这个函数做什么）:
 *   稳定排序：sequence 升序，缺失 sequence 排最后。
 */
function sortBufferedEvents(
  events: OperationalNotificationEvent[],
): OperationalNotificationEvent[] {
  return [...events].sort((a, b) => {
    const sa = typeof a.sequence === 'number' ? a.sequence : Number.MAX_SAFE_INTEGER;
    const sb = typeof b.sequence === 'number' ? b.sequence : Number.MAX_SAFE_INTEGER;
    if (sa !== sb) return sa - sb;
    return 0;
  });
}

/**
 * 运营通知协调 hook（无 UI，副作用-only）。
 *
 * Business Logic（为什么需要这个 hook）:
 *   全局常驻于 App providers 内，消费 owner snapshot/live/gap 并按偏好发系统通知。
 *
 * Code Logic（这个 hook 做什么）:
 *   挂载时加载偏好、注册 listener、执行 handshake；pathname 变化仅影响前台抑制判定；
 *   卸载时 unlisten 并取消 in-flight handshake generation。
 */
export function useOperationalNotifications(): void {
  const { t } = useTranslation(['orchestrator']);
  const location = useLocation();
  const pathnameRef = useRef(location.pathname);
  pathnameRef.current = location.pathname;

  const preferencesRef = useRef<OperationalNotificationPreferences>({
    ...DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES,
  });
  const phaseRef = useRef<HandshakePhase>('pending');
  const bufferRef = useRef<OperationalNotificationEvent[]>([]);
  const seenKeysRef = useRef<Set<string>>(new Set());
  const cursorRef = useRef<{ ownerInstanceId: string; sequence: number } | null>(
    null,
  );
  const handshakeGenerationRef = useRef(0);
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    let cancelled = false;
    let notificationUnlisten: UnlistenFn | null = null;
    let gapUnlisten: UnlistenFn | null = null;
    let listenersReady = false;

    /**
     * Business Logic（为什么需要这个函数）:
     *   偏好变更后协调器应尊重最新开关，不必要求用户重启应用。
     *
     * Code Logic（这个函数做什么）:
     *   从 orchestratorConfigApi.get 读取四字段，失败保留默认/当前值。
     */
    const loadPreferences = async (): Promise<void> => {
      try {
        const config = await orchestratorConfigApi.get();
        if (cancelled) return;
        preferencesRef.current = {
          notifyHumanReview:
            typeof config.notifyHumanReview === 'boolean'
              ? config.notifyHumanReview
              : DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES.notifyHumanReview,
          notifyBlocked:
            typeof config.notifyBlocked === 'boolean'
              ? config.notifyBlocked
              : DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES.notifyBlocked,
          notifyRemoteOutboxFailed:
            typeof config.notifyRemoteOutboxFailed === 'boolean'
              ? config.notifyRemoteOutboxFailed
              : DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES.notifyRemoteOutboxFailed,
          notifyTaskDone:
            typeof config.notifyTaskDone === 'boolean'
              ? config.notifyTaskDone
              : DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES.notifyTaskDone,
        };
      } catch {
        // 配置不可用时使用默认偏好，不阻断协调器
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   偏好开启且状态未见过时，需要失效 Attention 并可能发 OS 通知。
     *
     * Code Logic（这个函数做什么）:
     *   dedupe → 偏好 → Attention invalidation → 权限 → 前台抑制 → send title/body only。
     */
    const maybeNotify = async (
      event: OperationalNotificationEvent,
    ): Promise<void> => {
      const key = operationalNotificationDedupeKey(event);
      if (seenKeysRef.current.has(key)) return;
      seenKeysRef.current.add(key);

      if (!isOperationalNotificationKindEnabled(event.kind, preferencesRef.current)) {
        return;
      }

      // 会通知时始终刷新 Inbox/badge（即使 OS 被抑制）
      requestAttentionInvalidation();

      const visibility =
        typeof document !== 'undefined' ? document.visibilityState : 'hidden';
      if (shouldSuppressOperationalOsNotification(pathnameRef.current, visibility)) {
        return;
      }

      try {
        if (!(await checkNotificationGranted())) return;
      } catch {
        return;
      }
      if (cancelled) return;

      const keys = i18nKeysForKind(event.kind);
      const title = tRef.current(keys.titleKey);
      const body = tRef.current(keys.bodyKey);
      try {
        sendOperationalNotification({ title, body });
      } catch {
        // 发送失败静默
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   gap/冷启动后必须用 snapshot 重建 no-notify baseline，再消费更大 cursor。
     *
     * Code Logic（这个函数做什么）:
     *   generation 防竞态；拉 snapshot → seed dedupe → 过滤缓冲 → drain → live。
     */
    const runHandshake = async (): Promise<void> => {
      const generation = ++handshakeGenerationRef.current;
      phaseRef.current = 'pending';

      let snapshot: OperationalNotificationSnapshot;
      try {
        snapshot = await operationalNotificationsApi.getSnapshot();
      } catch {
        if (cancelled || generation !== handshakeGenerationRef.current) return;
        // snapshot 失败：保持 pending，缓冲继续；不切 live 以免无 baseline 刷屏
        return;
      }

      if (cancelled || generation !== handshakeGenerationRef.current) return;

      // baseline: snapshot items 全部 no-notify
      for (const item of snapshot.items) {
        seenKeysRef.current.add(operationalNotificationDedupeKey(item));
      }
      cursorRef.current = {
        ownerInstanceId: snapshot.asOfCursor.ownerInstanceId,
        sequence: snapshot.asOfCursor.sequence,
      };

      const asOfOwner = snapshot.asOfCursor.ownerInstanceId;
      const asOfSeq = snapshot.asOfCursor.sequence;
      const remaining: OperationalNotificationEvent[] = [];
      for (const buffered of bufferRef.current) {
        const owner = buffered.ownerInstanceId;
        const seq = buffered.sequence;
        // 同 owner 且 sequence<=asOf → baseline 丢弃；无 sequence 保守保留 drain
        if (
          typeof owner === 'string' &&
          owner === asOfOwner &&
          typeof seq === 'number' &&
          seq <= asOfSeq
        ) {
          // 仍 seed dedupe，避免后续 live 重复
          seenKeysRef.current.add(operationalNotificationDedupeKey(buffered));
          continue;
        }
        // owner 不匹配的缓冲在 re-handshake 后通常应丢弃（由新 snapshot 覆盖）
        if (typeof owner === 'string' && owner !== asOfOwner) {
          continue;
        }
        remaining.push(buffered);
      }
      bufferRef.current = [];

      const ordered = sortBufferedEvents(remaining);
      for (const event of ordered) {
        if (cancelled || generation !== handshakeGenerationRef.current) return;
        await maybeNotify(event);
      }

      if (cancelled || generation !== handshakeGenerationRef.current) return;
      phaseRef.current = 'live';
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   live 事件到达时若 handshake 未完成，必须缓冲，避免 snapshot 窗口丢事件。
     *
     * Code Logic（这个函数做什么）:
     *   pending → push buffer；live 且 owner 变 → re-handshake；否则 maybeNotify。
     */
    const handleLiveEvent = (raw: unknown): void => {
      const event = normalizeOperationalEvent(raw);
      if (!event) return;

      if (phaseRef.current === 'pending') {
        bufferRef.current.push(event);
        return;
      }

      const cursor = cursorRef.current;
      if (
        cursor &&
        typeof event.ownerInstanceId === 'string' &&
        event.ownerInstanceId !== cursor.ownerInstanceId
      ) {
        // owner restart：清空旧 dedupe，由新 snapshot baseline 重建
        seenKeysRef.current = new Set();
        bufferRef.current.push(event);
        void runHandshake();
        return;
      }

      if (
        cursor &&
        typeof event.ownerInstanceId === 'string' &&
        event.ownerInstanceId === cursor.ownerInstanceId &&
        typeof event.sequence === 'number' &&
        event.sequence <= cursor.sequence
      ) {
        // 过时/重放 cursor：只记 dedupe，不通知
        seenKeysRef.current.add(operationalNotificationDedupeKey(event));
        return;
      }

      if (typeof event.sequence === 'number' && cursor) {
        cursorRef.current = {
          ownerInstanceId: event.ownerInstanceId ?? cursor.ownerInstanceId,
          sequence: Math.max(cursor.sequence, event.sequence),
        };
      }

      void maybeNotify(event);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   N1 gap 表示可能丢事件，必须暂停 live 消费并重 baseline。
     *
     * Code Logic（这个函数做什么）:
     *   phase=pending；保留 buffer listener；runHandshake。
     */
    const handleGap = (raw: unknown): void => {
      // gap payload 若携带新 owner，可提前记录；handshake 以 snapshot 为准
      if (raw && typeof raw === 'object') {
        const owner = (raw as Record<string, unknown>).ownerInstanceId;
        if (typeof owner === 'string' && cursorRef.current) {
          if (owner !== cursorRef.current.ownerInstanceId) {
            // owner 切换：清空 dedupe 由新 snapshot 重建
            seenKeysRef.current = new Set();
          }
        }
      }
      phaseRef.current = 'pending';
      void runHandshake();
    };

    const start = async (): Promise<void> => {
      await loadPreferences();
      if (cancelled) return;

      if (!canListenToTauriEvents()) {
        // 非 Tauri 环境：仍尝试一次 snapshot baseline（测试可 mock），但不注册 listener
        await runHandshake();
        return;
      }

      try {
        const unlistenNotification = await listen<unknown>(
          OPERATIONAL_NOTIFICATION_EVENT,
          (event) => {
            handleLiveEvent(event.payload);
          },
        );
        if (cancelled) {
          unlistenNotification();
          return;
        }
        notificationUnlisten = unlistenNotification;

        const unlistenGap = await listen<unknown>(BACKEND_RUNTIME_GAP_EVENT, (event) => {
          handleGap(event.payload);
        });
        if (cancelled) {
          unlistenGap();
          notificationUnlisten?.();
          notificationUnlisten = null;
          return;
        }
        gapUnlisten = unlistenGap;
        listenersReady = true;
      } catch {
        // listener 注册失败：不刷屏；仍可尝试 snapshot 建立 baseline
        listenersReady = false;
      }

      await runHandshake();
      void listenersReady;
    };

    void start();

    return () => {
      cancelled = true;
      handshakeGenerationRef.current += 1;
      if (notificationUnlisten) {
        notificationUnlisten();
        notificationUnlisten = null;
      }
      if (gapUnlisten) {
        gapUnlisten();
        gapUnlisten = null;
      }
    };
  }, []);
}

/**
 * OperationalNotificationCoordinator - App 内无 UI 挂载点。
 *
 * Business Logic（为什么需要这个组件）:
 *   App providers 内需要常驻协调器副作用，且 hooks 必须在组件内调用。
 *
 * Code Logic（这个组件做什么）:
 *   调用 useOperationalNotifications() 并渲染 null。
 */
export function OperationalNotificationCoordinator(): null {
  useOperationalNotifications();
  return null;
}
