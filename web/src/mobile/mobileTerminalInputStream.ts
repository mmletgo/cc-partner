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
}

/**
 * 移动端终端输入常驻连接。
 *
 * Business Logic（为什么需要这个类）:
 *   手机软键盘不能为每个 onData 片段等待 HTTP/远端 RTT；断线时未 ACK 输入又不能自动重放。
 *
 * Code Logic（这个类做什么）:
 *   维护单 WebSocket、per-session lane/seq/outstanding；ready 后同步发送输入，ACK 仅用于确认，
 *   不阻塞后续帧。连接中断时封锁所有存在 outstanding 的 lane 并清空状态。
 */
export class MobileTerminalInputStream {
  private readonly socket: WebSocket;
  private readonly lanes = new Map<string, InputLane>();
  private state: MobileTerminalInputStreamState = { status: 'connecting' };
  private readonly createId: () => string;

  public constructor(options: MobileTerminalInputStreamOptions) {
    const url = new URL('/api/mobile/workbench/terminal-input-stream', window.location.href);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    this.createId = options.createId ?? (() => crypto.randomUUID());
    const createSocket = options.createWebSocket ?? ((socketUrl, protocols) => new WebSocket(socketUrl, protocols));
    this.socket = createSocket(url.toString(), TERMINAL_INPUT_SUBPROTOCOL);
    const publish = (next: MobileTerminalInputStreamState): void => {
      this.state = next;
      options.onStateChange(next);
    };
    publish(this.state);
    this.socket.addEventListener('open', () => {
      this.socket.send(JSON.stringify({ type: 'hello', clientId: `mobile-${this.createId()}` }));
    });
    this.socket.addEventListener('message', (event: MessageEvent<string>) => {
      this.handleMessage(event.data, publish);
    });
    this.socket.addEventListener('close', () => {
      const uncertain = [...this.lanes.values()].some((lane) => lane.outstanding.size > 0);
      this.lanes.forEach((lane) => {
        lane.blocked = true;
        lane.outstanding.clear();
        lane.outstandingBytes = 0;
      });
      publish(
        uncertain
          ? { status: 'blocked', message: '输入连接已断开；未确认输入不会自动重放' }
          : { status: 'closed' },
      );
    });
    this.socket.addEventListener('error', () => {
      publish({ status: 'blocked', message: '终端输入连接失败' });
    });
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

  /** 关闭连接并丢弃所有本地状态；不会重放 outstanding。 */
  public close(): void {
    this.lanes.clear();
    this.socket.close(1000, 'component disposed');
  }

  private handleMessage(
    raw: string,
    publish: (state: MobileTerminalInputStreamState) => void,
  ): void {
    let message: unknown;
    try {
      message = JSON.parse(raw);
    } catch {
      publish({ status: 'blocked', message: '终端输入响应格式无效' });
      return;
    }
    if (!isRecord(message) || typeof message.type !== 'string') return;
    if (message.type === 'ready') {
      publish({ status: 'ready' });
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
      publish({
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
