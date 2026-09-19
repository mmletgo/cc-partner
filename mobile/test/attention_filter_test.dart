import 'package:cc_partner_mobile/attention/filter.dart';
import 'package:test/test.dart';

AttentionItem itemFromJson(Map<String, dynamic> json) => AttentionItem.fromJson(json);

void main() {
  test('hides tmux/dependency Inbox items', () {
    final items = [
      const AttentionItem(
        id: 'workbench:dependency:tmux',
        sourceKind: 'workbenchDependency',
        targetKind: 'settings',
      ),
      const AttentionItem(
        id: 'agent-1',
        sourceKind: 'agentNeedsInput',
        targetKind: 'agentSession',
        projectId: 'p1',
        sessionId: 's1',
      ),
    ];
    final visible = filterMobileInboxAttentionItems(items);
    expect(visible, hasLength(1));
    expect(visible.single.id, 'agent-1');
    expect(isMobileHiddenAttentionItem(items.first), isTrue);
  });

  test('navigate-only maps agentNeedsInput to the terminal panel', () {
    const item = AttentionItem(
      id: 'agent-1',
      sourceKind: 'agentNeedsInput',
      targetKind: 'agentSession',
      projectId: 'p1',
      sessionId: 's1',
    );
    final nav = navigateAttention(item);
    expect(nav.panel, 'terminal');
    expect(nav.sessionId, 's1');
  });

  test('orchestrator Inbox targets navigate to automation, not Inbox itself', () {
    const task = AttentionItem(
      id: 'task-1',
      sourceKind: 'orchestrator',
      targetKind: 'orchestratorTask',
      projectId: 'p1',
    );
    const outbox = AttentionItem(
      id: 'outbox-1',
      sourceKind: 'orchestrator',
      targetKind: 'remoteOutbox',
      projectId: 'p1',
    );
    const experiment = AttentionItem(
      id: 'exp-1',
      sourceKind: 'orchestrator',
      targetKind: 'experiment',
      projectId: 'p1',
    );
    expect(navigateAttention(task).panel, 'automation');
    expect(navigateAttention(outbox).panel, 'automation');
    expect(navigateAttention(experiment).panel, 'automation');
  });

  test('isUnread follows readAt like the web client', () {
    const unread = AttentionItem(id: 'a', sourceKind: 's', targetKind: 't');
    const read = AttentionItem(id: 'b', sourceKind: 's', targetKind: 't', readAt: '2026-09-19T08:00:00Z');
    const blankReadAt = AttentionItem(id: 'c', sourceKind: 's', targetKind: 't', readAt: '');
    expect(unread.isUnread, isTrue);
    expect(read.isUnread, isFalse);
    expect(blankReadAt.isUnread, isTrue);
  });

  test('fromJson parses read/updatedAt/category and project·device names', () {
    final item = itemFromJson({
      'id': 'agent-1',
      'category': 'blocked',
      'sourceKind': 'agentNeedsInput',
      'title': '需要输入',
      'summary': '等待确认',
      'updatedAt': '2026-09-19T01:02:03Z',
      'freshness': 'live',
      'readAt': '2026-09-19T09:00:00Z',
      'project': {'id': 'p1', 'name': 'demo', 'kind': 'local'},
      'device': {'id': 'pc-a', 'name': 'Hans Mac'},
      'target': {
        'kind': 'agentSession',
        'projectId': 'p1',
        'terminalSessionId': 'tmux-1',
        'worktreeId': 'wt-1',
      },
    });
    expect(item.category, 'blocked');
    expect(item.summary, '等待确认');
    expect(item.updatedAt, '2026-09-19T01:02:03Z');
    expect(item.readAt, '2026-09-19T09:00:00Z');
    expect(item.isUnread, isFalse);
    expect(item.freshness, 'live');
    expect(item.projectName, 'demo');
    expect(item.deviceName, 'Hans Mac');
    expect(item.projectId, 'p1');
    expect(item.sessionId, 'tmux-1');
    expect(item.worktreeId, 'wt-1');
  });

  test('fromJson keeps old payloads without read/updatedAt fields working', () {
    final item = itemFromJson({
      'id': 'legacy-1',
      'sourceKind': 'orchestratorHumanReview',
      'target': {'kind': 'orchestratorTask', 'projectId': 'p1', 'taskId': 't1'},
    });
    expect(item.readAt, isNull);
    expect(item.updatedAt, isNull);
    expect(item.isUnread, isTrue);
    expect(item.targetKind, 'orchestratorTask');
  });

  test('partitionAttentionItemsByLocalDay splits today vs earlier in order', () {
    final now = DateTime(2026, 9, 19, 15, 30);
    final todayItem = const AttentionItem(
      id: 'today',
      sourceKind: 's',
      targetKind: 't',
      updatedAt: '2026-09-19T08:00:00',
    );
    final earlierItem = const AttentionItem(
      id: 'earlier',
      sourceKind: 's',
      targetKind: 't',
      updatedAt: '2026-09-18T23:59:00',
    );
    final partition = partitionAttentionItemsByLocalDay([earlierItem, todayItem], now);
    expect(partition.today.map((e) => e.id), ['today']);
    expect(partition.earlier.map((e) => e.id), ['earlier']);
  });

  test('partition treats invalid/missing updatedAt as today (fail-open)', () {
    final now = DateTime(2026, 9, 19, 15, 30);
    final badIso = const AttentionItem(
      id: 'bad',
      sourceKind: 's',
      targetKind: 't',
      updatedAt: 'not-a-date',
    );
    final missing = const AttentionItem(id: 'missing', sourceKind: 's', targetKind: 't');
    final partition = partitionAttentionItemsByLocalDay([badIso, missing], now);
    expect(partition.today, hasLength(2));
    expect(partition.earlier, isEmpty);
  });

  test('partition compares in local day, not UTC day', () {
    final now = DateTime(2026, 9, 19, 12);
    // 用本地 00:30 换算成 UTC ISO：无论测试机时区如何，它解析回本地都落在 9-19。
    final iso = DateTime(2026, 9, 19, 0, 30).toUtc().toIso8601String();
    final utcLateItem = AttentionItem(
      id: 'utc-late',
      sourceKind: 's',
      targetKind: 't',
      updatedAt: iso,
    );
    final partition = partitionAttentionItemsByLocalDay([utcLateItem], now);
    expect(partition.today.map((e) => e.id), ['utc-late']);
  });

  test('count helpers expose total unread and today-only badge口径', () {
    final now = DateTime(2026, 9, 19, 15, 30);
    final items = [
      const AttentionItem(
        id: 'today-unread',
        sourceKind: 's',
        targetKind: 't',
        updatedAt: '2026-09-19T08:00:00',
      ),
      const AttentionItem(
        id: 'today-read',
        sourceKind: 's',
        targetKind: 't',
        updatedAt: '2026-09-19T09:00:00',
        readAt: '2026-09-19T09:30:00',
      ),
      const AttentionItem(
        id: 'earlier-unread',
        sourceKind: 's',
        targetKind: 't',
        updatedAt: '2026-09-17T09:00:00',
      ),
    ];
    expect(countUnreadAttentionItems(items), 2);
    // 徽章与默认「今天」列表同口径：更早的未读不进徽章。
    expect(countTodayUnreadAttentionItems(items, now), 1);
  });

  test('attentionCategoryLabel maps known categories and hides unknown', () {
    // 文案与 web i18n 权威一致（web MobileAttentionPanel.test.tsx 锁定「需要你的决定」）。
    expect(attentionCategoryLabel('decision'), '需要你的决定');
    expect(attentionCategoryLabel('blocked'), '运行受阻');
    expect(attentionCategoryLabel('environment'), '环境受阻');
    expect(attentionCategoryLabel('unknown'), isNull);
    expect(attentionCategoryLabel(null), isNull);
  });
}
