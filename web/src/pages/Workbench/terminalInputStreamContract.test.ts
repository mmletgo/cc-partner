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

    expect(desktop).toMatch(/enqueueInput\(/);
    // writeInput 仅允许出现在 docstring 中（解释旧 API 被替换），禁止方法调用或字段访问。
    expect(desktop).not.toMatch(/\.writeInput\(/);
    expect(desktop).not.toMatch(/\bwriteInput\s*:/);
    expect(mobile).toContain('inputStreamRef.current?.enqueue');
    expect(mobile).not.toMatch(/\.writeInput\(/);
    expect(http).not.toContain('/sessions/write');
  });
});
