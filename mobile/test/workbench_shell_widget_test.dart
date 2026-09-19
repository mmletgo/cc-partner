import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/ui/workbench_home.dart';
import 'package:cc_partner_mobile/ui/workbench_shell.dart';
import 'package:cc_partner_mobile/workbench/nav.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('global shell exposes 项目 / 待处理 / 传输 / 设置 / Provider', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.global,
          panel: WorkbenchPanel.projects,
          onSelect: (_) {},
          child: const Text('global-body'),
        ),
      ),
    );
    expect(find.byKey(const Key('workbench-shell')), findsOneWidget);
    expect(find.text('global-body'), findsOneWidget);
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-projects')), findsOneWidget);
    expect(find.byKey(const Key('nav-attention')), findsOneWidget);
    expect(find.byKey(const Key('nav-transfer')), findsOneWidget);
    expect(find.byKey(const Key('nav-settings')), findsOneWidget);
    expect(find.byKey(const Key('nav-provider')), findsOneWidget);
    // 「项目」/「待处理」同时是分组标题与导航项文案；「工具」是分组标题。
    expect(find.text('项目'), findsWidgets);
    expect(find.text('待处理'), findsNWidgets(2));
    expect(find.text('工具'), findsOneWidget);
    expect(find.text('传输'), findsOneWidget);
    expect(find.text('设置'), findsOneWidget);
    expect(find.text('Provider'), findsOneWidget);
  });

  testWidgets('project shell exposes 终端 / 文件 / Git / worktrees / 自动化 / 浏览器', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          onSelect: (_) {},
          onBackToProjects: () {},
          worktreeStrip: const Text('wt-main'),
          child: const Text('project-body'),
        ),
      ),
    );
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    for (final name in [
      'terminal',
      'files',
      'git',
      'worktrees',
      'automation',
      'browser',
    ]) {
      expect(find.byKey(Key('nav-$name')), findsOneWidget);
    }
    expect(find.text('终端'), findsOneWidget);
    expect(find.text('文件'), findsOneWidget);
    expect(find.text('Git'), findsOneWidget);
    expect(find.text('worktrees'), findsOneWidget);
    expect(find.text('自动化'), findsOneWidget);
    expect(find.text('浏览器'), findsOneWidget);
    // Drawer 分组标题已中文化（不再是英文 id）。
    expect(find.text('工作'), findsOneWidget);
    expect(find.text('快捷'), findsOneWidget);
    expect(find.text('work'), findsNothing);
    expect(find.text('shortcuts'), findsNothing);
  });

  testWidgets('experimental switches off hide automation/browser from drawer', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          automationEnabled: false,
          browserEnabled: false,
          onSelect: (_) {},
          child: const Text('project-body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-automation')), findsNothing);
    expect(find.byKey(const Key('nav-browser')), findsNothing);
    expect(find.byKey(const Key('nav-terminal')), findsOneWidget);
  });

  testWidgets('hideWorktreeStrip removes the strip (terminal fullscreen)', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          hideWorktreeStrip: true,
          onSelect: (_) {},
          worktreeStrip: const Text('wt-main'),
          child: const Text('project-body'),
        ),
      ),
    );
    expect(find.byKey(const Key('worktree-strip')), findsNothing);
    expect(find.text('project-body'), findsOneWidget);
  });

  testWidgets('WorkbenchHome first screen is the real dual-mode shell', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.8:62116',
      probe: (_) async => throw Exception('offline'),
      forceIfUnreachable: true,
    );
    await tester.pumpWidget(
      MaterialApp(
        home: AppThemeScope(
          mode: ThemeMode.dark,
          onChanged: (_) {},
          child: WorkbenchHome(book: book, http: LanHttpClient()),
        ),
      ),
    );
    await tester.pump();
    expect(find.byKey(const Key('workbench-shell')), findsOneWidget);
    expect(find.text('添加本机目录'), findsOneWidget);
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.text('项目'), findsWidgets);
    expect(find.text('待处理'), findsNWidgets(2));
    expect(find.text('工具'), findsOneWidget);
    expect(find.text('传输'), findsOneWidget);
    expect(find.text('设置'), findsOneWidget);
    expect(find.text('Provider'), findsOneWidget);
  });

  testWidgets('CcPartnerApp first screen shows address book, not an empty shell', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await tester.pumpWidget(CcPartnerApp(book: book, http: LanHttpClient()));
    expect(find.textContaining('同一可达网络'), findsOneWidget);
    expect(find.byKey(const Key('add-server')), findsOneWidget);
  });
}
