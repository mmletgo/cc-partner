import 'dart:io';

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

  /// push/pull/commit/merge 的动作脚本：默认成功 envelope，可改抛错或返回 unknown。
  Object? Function(String kind)? mutationScript;
  final List<String> mutationCalls = [];
  List<Map<String, dynamic>> allProjects = const [];

  Object? _defaultMutation(String kind) {
    mutationCalls.add(kind);
    return {
      'kind': 'succeeded',
      'clientOperationId': 'server-op',
      'value': const <String, dynamic>{},
    };
  }

  Object? _dispatch(String kind) {
    final script = mutationScript;
    if (script != null) {
      return script(kind);
    }
    return _defaultMutation(kind);
  }

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

  @override
  Future<Map<String, dynamic>> commit({
    required String worktreeId,
    required String clientOperationId,
    String? message,
  }) async {
    return Map<String, dynamic>.from(_dispatch('commit') as Map);
  }

  @override
  Future<Map<String, dynamic>> pull({
    required String projectId,
    required String worktreeId,
    String? clientOperationId,
  }) async {
    return Map<String, dynamic>.from(_dispatch('pull') as Map);
  }

  @override
  Future<Map<String, dynamic>> push({
    required String projectId,
    required String worktreeId,
    String? clientOperationId,
  }) async {
    return Map<String, dynamic>.from(_dispatch('push') as Map);
  }

  @override
  Future<Map<String, dynamic>> merge({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) async {
    return Map<String, dynamic>.from(_dispatch('merge') as Map);
  }

  @override
  Future<List<Map<String, dynamic>>> listAllProjects() async => allProjects;
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

  /// 等 SnackBar 自动消失，避免测试结束时残留 Timer。
  Future<void> flushSnackbars(WidgetTester tester) async {
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  }

  _FakeGitClient seed({
    bool featCanPush = true,
    bool mainCanPush = true,
  }) =>
      _FakeGitClient()
        ..trees = [
          {
            'id': 'wt-main',
            'name': 'main',
            'branch': 'main',
            'isMain': true,
            'canCollectMerge': true,
            'homeBranch': 'main',
            'collectibleBranches': ['feat/app'],
            'status': {
              'branch': 'main',
              'changed': 0,
              'ahead': 0,
              'behind': 0,
              'conflicts': 0,
              'clean': true,
              'canPush': mainCanPush,
            },
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
              'canPush': featCanPush,
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
        ];

  testWidgets('状态卡展示分支/状态/领先落后/推送状态，动作行与提交历史渲染', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    expect(find.text('feat/app'), findsWidgets);
    expect(find.text('有改动（2 个文件）'), findsOneWidget);
    expect(find.text('领先 1 · 落后 0'), findsOneWidget);
    expect(find.text('可推送'), findsWidgets);
    expect(find.byKey(const Key('git-action-commit')), findsOneWidget);
    expect(find.byKey(const Key('git-action-push')), findsOneWidget);
    expect(find.byKey(const Key('git-action-merge')), findsOneWidget);
    expect(find.byKey(const Key('git-action-sync')), findsOneWidget);
    expect(find.text('最近提交'), findsOneWidget);
    expect(find.text('fix: mobile git history'), findsOneWidget);
    expect(find.text('a1b2c3d'), findsOneWidget);
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

  testWidgets('canPush=false 时推送按钮禁用，canPush=true 可点', (tester) async {
    final git = seed(featCanPush: false);
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 当前选中 wt-1（canPush=false）→ 推送禁用。
    final pushButton = tester.widget<OutlinedButton>(
      find.byKey(const Key('git-action-push')),
    );
    expect(pushButton.onPressed, isNull);
  });

  testWidgets('提交成功提示 2.5 秒自动消失', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 提交说明框即确认步骤；留空由 AI 生成。
    await tester.tap(find.byKey(const Key('git-action-commit')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '提交').last);
    await tester.pump();
    await tester.pump();
    expect(find.text('提交成功'), findsOneWidget);

    // 2.5s 后自动消失：先让入场动画完成（计时器在完成后的 build 注册），再跨过计时点。
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pump(const Duration(milliseconds: 2600));
    await tester.pumpAndSettle();
    expect(find.text('提交成功'), findsNothing);
  });

  testWidgets('功能 worktree 合并确认文案区分；取消不执行', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('git-action-merge')));
    await tester.pumpAndSettle();

    expect(find.text('确认合并'), findsOneWidget);
    expect(find.text('确定把「feat」合并到主工作区？'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(find.text('确认合并'), findsNothing);
    expect(git.mutationCalls, isEmpty);
  });

  testWidgets('主工作区 collect-merge 确认文案列出可收集分支', (tester) async {
    final git = seed();
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 切到主工作区卡片再点合并。
    await tester.tap(find.byKey(const Key('git-tree-wt-main')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('git-action-merge')));
    await tester.pumpAndSettle();

    expect(
      find.text('确定把本工作区的 1 条分支（feat/app）合并到「main」，并切回该主分支？'),
      findsOneWidget,
    );
    await tester.tap(find.text('确认'));
    await tester.pumpAndSettle();
    expect(git.mutationCalls, contains('merge'));
  });

  testWidgets('动作返回 unknown：锁定全部动作 + 重新对账确认已生效', (tester) async {
    final git = seed();
    git.mutationScript = (kind) {
      git.mutationCalls.add(kind);
      return {
        'kind': 'unknown',
        'clientOperationId': 'server-op-1',
      };
    };

    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('git-action-push')));
    await tester.pumpAndSettle();

    // unknown 相位：横幅出现 + 全部动作禁用。
    expect(find.byKey(const Key('git-unknown-banner')), findsOneWidget);
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('git-action-commit'))).onPressed,
      isNull,
    );
    expect(
      tester.widget<OutlinedButton>(find.byKey(const Key('git-action-push'))).onPressed,
      isNull,
    );

    // 重新对账：ledger 不可达 + push 无 authority → 保持 unknown（同一 op id，不盲重放）。
    final callsBefore = git.mutationCalls.length;
    await tester.tap(find.byKey(const Key('git-retry-reconcile')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('git-unknown-banner')), findsOneWidget);
    // 对账只查 ledger/列表，不重放 mutation。
    expect(git.mutationCalls.length, callsBefore);
  });

  testWidgets('传输异常（SocketException）进入 unknown 相位并锁定', (tester) async {
    final git = seed();
    git.mutationScript = (kind) {
      git.mutationCalls.add(kind);
      throw const SocketException('network unreachable');
    };
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('git-action-push')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('git-unknown-banner')), findsOneWidget);
    expect(find.textContaining('操作结果未知'), findsNWidgets(2));
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('git-action-commit'))).onPressed,
      isNull,
    );
    // SnackBar 没有「推送失败」——传输异常不算确定失败。
    expect(find.textContaining('推送失败'), findsNothing);
  });

  testWidgets('同步按钮：门控满足时推送主分支并逐兄弟拉取，摘要 SnackBar', (tester) async {
    final git = seed()
      ..allProjects = [
        {
          'id': 'p1',
          'deviceId': 'dev-a',
          'gitRemoteFingerprint': 'fp-1',
        },
        {
          'id': 'p2',
          'deviceId': 'dev-b',
          'deviceName': '书房',
          'gitRemoteFingerprint': 'fp-1',
        },
      ];
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    // 门控满足：主分支 canPush 且有一个兄弟设备。
    final syncButton = tester.widget<OutlinedButton>(
      find.byKey(const Key('git-action-sync')),
    );
    expect(syncButton.onPressed, isNotNull);

    await tester.tap(find.byKey(const Key('git-action-sync')));
    await tester.pumpAndSettle();

    // push 主分支 + pull 兄弟主 worktree 都发生。
    expect(git.mutationCalls, contains('push'));
    expect(git.mutationCalls, contains('pull'));
    expect(find.textContaining('已推送主分支'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('无兄弟设备时同步按钮禁用', (tester) async {
    final git = seed()
      ..allProjects = [
        {'id': 'p1', 'deviceId': 'dev-a', 'gitRemoteFingerprint': 'fp-1'},
      ];
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    expect(
      tester.widget<OutlinedButton>(find.byKey(const Key('git-action-sync'))).onPressed,
      isNull,
    );
  });
}
