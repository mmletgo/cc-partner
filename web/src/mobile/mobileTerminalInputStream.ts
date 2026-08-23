import { shouldReconnectOnDocumentResume } from '@/lib/documentResumeReconnect';

export const TERMINAL_INPUT_SUBPROTOCOL = 'cc-partner.terminal-input.v1';

export type MobileTerminalInputStreamState =
  | { status: 'connecting' }
  | { status: 'ready' }
  | { status: 'blocked'; message: string }
  | { status: 'closed' };

interface InputLane {
  laneId: string;
  nextSeq: number;
  outstanding: Map<number, number>;
  outstandingBytes: number;
  blocked: boolean;
}

const MAX_FRAME_BYTES = 32 * 1024;
const MAX_LANE_OUTSTANDING_BYTES = 1024 * 1024;
const MAX_SOCKET_BUFFERED_BYTES = 2 * 1024 * 1024;
const UTF8_ENCODER = new TextEncoder();

export interface MobileTerminalInputStreamOptions {
  onStateChange: (state: MobileTerminalInputStreamState) => void;
  createWebSocket?: (url: string, protocols: string | string[]) => WebSocket;
  createId?: () => string;
  /** 测试注入：当前文档是否可见。默认读 document.visibilityState。 */
  isDocumentVisible?: () => boolean;
  /**
   * 测试注入：订阅前台恢复。默认监听 visibilitychange / pageshow / online。
   * handler 收到 persistedPageshow 时表示 bfcache 恢复。
   */
  subscribeResume?: (
    handler: (detail: { persistedPageshow: boolean; hidden?: boolean }) => void,
  ) => () => void;
  now?: () => number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机通常通过局域网明文 HTTP 打开 `/mobile`，该环境在部分浏览器中没有仅限安全上下文的
 *   `crypto.randomUUID()`；终端输入标识只要求进程内低碰撞，不能因此阻断 WebSocket hello。
 *
 * Code Logic（这个函数做什么）:
 *   优先使用 randomUUID；不可用时尝试 getRandomValues，最后用时间与随机数组合生成非安全标识。
 */
export function createMobileTerminalInputId(): string {
  const cryptoApi = globalThis.crypto;
  if (typeof cryptoApi?.randomUUID === 'function') {
    try {
      return cryptoApi.randomUUID();
    } catch {
      // 非安全上下文中的浏览器实现可能暴露方法但调用时拒绝，继续使用兼容路径。
    }
  }
  if (typeof cryptoApi?.getRandomValues === 'function') {
    try {
      const words = new Uint32Array(4);
      cryptoApi.getRandomValues(words);
      return Array.from(words, (word) => word.toString(16).padStart(8, '0')).join('-');
    } catch {
      // 极旧浏览器或受限 WebView 可能连 getRandomValues 也不可用。
    }
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random()
    .toString(36)
    .slice(2)}`;
}

/**
 * 默认订阅页面从后台回到前台。
 *
 * Business Logic（为什么需要这个函数）:
 *   iOS/Android 浏览器冻结页面后 WebSocket 半开；回到前台必须主动重建输入流。
 *
 * Code Logic（这个函数做什么）:
 *   监听 visibilitychange（仅 visible）、pageshow（带 persisted）与 online。
 */
function subscribeDocumentResume(
  handler: (detail: { persistedPageshow: boolean; hidden?: boolean }) => void,
): () => void {
  const onVisibility = (): void => {
    if (document.visibilityState === 'hidden') {
      handler({ persistedPageshow: false, hidden: true });
      return;
    }
    if (document.visibilityState === 'visible') {
      handler({ persistedPageshow: false });
    }
  };
  const onPageShow = (event: Event): void => {
    const persisted =
      'persisted' in event && Boolean((event as PageTransitionEvent).persisted);
    handler({ persistedPageshow: persisted });
  };
  const onOnline = (): void => {
    handler({ persistedPageshow: false });
  };
  document.addEventListener('visibilitychange', onVisibility);
  window.addEventListener('pageshow', onPageShow);
  window.addEventListener('online', onOnline);
  return () => {
    document.removeEventListener('visibilitychange', onVisibility);
    window.removeEventListener('pageshow', onPageShow);
    window.removeEventListener('online', onOnline);
  };
}

/**
 * 移动端终端输入常驻连接。
 *
 * Business Logic（为什么需要这个类）:
 *   手机软键盘不能为每个 onData 片段等待 HTTP/远端 RTT；断线时未 ACK 输入又不能自动重放。
 *
 * Code Logic（这个类做什么）:
 *   维护单 WebSocket、per-session lane/seq/outstanding；ready 后同步发送输入，ACK 仅用于确认，
 *   不阻塞后续帧。连接中断时封锁 outstanding 且不重放；前台恢复或意外 close 时重建 socket。
 */
export class MobileTerminalInputStream {
  private socket: WebSocket;
  private socketGeneration = 0;
  private readonly lanes = new Map<string, InputLane>();
  private state: MobileTerminalInputStreamState = { status: 'connecting' };
  private readonly createId: () => string;
  private readonly createSocket: (url: string, protocols: string | string[]) => WebSocket;
  private readonly onStateChange: (state: MobileTerminalInputStreamState) => void;
  private readonly isDocumentVisible: () => boolean;
  private readonly now: () => number;
  private readonly unsubscribeResume: () => void;
  private disposed = false;
  private everReady = false;
  private wasBackgrounded = false;
  private lastReconnectAtMs: number | null = null;

  public constructor(options: MobileTerminalInputStreamOptions) {
    this.createId = options.createId ?? createMobileTerminalInputId;
    this.createSocket =
      options.createWebSocket ?? ((socketUrl, protocols) => new WebSocket(socketUrl, protocols));
    this.onStateChange = options.onStateChange;
    this.isDocumentVisible =
      options.isDocumentVisible ?? (() => document.visibilityState === 'visible');
    this.now = options.now ?? (() => Date.now());
    this.socket = this.openSocket();
    this.publish(this.state);
    const subscribeResume = options.subscribeResume ?? subscribeDocumentResume;
    this.unsubscribeResume = subscribeResume((detail) => {
      if (detail.hidden) {
        this.wasBackgrounded = true;
        return;
      }
      this.attemptResume(detail.persistedPageshow);
    });
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   状态变化必须同步给面板，才能禁用输入并在 ready 后清掉断线错误。
   *
   * Code Logic（这个函数做什么）:
   *   写入 this.state 并回调 onStateChange。
   */
  private publish(next: MobileTerminalInputStreamState): void {
    this.state = next;
    this.onStateChange(next);
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   新建或替换 socket 时必须绑定当前 generation，避免旧 close/error 污染新连接。
   *
   * Code Logic（这个函数做什么）:
   *   用工厂创建 WebSocket，注册 open/message/close/error；返回新 socket。
   */
  private openSocket(): WebSocket {
    const url = new URL('/api/mobile/workbench/terminal-input-stream', window.location.href);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const generation = this.socketGeneration;
    const socket = this.createSocket(url.toString(), TERMINAL_INPUT_SUBPROTOCOL);
    socket.addEventListener('open', () => {
      if (this.disposed || generation !== this.socketGeneration) return;
      try {
        socket.send(JSON.stringify({ type: 'hello', clientId: `mobile-${this.createId()}` }));
      } catch {
        this.publish({ status: 'blocked', message: '终端输入连接初始化失败' });
      }
    });
    socket.addEventListener('message', (event: MessageEvent<string>) => {
      if (this.disposed || generation !== this.socketGeneration) return;
      this.handleMessage(event.data);
    });
    socket.addEventListener('close', () => {
      if (this.disposed || generation !== this.socketGeneration) return;
      const uncertain = [...this.lanes.values()].some((lane) => lane.outstanding.size > 0);
      this.lanes.forEach((lane) => {
        lane.blocked = true;
        lane.outstanding.clear();
        lane.outstandingBytes = 0;
      });
      this.publish(
        uncertain
          ? { status: 'blocked', message: '输入连接已断开；未确认输入不会自动重放' }
          : { status: 'closed' },
      );
      if (this.isDocumentVisible()) {
        this.reconnect();
      }
    });
    socket.addEventListener('error', () => {
      if (this.disposed || generation !== this.socketGeneration) return;
      this.publish({ status: 'blocked', message: '终端输入连接失败' });
    });
    return socket;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   页面从后台回到前台时，即使旧 socket 仍显示 OPEN 也可能是半开连接，必须重建。
   *
   * Code Logic（这个函数做什么）:
   *   按 shouldReconnectOnDocumentResume 判定后调用 reconnect。
   */
  private attemptResume(persistedPageshow: boolean): void {
    if (this.disposed) return;
    if (
      !shouldReconnectOnDocumentResume({
        visible: this.isDocumentVisible(),
        persistedPageshow,
        hasEstablishedSession: this.everReady,
        wasBackgrounded: this.wasBackgrounded,
        nowMs: this.now(),
        lastReconnectAtMs: this.lastReconnectAtMs,
      })
    ) {
      return;
    }
    this.wasBackgrounded = false;
    this.reconnect();
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   重建输入通道，让用户继续键入；已发送未 ACK 的帧结果未知，不得重放。
   *
   * Code Logic（这个函数做什么）:
   *   抬 generation、清空 lane、打开新 socket，再关闭仍未关闭的旧 socket。
   */
  private reconnect(): void {
    if (this.disposed) return;
    this.lastReconnectAtMs = this.now();
    const replacing = this.socket;
    this.socketGeneration += 1;
    this.lanes.clear();
    this.socket = this.openSocket();
    this.publish({ status: 'connecting' });
    if (replacing.readyState === WebSocket.CONNECTING || replacing.readyState === WebSocket.OPEN) {
      try {
        replacing.close(1000, 'replaced');
      } catch {
        // 旧 socket 可能已被浏览器关闭。
      }
    }
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   xterm onData 必须在 ready 且 lane 未封锁时立即写入 socket，不等待 ACK。
   *
   * Code Logic（这个函数做什么）:
   *   创建或复用 session lane，递增 seq、登记 outstanding 并发送 JSON input 帧。
   */
  public enqueue(sessionId: string, data: string): void {
    if (this.state.status !== 'ready' || this.socket.readyState !== WebSocket.OPEN) {
      throw new Error('终端输入流尚未就绪');
    }
    const frameBytes = UTF8_ENCODER.encode(data).byteLength;
    if (frameBytes > MAX_FRAME_BYTES) {
      throw new Error('单个终端输入帧超过 32 KiB');
    }
    const lane = this.lanes.get(sessionId) ?? {
      laneId: this.createId(),
      nextSeq: 1,
      outstanding: new Map<number, number>(),
      outstandingBytes: 0,
      blocked: false,
    };
    this.lanes.set(sessionId, lane);
    if (lane.blocked) throw new Error('终端输入通道已封锁');
    if (
      lane.outstandingBytes + frameBytes > MAX_LANE_OUTSTANDING_BYTES ||
      this.socket.bufferedAmount + frameBytes > MAX_SOCKET_BUFFERED_BYTES
    ) {
      lane.blocked = true;
      throw new Error('终端输入背压超过安全上限，通道已封锁');
    }
    const seq = lane.nextSeq;
    lane.nextSeq += 1;
    lane.outstanding.set(seq, frameBytes);
    lane.outstandingBytes += frameBytes;
    this.socket.send(JSON.stringify({
      type: 'input',
      laneId: lane.laneId,
      sessionId,
      seq,
      data,
    }));
  }

  /** 关闭连接并丢弃所有本地状态；不会重放 outstanding，也不会自动重连。 */
  public close(): void {
    this.disposed = true;
    this.unsubscribeResume();
    this.lanes.clear();
    try {
      this.socket.close(1000, 'component disposed');
    } catch {
      // 组件卸载时 socket 可能已经关闭。
    }
  }

  private handleMessage(raw: string): void {
    let message: unknown;
    try {
      message = JSON.parse(raw);
    } catch {
      this.publish({ status: 'blocked', message: '终端输入响应格式无效' });
      return;
    }
    if (!isRecord(message) || typeof message.type !== 'string') return;
    if (message.type === 'ready') {
      this.everReady = true;
      this.publish({ status: 'ready' });
      return;
    }
    if (message.type === 'ack' && typeof message.sessionId === 'string' && typeof message.seq === 'number') {
      const lane = this.lanes.get(message.sessionId);
      const acknowledgedBytes = lane?.outstanding.get(message.seq);
      if (lane && acknowledgedBytes != null) {
        lane.outstanding.delete(message.seq);
        lane.outstandingBytes = Math.max(0, lane.outstandingBytes - acknowledgedBytes);
      }
      return;
    }
    if (message.type === 'error') {
      const sessionId = typeof message.sessionId === 'string' ? message.sessionId : null;
      if (sessionId) {
        const lane = this.lanes.get(sessionId);
        if (lane) {
          lane.blocked = true;
          lane.outstanding.clear();
          lane.outstandingBytes = 0;
        }
      }
      this.publish({
        status: 'blocked',
        message: typeof message.message === 'string' ? message.message : '终端输入被后端拒绝',
      });
    }
  }
}

/** 判断未知 JSON 值是否为可安全读取字段的记录。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
