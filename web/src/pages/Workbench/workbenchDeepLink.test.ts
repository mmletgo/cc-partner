import { describe, test } from 'vitest';
import { buildWorkbenchDeepLink, parseWorkbenchDeepLink } from './workbenchDeepLink';

describe('workbench deep links', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   Orchestrator 任务需要把用户带回对应的 Workbench 项目、worktree 和终端窗口。
   *
   * Code Logic（这个测试做什么）:
   *   传入完整 query string，断言 helper 能解析出三个稳定 id。
   */
  test('parses complete ids', () => {
    const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=w1&sessionId=s1');

    if (parsed.projectId !== 'p1' || parsed.worktreeId !== 'w1' || parsed.sessionId !== 's1') {
      throw new Error(`expected complete deep link ids, got ${JSON.stringify(parsed)}`);
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户直接打开 Workbench 时不应受到 deep link 状态影响。
   *
   * Code Logic（这个测试做什么）:
   *   传入空 query string，断言三个 id 全部归一成 null。
   */
  test('parses empty deep link', () => {
    const parsed = parseWorkbenchDeepLink('');

    if (parsed.projectId !== null || parsed.worktreeId !== null || parsed.sessionId !== null) {
      throw new Error(`expected null deep link ids, got ${JSON.stringify(parsed)}`);
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Orchestrator 构造 URL 时可能只有部分关联 id，空参数不能污染 Workbench 的选择逻辑。
   *
   * Code Logic（这个测试做什么）:
   *   传入空字符串参数，断言 helper 会把空 worktree/session id 归一成 null。
   */
  test('normalizes blank deep link values', () => {
    const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=&sessionId=');

    if (parsed.projectId !== 'p1' || parsed.worktreeId !== null || parsed.sessionId !== null) {
      throw new Error(`expected blank ids to normalize to null, got ${JSON.stringify(parsed)}`);
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   远端 Orchestrator 任务打开 Workbench 时，project/worktree/session id 已按 Workbench 远端前缀规则封装，
   *   deep link 构造和解析都不能剥掉 `remote:<device>:...` 前缀。
   *
   * Code Logic（这个测试做什么）:
   *   构造包含远端 project/worktree/session id 的 URL，断言 query 编码后再解析仍得到完整原始 id。
   */
  test('preserves remote shortcut ids', () => {
    const projectId = 'remote:device-a:project-hash';
    const worktreeId = 'remote:device-a:worktree-1';
    const sessionId = 'remote:device-a:session-1';
    const url = buildWorkbenchDeepLink({ projectId, worktreeId, sessionId });
    const parsed = parseWorkbenchDeepLink(url.replace('/workbench', ''));

    if (!url.includes('projectId=remote%3Adevice-a%3Aproject-hash')) {
      throw new Error(`expected remote project id to be URL encoded with prefix, got ${url}`);
    }

    if (
      parsed.projectId !== projectId ||
      parsed.worktreeId !== worktreeId ||
      parsed.sessionId !== sessionId
    ) {
      throw new Error(`expected remote shortcut ids to round trip, got ${JSON.stringify(parsed)}`);
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Attention Inbox 必须通过 view=automation + taskId 打开详情/Evidence，而不是终端。
   *
   * Code Logic（这个测试做什么）:
   *   构造/解析 automation+taskId deep link，断言字段 round-trip。
   */
  test('parses and builds automation task deep links', () => {
    const url = buildWorkbenchDeepLink({
      projectId: 'proj-1',
      worktreeId: null,
      sessionId: null,
      view: 'automation',
      taskId: 'task-9',
      outboxId: null,
    });
    const parsed = parseWorkbenchDeepLink(url.replace('/workbench', ''));

    if (
      url !== '/workbench?projectId=proj-1&view=automation&taskId=task-9' ||
      parsed.view !== 'automation' ||
      parsed.taskId !== 'task-9' ||
      parsed.outboxId !== null
    ) {
      throw new Error(`unexpected automation task deep link: ${url} / ${JSON.stringify(parsed)}`);
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   failed outbox 导航必须定位到 automation 面板中的 outbox 条目。
   *
   * Code Logic（这个测试做什么）:
   *   构造/解析 automation+outboxId deep link。
   */
  test('parses and builds automation outbox deep links', () => {
    const url = buildWorkbenchDeepLink({
      projectId: 'remote:device-a:p',
      worktreeId: null,
      sessionId: null,
      view: 'automation',
      taskId: null,
      outboxId: 'outbox-3',
    });
    const parsed = parseWorkbenchDeepLink(url.replace('/workbench', ''));

    if (parsed.view !== 'automation' || parsed.outboxId !== 'outbox-3' || parsed.taskId !== null) {
      throw new Error(`unexpected automation outbox deep link: ${url} / ${JSON.stringify(parsed)}`);
    }
    if (!url.includes('outboxId=outbox-3') || !url.includes('view=automation')) {
      throw new Error(`expected outbox automation query, got ${url}`);
    }
  });
});
