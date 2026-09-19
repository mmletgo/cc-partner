import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/git_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 注入 GitPage 的假 GitClient：不发网络，按测试脚本返回/抛错。
class _FakeGitClient extends GitClient {
  _FakeGitClient() : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<Map<String, dynamic>> trees = const [];
  List<WorkbenchGitCommit> commitSeed = const [];
  Object? commitsError;
  int commitsRequestCount = 0;

  @override
  Future<Map<String, dynamic>> listWorktrees(
    String projectId, {
    bool includeGitStatus = false,
  }) async {
    return {'ok': true, 'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)]};
  }

  @override
  Future<List<WorkbenchGitCommit>> commits(
    String projectId, {
    String? worktreeId,
    int limit = 30,
  }) async {
    commitsRequestCount += 1;
    if (commitsError != null) {
      throw commitsError!;
    }
    return commitSeed;
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
        body: GitPage(
          book: addressBook,
          http: LanHttpClient(),
          project: const ProjectSummary(id: 'p1', name: 'demo'),
          worktreeId: 'wt-1',
          gitClient: git,
        ),
      ),
    );
  }

  _FakeGitClient seed() => _FakeGitClient()
    ..trees = const [
      {
        'id': 'wt-main',
        'name': 'main',
        'branch': 'main',
        'isMain': true,
      },
      {
        'id': 'wt-1',
        'name': 'feat',
        'branch': 'feat/app',
        'isMain': false,
        'status': {
          'branch': 'feat/app',
          'changed': 2,
          'ahead': 1,
          'behind': 0,
          'conflicts': 0,
          'clean': false,
          'canPush': true,
        },
      },
    ]
    ..commitSeed = const [
      WorkbenchGitCommit(
        hash: 'a1b2c3d4e5f6',
        shortHash: 'a1b2c3d',
        authorName: '韩梅梅',
        authoredAt: '2026-09-19T10:30:00Z',
        summary: 'fix: mobile git history',
        refs: [WorkbenchGitRef(name: 'feat/app', kind: 'local', isHead: true)],
      ),
      WorkbenchGitCommit(hash: 'fff000111222', shortHash: 'fff0001', summary: 'init'),
    ];

  testWidgets('状态卡展示分支、工作区状态与领先/落后，提交历史渲染摘要与 refs', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 状态卡（当前选中 wt-1）
    expect(find.text('feat/app'), findsWidgets);
    expect(find.text('有改动（2 个文件）'), findsOneWidget);
    expect(find.text('领先 1 · 落后 0'), findsOneWidget);
    // worktree 卡片 + 动作菜单入口仍在
    expect(find.text('main'), findsWidgets);
    // 提交历史
    expect(find.text('最近提交'), findsOneWidget);
    expect(find.text('fix: mobile git history'), findsOneWidget);
    expect(find.text('a1b2c3d'), findsOneWidget);
    expect(find.textContaining('韩梅梅'), findsWidgets);
    expect(find.text('init'), findsOneWidget);
    expect(find.text('feat/app'), findsWidgets);
  });

  testWidgets('提交历史加载失败展示错误卡并可重试', (tester) async {
    final git = seed()..commitsError = Exception('boom');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    expect(find.text('提交历史加载失败'), findsOneWidget);
    expect(git.commitsRequestCount, 1);

    git.commitsError = null;
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();

    expect(find.text('提交历史加载失败'), findsNothing);
    expect(find.text('fix: mobile git history'), findsOneWidget);
    expect(git.commitsRequestCount, 2);
  });

  testWidgets('合并前弹出确认框，取消不执行', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byIcon(Icons.more_vert).last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('合并'));
    await tester.pumpAndSettle();

    expect(find.text('确认合并'), findsOneWidget);
    expect(find.textContaining('「feat」'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(find.text('确认合并'), findsNothing);
  });
}
