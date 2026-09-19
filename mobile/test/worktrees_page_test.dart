import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/worktrees_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 注入 WorktreesPage 的假 GitClient：不发网络，按测试脚本返回/抛错。
class _FakeGitClient extends GitClient {
  _FakeGitClient() : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<Map<String, dynamic>> trees = const [];
  Object? createError;
  Object? removeError;
  int removeCount = 0;
  List<String> removedIds = [];

  @override
  Future<Map<String, dynamic>> listWorktrees(
    String projectId, {
    bool includeGitStatus = false,
  }) async {
    return {'ok': true, 'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)]};
  }

  @override
  Future<Map<String, dynamic>> create({
    required String projectId,
    required String branchName,
    String? baseBranch,
  }) async {
    if (createError != null) {
      throw createError!;
    }
    trees = [
      ...trees,
      {'id': 'wt-new', 'name': branchName, 'branch': branchName, 'isMain': false},
    ];
    return {'id': 'wt-new', 'name': branchName, 'branch': branchName, 'isMain': false};
  }

  @override
  Future<Map<String, dynamic>> remove({
    required String worktreeId,
    required String clientOperationId,
    bool force = false,
  }) async {
    if (removeError != null) {
      throw removeError!;
    }
    removeCount += 1;
    removedIds.add(worktreeId);
    trees = trees.where((tree) => tree['id'] != worktreeId).toList();
    return {'kind': 'succeeded'};
  }
}

void main() {
  Future<AddressBook> book() async {
    final addressBook = AddressBook(store: MemoryAddressBookStore());
    await addressBook.addFromInput(
      '127.0.0.1:62116',
      probe: (_) async => throw Exception('skip'),
      forceIfUnreachable: true,
    );
    return addressBook;
  }

  Widget wrap(AddressBook addressBook, GitClient git) {
    return MaterialApp(
      home: Scaffold(
        body: WorktreesPage(
          book: addressBook,
          http: LanHttpClient(),
          project: const ProjectSummary(id: 'p1', name: 'demo'),
          activeId: 'wt-main',
          onSelect: (_) {},
          gitClient: git,
        ),
      ),
    );
  }

  /// 等 SnackBar 自动消失，避免测试结束时残留 Timer。
  Future<void> flushSnackbars(WidgetTester tester) async {
    await tester.pump(const Duration(seconds: 4));
    await tester.pumpAndSettle();
  }

  _FakeGitClient seed() => _FakeGitClient()
    ..trees = const [
      {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true},
      {'id': 'wt-1', 'name': 'feat', 'branch': 'feat/app', 'isMain': false},
    ];

  testWidgets('删除前弹出确认框，取消时不删除', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);

    await tester.tap(find.byTooltip('删除'));
    await tester.pumpAndSettle();
    expect(find.text('删除 worktree'), findsOneWidget);
    expect(find.textContaining('确定删除 worktree「feat」吗？'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(find.text('删除 worktree'), findsNothing);
    expect(git.removeCount, 0);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
  });

  testWidgets('确认后删除并刷新列表与提示', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('删除'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '删除'));
    await tester.pumpAndSettle();

    expect(git.removeCount, 1);
    expect(git.removedIds, ['wt-1']);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsNothing);
    expect(find.textContaining('已删除 worktree「feat」'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('删除失败通过 SnackBar 上屏且列表保留', (tester) async {
    final git = seed()..removeError = Exception('device offline');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('删除'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '删除'));
    await tester.pumpAndSettle();

    expect(find.textContaining('删除失败'), findsOneWidget);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('创建失败通过 SnackBar 上屏且输入保留', (tester) async {
    final git = seed()..createError = Exception('branch exists');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField), 'feat/x');
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(find.textContaining('创建失败'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'feat/x'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('创建成功刷新列表并提示', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField), 'feat/x');
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('worktree-item-wt-new')), findsOneWidget);
    expect(find.textContaining('已创建 worktree「feat/x」'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'feat/x'), findsNothing);
    await flushSnackbars(tester);
  });
}
