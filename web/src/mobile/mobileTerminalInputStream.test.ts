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
  test('LAN HTTP 环境缺少 crypto.randomUUID 时仍发送 hello', () => {
    const socket = new FakeWebSocket();
    const states: MobileTerminalInputStreamState[] = [];
    vi.stubGlobal('crypto', {});
    try {
      new MobileTerminalInputStream({
        createWebSocket: () => socket as unknown as WebSocket,
        onStateChange: (state) => states.push(state),
      });

      socket.open();

      expect(socket.sent).toHaveLength(1);
      expect(JSON.parse(socket.sent[0])).toMatchObject({ type: 'hello' });
      expect(JSON.parse(socket.sent[0]).clientId).toMatch(/^mobile-/);
    } finally {
      vi.unstubAllGlobals();
    }
  });

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

  test('存在未 ACK 输入时断线封锁且不重放，前台恢复后可发新输入', () => {
    const sockets: FakeWebSocket[] = [];
    const states: MobileTerminalInputStreamState[] = [];
    const stream = new MobileTerminalInputStream({
      createWebSocket: () => {
        const next = new FakeWebSocket();
        sockets.push(next);
        return next as unknown as WebSocket;
      },
      createId: () => 'stable-id',
      isDocumentVisible: () => true,
      onStateChange: (state) => states.push(state),
    });
    sockets[0]?.open();
    sockets[0]?.receive({ type: 'ready', deviceId: 'device-1' });
    stream.enqueue('session-1', 'unknown');
    const sentBeforeClose = sockets[0]?.sent.length ?? 0;

    sockets[0]?.close();

    expect(states.some((state) => state.status === 'blocked')).toBe(true);
    expect(sockets[0]?.sent).toHaveLength(sentBeforeClose);
    expect(sockets).toHaveLength(2);
    expect(() => stream.enqueue('session-1', 'later')).toThrow('尚未就绪');

    sockets[1]?.open();
    sockets[1]?.receive({ type: 'ready', deviceId: 'device-1' });
    stream.enqueue('session-1', 'later');

    expect(sockets[0]?.sent).toHaveLength(sentBeforeClose);
    expect(JSON.parse(sockets[1]?.sent.at(-1) ?? '{}')).toMatchObject({
      type: 'input',
      sessionId: 'session-1',
      seq: 1,
      data: 'later',
    });
    stream.close();
  });

  test('后台断线不立即重连，回到前台才重建输入流', () => {
    const sockets: FakeWebSocket[] = [];
    let visible = false;
    const resume = {
      handler: undefined as
        | ((detail: { persistedPageshow: boolean; hidden?: boolean }) => void)
        | undefined,
    };
    const stream = new MobileTerminalInputStream({
      createWebSocket: () => {
        const next = new FakeWebSocket();
        sockets.push(next);
        return next as unknown as WebSocket;
      },
      isDocumentVisible: () => visible,
      subscribeResume: (handler) => {
        resume.handler = handler;
        return () => {
          resume.handler = undefined;
        };
      },
      onStateChange: () => undefined,
    });
    sockets[0]?.open();
    sockets[0]?.receive({ type: 'ready', deviceId: 'device-1' });
    sockets[0]?.close();

    expect(sockets).toHaveLength(1);
    expect(() => stream.enqueue('session-1', 'later')).toThrow('尚未就绪');

    visible = true;
    resume.handler?.({ persistedPageshow: false });

    expect(sockets).toHaveLength(2);
    sockets[1]?.open();
    sockets[1]?.receive({ type: 'ready', deviceId: 'device-1' });
    stream.enqueue('session-1', 'after-resume');
    expect(JSON.parse(sockets[1]?.sent.at(-1) ?? '{}')).toMatchObject({
      type: 'input',
      data: 'after-resume',
    });
    stream.close();
  });

  test('前台恢复时即使旧 socket 仍显示 OPEN 也会重建半开连接', () => {
    const sockets: FakeWebSocket[] = [];
    const resume = {
      handler: undefined as
        | ((detail: { persistedPageshow: boolean; hidden?: boolean }) => void)
        | undefined,
    };
    const stream = new MobileTerminalInputStream({
      createWebSocket: () => {
        const next = new FakeWebSocket();
        sockets.push(next);
        return next as unknown as WebSocket;
      },
      isDocumentVisible: () => true,
      subscribeResume: (handler) => {
        resume.handler = handler;
        return () => {
          resume.handler = undefined;
        };
      },
      onStateChange: () => undefined,
    });
    sockets[0]?.open();
    sockets[0]?.receive({ type: 'ready', deviceId: 'device-1' });
    expect(sockets[0]?.readyState).toBe(FakeWebSocket.OPEN);

    resume.handler?.({ persistedPageshow: false });

    expect(sockets).toHaveLength(2);
    stream.close();
  });

  test('组件 dispose 的 close 不会自动重连', () => {
    const sockets: FakeWebSocket[] = [];
    const stream = new MobileTerminalInputStream({
      createWebSocket: () => {
        const next = new FakeWebSocket();
        sockets.push(next);
        return next as unknown as WebSocket;
      },
      isDocumentVisible: () => true,
      onStateChange: () => undefined,
    });
    sockets[0]?.open();
    sockets[0]?.receive({ type: 'ready', deviceId: 'device-1' });
    stream.close();

    expect(sockets).toHaveLength(1);
  });

  test('握手前连接被中止触发 error 时报告 blocked(dev StrictMode cleanup 场景)', () => {
    // dev StrictMode 双调用 inputStream effect:首个 stream 在 CONNECTING 阶段被 cleanup 的
    // close() 中止,浏览器对中止未完成的 upgrade 会先 dispatch error 再 close。该 stream 的
    // onStateChange 仍会报告 blocked「终端输入连接失败」;MobileTerminalPanel 须用 active 守卫
    // 忽略被废弃 stream 的事件,并在 ready 时清除历史 blocked 错误,否则 ready 后仍显示该错误。
    const socket = new FakeWebSocket();
    const states: MobileTerminalInputStreamState[] = [];
    new MobileTerminalInputStream({
      createWebSocket: () => socket as unknown as WebSocket,
      onStateChange: (state) => states.push(state),
    });

    socket.dispatchEvent(new Event('error'));

    expect(states).toContainEqual({ status: 'blocked', message: '终端输入连接失败' });
  });
});
