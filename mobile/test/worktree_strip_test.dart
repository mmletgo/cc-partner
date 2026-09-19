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
}
