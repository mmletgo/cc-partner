import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/attention/client.dart';
import 'package:cc_partner_mobile/attention/filter.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/attention_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 假 Attention 客户端：内存中维护条目与标记调用记录，不发网络请求。
class _FakeAttentionClient extends AttentionClient {
  _FakeAttentionClient(this._items) : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<AttentionItem> _items;
  final List<List<String>> readCalls = [];
  final List<List<String>> unreadCalls = [];
  int allReadCalls = 0;

  @override
  Future<List<AttentionItem>> listVisible() async => List.of(_items);

  @override
  Future<List<AttentionItem>> markRead(List<String> itemIds) async {
    readCalls.add(itemIds);
    return _withReadState(itemIds, read: true);
  }

  @override
  Future<List<AttentionItem>> markUnread(List<String> itemIds) async {
    unreadCalls.add(itemIds);
    return _withReadState(itemIds, read: false);
  }

  @override
  Future<List<AttentionItem>> markAllRead() async {
    allReadCalls++;
    return _withReadState(_items.map((e) => e.id).toList(), read: true);
  }

  /// 与真实后端一致：返回完整快照，仅被标记条目更新 readAt。
  List<AttentionItem> _withReadState(List<String> ids, {required bool read}) {
    _items = [
      for (final item in _items)
        ids.contains(item.id)
            ? AttentionItem(
                id: item.id,
                sourceKind: item.sourceKind,
                targetKind: item.targetKind,
                title: item.title,
                projectId: item.projectId,
                sessionId: item.sessionId,
                worktreeId: item.worktreeId,
                category: item.category,
                summary: item.summary,
                updatedAt: item.updatedAt,
                readAt: read ? '2026-09-19T10:00:00Z' : null,
                freshness: item.freshness,
                projectName: item.projectName,
                deviceName: item.deviceName,
              )
            : item,
    ];
    return List.of(_items);
  }
}

/// 假 Projects 客户端：返回固定列表。
class _FakeProjectsClient extends ProjectsClient {
  _FakeProjectsClient(this.items) : super(LanHttpClient(), 'http://127.0.0.1:1');

  final List<ProjectSummary> items;

  @override
  Future<List<ProjectSummary>> listRecent() async => items;
}

AttentionItem _item(
  String id, {
  String? readAt,
  String? updatedAt,
  String targetKind = 'agentSession',
}) =>
    AttentionItem(
      id: id,
      category: 'blocked',
      sourceKind: 'agentNeedsInput',
      title: '标题 $id',
      summary: '摘要 $id',
      updatedAt: updatedAt ?? DateTime.now().toIso8601String(),
      freshness: 'live',
      readAt: readAt,
      projectId: 'p1',
      sessionId: 'tmux-1',
      worktreeId: 'wt-1',
      targetKind: targetKind,
      projectName: 'demo',
      deviceName: 'Hans Mac',
    );

void main() {
  final earlierIso = DateTime.now().subtract(const Duration(days: 2)).toIso8601String();

  Future<List<(ProjectSummary, String, String?)>> pumpPage(
    WidgetTester tester, {
    _FakeProjectsClient? projectsClient,
  }) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    final client = _FakeAttentionClient([
      _item('unread-1'),
      _item('read-1', readAt: '2026-09-19T09:00:00Z'),
      _item('earlier-1', updatedAt: earlierIso),
    ]);
    final navigated = <(ProjectSummary, String, String?)>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: AttentionPage(
            book: book,
            http: LanHttpClient(),
            attentionClient: client,
            projectsClient: projectsClient,
            onNavigate: (project, panel, sessionId) => navigated.add((project, panel, sessionId)),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    return navigated;
  }

  testWidgets('renders unread/read distinction with category and meta', (tester) async {
    await pumpPage(tester);
    expect(find.text('标题 unread-1'), findsOneWidget);
    expect(find.text('标题 earlier-1'), findsNothing); // 默认只显示今天
    expect(find.text('显示更早 1 条'), findsOneWidget);
    // 未读给「标为已读」，已读给「标为未读」。
    expect(find.text('标为已读'), findsOneWidget);
    expect(find.text('标为未读'), findsOneWidget);
    // 未读标题加粗。
    final unreadTitle = tester.widget<Text>(find.text('标题 unread-1'));
    expect(unreadTitle.style?.fontWeight, FontWeight.w600);
    // meta 含分类、来源与项目·设备。
    expect(find.text('阻塞 · agentNeedsInput'), findsNWidgets(2));
    expect(find.textContaining('demo · Hans Mac'), findsNWidgets(2));
  });

  testWidgets('toggle button marks read locally without full reload', (tester) async {
    await pumpPage(tester);
    await tester.tap(find.byKey(const Key('attention-toggle-read-unread-1')));
    await tester.pumpAndSettle();
    // 列表仍在（没有整页重载成 loading），且未读按钮消失。
    expect(find.text('标题 unread-1'), findsOneWidget);
    expect(find.text('标为已读'), findsNothing);
    expect(find.text('标为未读'), findsNWidgets(2));
    // 再点一次翻回未读。
    await tester.tap(find.byKey(const Key('attention-toggle-read-unread-1')));
    await tester.pumpAndSettle();
    expect(find.text('标为已读'), findsOneWidget);
  });

  testWidgets('tapping an unread item marks read first then navigates', (tester) async {
    final navigated = await pumpPage(
      tester,
      projectsClient: _FakeProjectsClient([
        const ProjectSummary(id: 'p1', name: 'demo'),
      ]),
    );
    await tester.tap(find.text('标题 unread-1'));
    await tester.pumpAndSettle();
    expect(navigated, hasLength(1));
    expect(navigated.first.$1.id, 'p1');
    expect(navigated.first.$2, 'terminal');
    expect(navigated.first.$3, 'tmux-1');
    // 未读已在导航前标记为已读。
    expect(find.text('标为已读'), findsNothing);
  });

  testWidgets('missing project shows a SnackBar instead of silent failure', (tester) async {
    await pumpPage(
      tester,
      projectsClient: _FakeProjectsClient([
        const ProjectSummary(id: 'p-other', name: 'other'),
      ]),
    );
    await tester.tap(find.text('标题 read-1'));
    await tester.pumpAndSettle();
    expect(find.text('该项目已不在列表中'), findsOneWidget);
  });

  testWidgets('day filter toggle shows and hides earlier items', (tester) async {
    await pumpPage(tester);
    expect(find.text('标题 earlier-1'), findsNothing);
    await tester.tap(find.byKey(const Key('attention-day-filter')));
    await tester.pumpAndSettle();
    expect(find.text('收起更早条目'), findsOneWidget);
    expect(find.text('标题 earlier-1'), findsOneWidget);
    await tester.tap(find.byKey(const Key('attention-day-filter')));
    await tester.pumpAndSettle();
    expect(find.text('标题 earlier-1'), findsNothing);
  });

  testWidgets('mark-all-read posts once and disables when nothing unread', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    final client = _FakeAttentionClient([
      _item('unread-1'),
      _item('read-1', readAt: '2026-09-19T09:00:00Z'),
    ]);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: AttentionPage(
            book: book,
            http: LanHttpClient(),
            attentionClient: client,
            onNavigate: (_, __, ___) {},
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    final button = find.byKey(const Key('attention-mark-all-read'));
    expect(tester.widget<TextButton>(button).onPressed, isNotNull);
    await tester.tap(button);
    await tester.pumpAndSettle();
    expect(client.allReadCalls, 1);
    // 全部已读后按钮禁用。
    expect(tester.widget<TextButton>(button).onPressed, isNull);
    expect(find.text('标为已读'), findsNothing);
  });
}
