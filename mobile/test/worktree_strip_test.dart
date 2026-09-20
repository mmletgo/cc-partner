import 'package:cc_partner_mobile/ui/worktree_strip.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Map<String, dynamic> tree(
  String id, {
  bool isMain = false,
  int changed = 0,
  int conflicts = 0,
  bool clean = true,
}) =>
    {
      'id': id,
      'name': id,
      'branch': id,
      'isMain': isMain,
      'status': {
        'branch': id,
        'changed': changed,
        'ahead': 0,
        'behind': 0,
        'conflicts': conflicts,
        'clean': clean,
        'canPush': false,
      },
    };

void main() {
  Future<void> pump(
    WidgetTester tester, {
    required List<Map<String, dynamic>> worktrees,
    String? activeId,
    ValueChanged<Map<String, dynamic>>? onRemove,
    bool busy = false,
    ValueChanged<String>? onCreate,
    bool creating = false,
    String? mutationError,
    VoidCallback? onRetryReconcile,
  }) async {
    final removed = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: WorktreeStrip(
            worktrees: worktrees,
            activeId: activeId,
            onSelect: (_) {},
            onRemove: onRemove ?? ((tree) => removed.add(tree['id'] as String)),
            busy: busy,
            onCreate: onCreate,
            creating: creating,
            mutationError: mutationError,
            onRetryReconcile: onRetryReconcile,
          ),
        ),
      ),
    );
  }

  test('worktreeStripToneOf：冲突 > 有改动 > 干净', () {
    expect(worktreeStripToneOf(tree('a')), WorktreeStripTone.clean);
    expect(worktreeStripToneOf(tree('b', changed: 2, clean: false)), WorktreeStripTone.dirty);
    expect(worktreeStripToneOf(tree('c', changed: 2, conflicts: 1)), WorktreeStripTone.conflict);
    // status 缺失宽容回干净。
    expect(worktreeStripToneOf({'id': 'd'}), WorktreeStripTone.clean);
  });

  testWidgets('chip 展示状态点与主/linked 后缀；主 chip 无删除按钮', (tester) async {
    await pump(
      tester,
      worktrees: [
        tree('wt-main', isMain: true),
        tree('wt-1', changed: 1, clean: false),
      ],
      activeId: 'wt-main',
    );
    expect(find.byKey(const Key('worktree-wt-main')), findsOneWidget);
    expect(find.byKey(const Key('worktree-wt-1')), findsOneWidget);
    expect(find.text('主'), findsOneWidget);
    expect(find.text('worktree'), findsOneWidget);
    expect(find.byKey(const Key('worktree-remove-wt-main')), findsNothing);
    expect(find.byKey(const Key('worktree-remove-wt-1')), findsOneWidget);
  });

  testWidgets('非主 chip 的 X 触发 onRemove；busy 时禁用', (tester) async {
    final removed = <String>[];
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true), tree('wt-1')],
      activeId: 'wt-main',
      onRemove: (tree) => removed.add(tree['id'] as String),
    );
    await tester.tap(find.byKey(const Key('worktree-remove-wt-1')));
    await tester.pumpAndSettle();
    expect(removed, ['wt-1']);

    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true), tree('wt-1')],
      activeId: 'wt-main',
      onRemove: (tree) => removed.add(tree['id'] as String),
      busy: true,
    );
    expect(
      tester.widget<IconButton>(find.byKey(const Key('worktree-remove-wt-1'))).onPressed,
      isNull,
    );
  });

  testWidgets('B5 「+ 新建」展开 inline 表单，按 前缀/后缀 回调 onCreate', (tester) async {
    final created = <String>[];
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      onCreate: created.add,
    );
    // 未展开时只有「+ 新建」chip，无表单。
    expect(find.byKey(const Key('worktree-create')), findsOneWidget);
    expect(find.byKey(const Key('worktree-create-form')), findsNothing);

    await tester.tap(find.byKey(const Key('worktree-create')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-create-form')), findsOneWidget);

    // 空后缀：确认禁用。
    expect(
      tester.widget<TextButton>(find.byKey(const Key('worktree-create-confirm'))).onPressed,
      isNull,
    );

    await tester.enterText(find.byKey(const Key('worktree-create-suffix')), 'my-task');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-create-confirm')));
    await tester.pumpAndSettle();

    expect(created, ['feature/my-task']);
    // 确认后表单收起。
    expect(find.byKey(const Key('worktree-create-form')), findsNothing);
  });

  testWidgets('B5 切换前缀后组合分支名；取消收起表单', (tester) async {
    final created = <String>[];
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      onCreate: created.add,
    );
    await tester.tap(find.byKey(const Key('worktree-create')));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('worktree-create-prefix')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('fix').last);
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('worktree-create-suffix')), 'login-crash');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-create-confirm')));
    await tester.pumpAndSettle();
    expect(created, ['fix/login-crash']);

    // 再展开后取消：表单收起且不回调。
    await tester.tap(find.byKey(const Key('worktree-create')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('worktree-create-suffix')), 'dropped');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-create-cancel')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-create-form')), findsNothing);
    expect(created, ['fix/login-crash']);
  });

  testWidgets('B5 busy/creating 时创建槽禁用', (tester) async {
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      onCreate: (_) {},
      busy: true,
    );
    expect(
      tester.widget<ActionChip>(find.byKey(const Key('worktree-create'))).onPressed,
      isNull,
    );

    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      onCreate: (_) {},
      creating: true,
    );
    expect(
      tester.widget<ActionChip>(find.byKey(const Key('worktree-create'))).onPressed,
      isNull,
    );
  });

  testWidgets('B1 mutation 错误条展示并触发重新对账', (tester) async {
    var retried = 0;
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      mutationError: '移除结果未知，请重新对账。',
      onRetryReconcile: () => retried += 1,
    );
    expect(find.byKey(const Key('worktree-strip-error')), findsOneWidget);
    expect(find.text('移除结果未知，请重新对账。'), findsOneWidget);

    await tester.tap(find.byKey(const Key('worktree-retry-reconcile')));
    await tester.pumpAndSettle();
    expect(retried, 1);

    // 无错误时不渲染错误条。
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
    );
    expect(find.byKey(const Key('worktree-strip-error')), findsNothing);
  });

  testWidgets('空列表显示「暂无 worktree」占位，创建槽仍可用', (tester) async {
    // 空列表（无创建槽）：只渲染占位文案。
    await pump(tester, worktrees: const []);
    expect(find.text('暂无 worktree'), findsOneWidget);
    expect(find.byKey(const Key('worktree-strip-empty')), findsOneWidget);
    expect(find.byKey(const Key('worktree-create')), findsNothing);

    // 空列表（带创建槽）：占位与「+ 新建」并存（对齐 web 空态文案 + 条上创建表单）。
    var created = '';
    await pump(tester, worktrees: const [], onCreate: (branch) => created = branch);
    expect(find.text('暂无 worktree'), findsOneWidget);
    expect(find.byKey(const Key('worktree-create')), findsOneWidget);

    // 非空列表不渲染占位。
    await pump(
      tester,
      worktrees: [tree('wt-main', isMain: true)],
      activeId: 'wt-main',
      onCreate: (branch) => created = branch,
    );
    expect(find.text('暂无 worktree'), findsNothing);
    expect(find.byKey(const Key('worktree-create')), findsOneWidget);
    expect(created, isEmpty);
  });
}
