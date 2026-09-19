import 'package:cc_partner_mobile/ui/workbench_shell.dart';
import 'package:cc_partner_mobile/workbench/nav.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('drawer shows unread badge only on the attention panel', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.global,
          panel: WorkbenchPanel.projects,
          onSelect: (_) {},
          badges: const {WorkbenchPanel.attention: 5},
          child: const Text('body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-badge-attention')), findsOneWidget);
    expect(find.text('5'), findsOneWidget);
    // 其他面板不显示徽章。
    expect(find.byKey(const Key('nav-badge-transfer')), findsNothing);
    expect(find.byKey(const Key('nav-badge-projects')), findsNothing);
  });

  testWidgets('drawer hides the badge when unread count is zero', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.global,
          panel: WorkbenchPanel.projects,
          onSelect: (_) {},
          badges: const {WorkbenchPanel.attention: 0},
          child: const Text('body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-badge-attention')), findsNothing);
  });

  testWidgets('badge caps at 99+ for very large counts', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.global,
          panel: WorkbenchPanel.projects,
          onSelect: (_) {},
          badges: const {WorkbenchPanel.attention: 120},
          child: const Text('body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.text('99+'), findsOneWidget);
  });

  testWidgets('project mode shortcuts group also carries the attention badge', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          onSelect: (_) {},
          onBackToProjects: () {},
          badges: const {WorkbenchPanel.attention: 2},
          child: const Text('body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-badge-attention')), findsOneWidget);
    expect(find.text('2'), findsOneWidget);
  });
}
