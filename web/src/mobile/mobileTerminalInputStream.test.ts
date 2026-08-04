// @vitest-environment jsdom

import { describe, expect, test, vi } from 'vitest';
import {
  MobileTerminalInputStream,
  TERMINAL_INPUT_SUBPROTOCOL,
  type MobileTerminalInputStreamState,
} from './mobileTerminalInputStream';

class FakeWebSocket extends EventTarget {
  public static readonly OPEN = 1;
  public readyState = 0;
  public bufferedAmount = 0;
  public readonly sent: string[] = [];
  public close = vi.fn(() => {
    this.readyState = 3;
    this.dispatchEvent(new Event('close'));
  });

  /** 模拟握手成功。 */
  public open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.dispatchEvent(new Event('open'));
  }

  /** 记录浏览器发出的文本帧。 */
  public send(data: string): void {
    this.sent.push(data);
  }

  /** 模拟服务端文本帧。 */
  public receive(value: unknown): void {
    this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(value) }));
  }
}

describe('MobileTerminalInputStream', () => {
  test('ready 后连续发送不等待 ACK，并维持 per-session seq', () => {
    const socket = new FakeWebSocket();
    const states: MobileTerminalInputStreamState[] = [];
    const ids = ['client', 'lane'];
    const stream = new MobileTerminalInputStream({
      createWebSocket: (url, protocols) => {
        expect(url).toContain('/api/mobile/workbench/terminal-input-stream');
        expect(protocols).toBe(TERMINAL_INPUT_SUBPROTOCOL);
        return socket as unknown as WebSocket;
      },
      createId: () => ids.shift() ?? 'fallback',
      onStateChange: (state) => states.push(state),
    });

    socket.open();
    socket.receive({ type: 'ready', deviceId: 'device-1' });
    stream.enqueue('session-1', 'a');
    stream.enqueue('session-1', 'b');

    expect(states.at(-1)).toEqual({ status: 'ready' });
    expect(socket.sent.map((raw) => JSON.parse(raw))).toEqual([
      { type: 'hello', clientId: 'mobile-client' },
      { type: 'input', laneId: 'lane', sessionId: 'session-1', seq: 1, data: 'a' },
      { type: 'input', laneId: 'lane', sessionId: 'session-1', seq: 2, data: 'b' },
    ]);
  });

  test('存在未 ACK 输入时断线封锁且不重放', () => {
    const socket = new FakeWebSocket();
    const states: MobileTerminalInputStreamState[] = [];
    const stream = new MobileTerminalInputStream({
      createWebSocket: () => socket as unknown as WebSocket,
      createId: () => 'stable-id',
      onStateChange: (state) => states.push(state),
    });
    socket.open();
    socket.receive({ type: 'ready', deviceId: 'device-1' });
    stream.enqueue('session-1', 'unknown');
    const sentBeforeClose = socket.sent.length;

    socket.close();

    expect(states.at(-1)?.status).toBe('blocked');
    expect(socket.sent).toHaveLength(sentBeforeClose);
    expect(() => stream.enqueue('session-1', 'later')).toThrow('尚未就绪');
  });
});
