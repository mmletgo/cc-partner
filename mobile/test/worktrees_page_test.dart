import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/git/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/sessions/client.dart';
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
  List<String> createdBranchNames = [];
  List<bool> listCalls = [];

  @override
  Future<Map<String, dynamic>> listWorktrees(
    String projectId, {
    bool includeGitStatus = false,
  }) async {
    listCalls.add(includeGitStatus);
    return {
      'ok': true,
      'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)],
    };
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
    createdBranchNames.add(branchName);
    final created = {
      'id': 'wt-new',
      'name': branchName,
      'branch': branchName,
      'isMain': false,
      'path': '/repo/.worktrees/$branchName',
    };
    trees = [...trees, Map<String, dynamic>.from(created)];
    return Map<String, dynamic>.from(created);
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

  Widget wrap(
    AddressBook addressBook,
    GitClient git, {
    ValueChanged<Map<String, dynamic>>? onSelect,
    Future<SessionSummary> Function(String projectId, String worktreeId)? onCreateSession,
  }) {
    return MaterialApp(
      home: Scaffold(
        body: WorktreesPage(
          book: addressBook,
          http: LanHttpClient(),
          project: const ProjectSummary(id: 'p1', name: 'demo'),
          activeId: 'wt-main',
          onSelect: onSelect ?? (_) {},
          gitClient: git,
          onCreateSession: onCreateSession,
        ),
      ),
    );
  }

  /// 等 SnackBar 自动消失，避免测试结束时残留 Timer。
  Future<void> flushSnackbars(WidgetTester tester) async {
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  }

  _FakeGitClient seed() => _FakeGitClient()
    ..trees = const [
      {
        'id': 'wt-main',
        'name': 'main',
        'branch': 'main',
        'isMain': true,
        'path': '/repo',
        'status': {
          'branch': 'main',
          'changed': 0,
          'ahead': 0,
          'behind': 0,
          'conflicts': 0,
          'clean': true,
          'canPush': true,
        },
      },
      {
        'id': 'wt-1',
        'name': 'feat',
        'branch': 'feat/app',
        'isMain': false,
        'path': '/repo/.worktrees/feat-app',
        'status': {
          'branch': 'feat/app',
          'changed': 2,
          'ahead': 1,
          'behind': 3,
          'conflicts': 1,
          'clean': false,
          'canPush': false,
        },
      },
    ];

  testWidgets('卡片展示主/linked、分支、路径与状态/同步/可推送徽章', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('worktree-item-wt-main')), findsOneWidget);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
    expect(find.text('主工作区'), findsOneWidget);
    expect(find.text('worktree'), findsOneWidget);
    expect(find.text('feat/app'), findsWidgets);
    expect(find.text('/repo/.worktrees/feat-app'), findsOneWidget);
    expect(find.text('1 处冲突'), findsOneWidget);
    expect(find.text('领先 1 / 落后 3'), findsOneWidget);
    expect(find.text('不可推送'), findsOneWidget);
    expect(find.text('干净'), findsOneWidget);
  });

  testWidgets('列表请求带 includeGitStatus=true（状态徽章数据源）', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();
    expect(git.listCalls, isNotEmpty);
    expect(git.listCalls.every((flag) => flag), isTrue);
  });

  testWidgets('移除前弹出确认框，取消时不删除', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    expect(find.text('移除 worktree'), findsOneWidget);
    expect(find.textContaining('未推送的提交可能丢失'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(find.text('移除 worktree'), findsNothing);
    expect(git.removeCount, 0);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
  });

  testWidgets('确认后移除并刷新列表与提示', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    expect(git.removeCount, 1);
    expect(git.removedIds, ['wt-1']);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsNothing);
    expect(find.textContaining('已移除 worktree「feat」'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('移除失败通过 SnackBar 上屏且列表保留', (tester) async {
    final git = seed()..removeError = Exception('device offline');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    expect(find.textContaining('移除失败'), findsOneWidget);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('后缀为空时创建按钮禁用；填写后按 前缀/后缀 组合创建', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 空后缀：创建禁用。
    expect(
      tester.widget<FilledButton>(find.widgetWithText(FilledButton, '创建')).onPressed,
      isNull,
    );

    await tester.enterText(find.byKey(const Key('worktree-suffix-input')), 'my-task');
    await tester.pumpAndSettle();
    expect(
      tester.widget<FilledButton>(find.widgetWithText(FilledButton, '创建')).onPressed,
      isNotNull,
    );

    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();
    expect(git.createdBranchNames, ['feature/my-task']);
    await flushSnackbars(tester);
  });

  testWidgets('切换 prefix 下拉后按组合创建', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-prefix-select')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('fix').last);
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('worktree-suffix-input')), 'login-crash');
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(git.createdBranchNames, ['fix/login-crash']);
    await flushSnackbars(tester);
  });

  testWidgets('创建成功后自动开绑定终端窗口并回调 onSelect', (tester) async {
    final git = seed();
    final createdSessions = <String>[];
    final selected = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        onSelect: (tree) => selected.add(tree['id'] as String),
        onCreateSession: (projectId, worktreeId) async {
          createdSessions.add('$projectId/$worktreeId');
          return SessionSummary(
            id: 'tmux-1',
            projectId: projectId,
            name: 'w',
            status: 'running',
            worktreeId: worktreeId,
          );
        },
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('worktree-suffix-input')), 'auto-term');
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(createdSessions, ['p1/wt-new']);
    expect(selected, ['wt-new']);
    expect(find.textContaining('已创建 worktree「feature/auto-term」'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'auto-term'), findsNothing);
    await flushSnackbars(tester);
  });

  testWidgets('终端窗口创建失败时保留 worktree、提示且仍回调 onSelect', (tester) async {
    final git = seed();
    final selected = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        onSelect: (tree) => selected.add(tree['id'] as String),
        onCreateSession: (projectId, worktreeId) async => throw Exception('pty boom'),
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('worktree-suffix-input')), 'term-fail');
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(find.textContaining('终端窗口创建失败'), findsOneWidget);
    // worktree 已保留并切换（不回滚）。
    expect(find.byKey(const Key('worktree-item-wt-new')), findsOneWidget);
    expect(selected, ['wt-new']);
    await flushSnackbars(tester);
  });

  testWidgets('创建失败通过 SnackBar 上屏且输入保留', (tester) async {
    final git = seed()..createError = Exception('branch exists');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('worktree-suffix-input')), 'feat/x');
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(find.textContaining('创建失败'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'feat/x'), findsOneWidget);
    await flushSnackbars(tester);
  });
}
