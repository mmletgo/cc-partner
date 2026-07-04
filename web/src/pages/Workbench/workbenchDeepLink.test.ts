import { parseWorkbenchDeepLink } from './workbenchDeepLink';

/**
 * Business Logic（为什么需要这个测试）:
 *   Orchestrator 任务需要把用户带回对应的 Workbench 项目、worktree 和终端窗口。
 *
 * Code Logic（这个测试做什么）:
 *   传入完整 query string，断言 helper 能解析出三个稳定 id。
 */
function testParseFullWorkbenchDeepLink(): void {
  const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=w1&sessionId=s1');

  if (parsed.projectId !== 'p1' || parsed.worktreeId !== 'w1' || parsed.sessionId !== 's1') {
    throw new Error(`expected complete deep link ids, got ${JSON.stringify(parsed)}`);
  }
}

/**
 * Business Logic（为什么需要这个测试）:
 *   用户直接打开 Workbench 时不应受到 deep link 状态影响。
 *
 * Code Logic（这个测试做什么）:
 *   传入空 query string，断言三个 id 全部归一成 null。
 */
function testParseEmptyWorkbenchDeepLink(): void {
  const parsed = parseWorkbenchDeepLink('');

  if (parsed.projectId !== null || parsed.worktreeId !== null || parsed.sessionId !== null) {
    throw new Error(`expected null deep link ids, got ${JSON.stringify(parsed)}`);
  }
}

/**
 * Business Logic（为什么需要这个测试）:
 *   Orchestrator 构造 URL 时可能只有部分关联 id，空参数不能污染 Workbench 的选择逻辑。
 *
 * Code Logic（这个测试做什么）:
 *   传入空字符串参数，断言 helper 会把空 worktree/session id 归一成 null。
 */
function testParseBlankWorkbenchDeepLinkValues(): void {
  const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=&sessionId=');

  if (parsed.projectId !== 'p1' || parsed.worktreeId !== null || parsed.sessionId !== null) {
    throw new Error(`expected blank ids to normalize to null, got ${JSON.stringify(parsed)}`);
  }
}

testParseFullWorkbenchDeepLink();
testParseEmptyWorkbenchDeepLink();
testParseBlankWorkbenchDeepLinkValues();
