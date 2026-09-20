import 'dart:io';

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

  /// remove/merge 的 envelope 脚本；null 走默认 succeeded。
  Map<String, dynamic>? Function(String kind)? mutationEnvelopeScript;

  /// mutation-operation（ledger 对账）返回脚本；null 表示后端无记录。
  Map<String, dynamic>? Function(String operationId)? ledgerScript;

  int removeCount = 0;
  List<String> removedIds = [];
  List<String> createdBranchNames = [];
  List<bool> listCalls = [];
  final List<String> mutationOperationIds = [];
  final List<String> removeOperationIds = [];
  final List<String> mergeOperationIds = [];
  int mergeCount = 0;

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
    // 请求已发出即计数（即使传输层抛错，也用于断言「没有盲重放」）。
    removeCount += 1;
    removedIds.add(worktreeId);
    removeOperationIds.add(clientOperationId);
    if (removeError != null) {
      throw removeError!;
    }
    final scripted = mutationEnvelopeScript?.call('remove');
    if (scripted == null) {
      // 默认成功模拟：源 worktree 从权威列表消失。
      trees = trees.where((tree) => tree['id'] != worktreeId).toList();
      return {'kind': 'succeeded'};
    }
    // 权威列表副作用由测试脚本决定（模拟服务端「实际已删/未删」而响应不确定）。
    onRemoveApplied?.call(worktreeId);
    // unknown envelope 回显请求 id（后端语义），保证对账同 id。
    if (scripted['kind'] == 'unknown') {
      return {'kind': 'unknown', 'clientOperationId': clientOperationId};
    }
    return scripted;
  }

  @override
  Future<Map<String, dynamic>> merge({
    required String projectId,
    required String worktreeId,
    required String clientOperationId,
  }) async {
    mergeCount += 1;
    mergeOperationIds.add(clientOperationId);
    final scripted = mutationEnvelopeScript?.call('merge');
    if (scripted == null) {
      // 默认成功模拟：源 worktree 从权威列表消失。
      trees = trees.where((tree) => tree['id'] != worktreeId).toList();
      return {'kind': 'succeeded'};
    }
    // unknown envelope 回显请求 id（后端语义），保证对账同 id。
    if (scripted['kind'] == 'unknown') {
      return {'kind': 'unknown', 'clientOperationId': clientOperationId};
    }
    return scripted;
  }

  /// remove 调用后的权威列表副作用钩子（模拟服务端实际执行结果，测试注入）。
  void Function(String worktreeId)? onRemoveApplied;

  @override
  Future<Map<String, dynamic>?> mutationOperation(
    String clientOperationId,
  ) async {
    mutationOperationIds.add(clientOperationId);
    final ledger = ledgerScript?.call(clientOperationId);
    return ledger == null ? null : Map<String, dynamic>.from(ledger);
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
    Future<SessionSummary> Function(String projectId, String worktreeId)?
    onCreateSession,
    String? activeId = 'wt-main',
    Future<bool> Function(String worktreeId)? confirmLeaveDirty,
    VoidCallback? onWorktreesMutated,
  }) {
    return MaterialApp(
      home: Scaffold(
        body: WorktreesPage(
          book: addressBook,
          http: LanHttpClient(),
          project: const ProjectSummary(id: 'p1', name: 'demo'),
          activeId: activeId,
          onSelect: onSelect ?? (_) {},
          gitClient: git,
          onCreateSession: onCreateSession,
          confirmLeaveDirty: confirmLeaveDirty,
          onWorktreesMutated: onWorktreesMutated,
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
    // 对齐 web MobileWorktreePanel removeConfirm 文案（列表卡轻口径）。
    expect(find.textContaining('请先确认不再需要该工作区'), findsOneWidget);

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
    expect(find.textContaining('已移除 worktree「feat/app」'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('删除激活 worktree 前脏文件预检：取消则不弹删除确认也不调后端', (tester) async {
    final git = seed();
    final preflightIds = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        activeId: 'wt-1',
        confirmLeaveDirty: (worktreeId) async {
          preflightIds.add(worktreeId);
          return false;
        },
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();

    // 只读预检先行且取消：删除确认不出现、后端未收到请求。
    expect(preflightIds, ['wt-1']);
    expect(git.removeCount, 0);
    expect(find.widgetWithText(FilledButton, '移除'), findsNothing);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
  });

  testWidgets('删除激活 worktree 前脏文件预检：丢弃后进入删除确认并成功移除', (tester) async {
    final git = seed();
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        activeId: 'wt-1',
        confirmLeaveDirty: (_) async => true,
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    expect(git.removeCount, 1);
    expect(git.removedIds, ['wt-1']);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsNothing);
    expect(find.textContaining('已移除 worktree「feat/app」'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('删除非激活 worktree 不触发脏文件预检', (tester) async {
    final git = seed();
    var preflightCalls = 0;
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        confirmLeaveDirty: (_) async {
          preflightCalls += 1;
          return true;
        },
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    // 预检只针对激活 worktree（对齐 web requiresActivePreflight）。
    expect(preflightCalls, 0);
    expect(git.removeCount, 1);
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
      tester
          .widget<FilledButton>(find.widgetWithText(FilledButton, '创建'))
          .onPressed,
      isNull,
    );

    await tester.enterText(
      find.byKey(const Key('worktree-suffix-input')),
      'my-task',
    );
    await tester.pumpAndSettle();
    expect(
      tester
          .widget<FilledButton>(find.widgetWithText(FilledButton, '创建'))
          .onPressed,
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
    await tester.enterText(
      find.byKey(const Key('worktree-suffix-input')),
      'login-crash',
    );
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

    await tester.enterText(
      find.byKey(const Key('worktree-suffix-input')),
      'auto-term',
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(createdSessions, ['p1/wt-new']);
    expect(selected, ['wt-new']);
    expect(
      find.textContaining('已创建 worktree「feature/auto-term」'),
      findsOneWidget,
    );
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
        onCreateSession: (projectId, worktreeId) async =>
            throw Exception('pty boom'),
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('worktree-suffix-input')),
      'term-fail',
    );
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

    await tester.enterText(
      find.byKey(const Key('worktree-suffix-input')),
      'feat/x',
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('创建'));
    await tester.pumpAndSettle();

    expect(find.textContaining('创建失败'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'feat/x'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('B1 移除返回 unknown → 同 id 对账确认已删 → 横幅消失并提示', (tester) async {
    final git = seed();
    git.mutationEnvelopeScript = (kind) => {
      'kind': 'unknown',
      'clientOperationId': 'srv-op-r1',
    };
    git.ledgerScript = (operationId) => {
      'state': 'running',
      'intent': {'kind': 'remove', 'worktreeId': 'wt-1'},
    };
    // 服务端实际已删（权威列表移除），ledger 记录 remove intent 进行中 → 对账可确认成功。
    git.onRemoveApplied = (worktreeId) {
      git.trees = git.trees.where((tree) => tree['id'] != worktreeId).toList();
    };
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    expect(git.removeCount, 1);
    expect(git.mutationOperationIds, [git.removeOperationIds.first]);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsNothing);
    expect(find.byKey(const Key('worktrees-unknown-banner')), findsNothing);
    expect(find.textContaining('已移除 worktree「feat/app」'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('B1 移除传输异常 → unknown 横幅可重新对账，仍未知时不盲重放', (tester) async {
    final git = seed()
      ..removeError = const SocketException('network unreachable');
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    // 传输异常不算确定失败：无「移除失败」SnackBar，出现横幅 + 重新对账。
    expect(find.textContaining('移除失败'), findsNothing);
    expect(find.byKey(const Key('worktrees-unknown-banner')), findsOneWidget);
    expect(git.removeCount, 1);
    // 源树仍在：重新对账（ledger 无记录）保持 unknown，删除不重发。
    await tester.tap(find.byKey(const Key('worktrees-retry-reconcile')));
    await tester.pumpAndSettle();
    expect(git.removeCount, 1);
    expect(find.byKey(const Key('worktrees-unknown-banner')), findsOneWidget);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('B14 非主卡片「合并」：确认 → 成功提示并刷新', (tester) async {
    final git = seed();
    final selected = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        onSelect: (tree) => selected.add(tree['id'] as String),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    expect(find.text('确定把「feat/app」合并到主工作区？'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(git.mergeCount, 1);
    // 被合并树不是 active（activeId=wt-main）→ 不触发 onSelect。
    expect(selected, isEmpty);
    expect(find.textContaining('合并成功'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('B14 合并激活 worktree：dirty 预检取消则不调后端', (tester) async {
    final git = seed();
    final preflightIds = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        activeId: 'wt-1',
        confirmLeaveDirty: (worktreeId) async {
          preflightIds.add(worktreeId);
          return false;
        },
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    // 对齐 web runMobileWorktreeMergeFlow：源=激活树先只读预检，取消不调后端。
    expect(preflightIds, ['wt-1']);
    expect(git.mergeCount, 0);
    expect(find.byKey(const Key('worktree-item-wt-1')), findsOneWidget);
  });

  testWidgets('B14 合并激活 worktree：预检放行后正常合并', (tester) async {
    final git = seed();
    final selected = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        onSelect: (tree) => selected.add(tree['id'] as String),
        activeId: 'wt-1',
        confirmLeaveDirty: (_) async => true,
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(git.mergeCount, 1);
    // 源树被删且正是 active → onSelect(主树) 交 shell 兜底。
    expect(selected, ['wt-main']);
    await flushSnackbars(tester);
  });

  testWidgets('B14 合并非激活 worktree 不触发 dirty 预检', (tester) async {
    final git = seed();
    var preflightCalls = 0;
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        confirmLeaveDirty: (_) async {
          preflightCalls += 1;
          return true;
        },
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    // activeId=wt-main，合并 wt-1 不需要预检（对齐 web requiresActivePreflight）。
    expect(preflightCalls, 0);
    expect(git.mergeCount, 1);
    await flushSnackbars(tester);
  });

  testWidgets('B14 合并的源树是 active → 成功后 onSelect(主树) 交 shell 兜底', (
    tester,
  ) async {
    final git = seed();
    final selected = <String>[];
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        onSelect: (tree) => selected.add(tree['id'] as String),
        activeId: 'wt-1',
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(git.mergeCount, 1);
    expect(selected, ['wt-main']);
    await flushSnackbars(tester);
  });

  testWidgets('B14 合并 unknown → 同 id 对账 ledger 失败 → 提示可重新发起', (tester) async {
    final git = seed();
    git.mutationEnvelopeScript = (kind) => {
      'kind': 'unknown',
      'clientOperationId': 'srv-op-m1',
    };
    git.ledgerScript = (operationId) => {
      'state': 'failed',
      'intent': {'kind': 'merge', 'sourceWorktreeId': 'wt-1'},
    };
    await tester.pumpWidget(wrap(await book(), git));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-merge-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();

    expect(git.mergeCount, 1);
    expect(git.mutationOperationIds, [git.mergeOperationIds.first]);
    expect(find.byKey(const Key('worktrees-unknown-banner')), findsNothing);
    expect(find.textContaining('操作失败，可以重新发起'), findsOneWidget);
    await flushSnackbars(tester);
  });

  testWidgets('删除/合并成功回调 onWorktreesMutated（壳层收敛接缝契约）', (tester) async {
    final git = seed();
    git.trees = [
      ...git.trees,
      {
        'id': 'wt-2',
        'name': 'chore',
        'branch': 'chore/x',
        'isMain': false,
        'path': '/repo/.worktrees/chore-x',
      },
    ];
    var mutated = 0;
    await tester.pumpWidget(
      wrap(
        await book(),
        git,
        activeId: 'wt-1',
        onWorktreesMutated: () => mutated++,
      ),
    );
    await tester.pumpAndSettle();

    // 删除激活树 wt-1：成功 → 回调壳层一次。
    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();
    expect(git.removeCount, 1);
    expect(mutated, 1, reason: '删除成功后应回调壳层（重拉列表 + bump 会话令牌）');

    // 合并非激活树 wt-2：成功 → 再回调一次。
    await tester.tap(find.byKey(const Key('worktree-merge-wt-2')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();
    expect(git.mergeCount, 1);
    expect(mutated, 2, reason: '合并成功后应再回调壳层一次');
    await flushSnackbars(tester);
  });
}
