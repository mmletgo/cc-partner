import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'vitest';

/** 读取仓库内前端源码，锁定 xterm 输入不得回退旧 HTTP/RPC 写入口。 */
function source(relativePath: string): string {
  return readFileSync(new URL(`../../${relativePath}`, import.meta.url), 'utf8');
}

describe('terminal input stream static contract', () => {
  test('desktop 与 mobile xterm 仅使用专用输入流', () => {
    const desktop = source('pages/Workbench/controllers/useWorkbenchTerminalController.ts');
    const mobile = source('mobile/components/MobileTerminalPanel.tsx');
    const http = source('api/workbenchHttp.ts');

    expect(desktop).toContain('sessions.enqueueInput');
    expect(desktop).not.toContain('sessions.writeInput');
    expect(mobile).toContain('inputStreamRef.current?.enqueue');
    expect(mobile).not.toContain('sessions.writeInput');
    expect(http).not.toContain('/sessions/write');
  });
});
