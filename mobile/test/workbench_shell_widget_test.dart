import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/ui/workbench_home.dart';
import 'package:cc_partner_mobile/ui/workbench_shell.dart';
import 'package:cc_partner_mobile/workbench/nav.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 按 path 路由的假 HTTP 通道：只为 shell 级测试提供 workbench worktree 相关端点，
/// 其余请求抛 LanHttpException（调用方均为宽容解析，静默降级）。
class _RoutingHttp extends LanHttpClient {
  List<Map<String, dynamic>> trees;
  Object? removeError;

  _RoutingHttp(this.trees) : super();

  Never _notFound() => throw LanHttpException(404, 'not found');

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
      // experimentalFeatures 缺省 → fail-closed 全关。
      return <String, dynamic>{};
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> getDynamic(String baseUrl, String path) async {
    if (path == '/api/mobile/workbench/projects/list') {
      return {
        'projects': [
          {'id': 'p1', 'name': 'demo', 'path': '/repo', 'kind': 'local'},
        ],
      };
    }
    if (path == '/api/mobile/workbench/fs/roots') {
      return {'roots': <Map<String, dynamic>>[]};
    }
    _notFound();
  }

  @override
  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    if (path == '/api/mobile/workbench/worktrees/list') {
      return {
        'ok': true,
        'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)],
      };
    }
    if (path == '/api/mobile/workbench/worktrees/remove') {
      if (removeError != null) {
        throw removeError!;
      }
      // 模拟「服务端实际已删但响应不确定」：权威列表同步移除，envelope 回 unknown + 回显 id。
      final id = body['worktreeId'] as String?;
      trees = [
        for (final tree in trees)
          if (tree['id'] != id) Map<String, dynamic>.from(tree),
      ];
      return {
        'kind': 'unknown',
        'clientOperationId': body['clientOperationId'],
      };
    }
    _notFound();
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    // mutation-operation 走 postDynamic（宽容解析通道）。
    if (path == '/api/mobile/workbench/worktrees/mutation-operation') {
      return {
        'state': 'running',
        'intent': {
          'kind': 'remove',
          'worktreeId': 'wt-1',
        },
      };
    }
    _notFound();
  }
}

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

  testWidgets('B1 shell strip 移除 unknown → 同 id 对账确认成功，错误条消失', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.8:62116',
      probe: (_) async => throw Exception('offline'),
      forceIfUnreachable: true,
    );
    final http = _RoutingHttp([
      {
        'id': 'wt-main',
        'name': 'main',
        'branch': 'main',
        'isMain': true,
        'path': '/repo',
      },
      {
        'id': 'wt-1',
        'name': 'feat',
        'branch': 'feat/app',
        'isMain': false,
        'path': '/repo/.worktrees/feat-app',
      },
    ]);
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchHome(book: book, http: http),
      ),
    );
    await tester.pumpAndSettle();

    // 打开项目 → 终端面板 + worktree strip。
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
    expect(find.byKey(const Key('worktree-remove-wt-1')), findsOneWidget);

    // strip X → 确认移除 → 服务端 unknown envelope。
    await tester.tap(find.byKey(const Key('worktree-remove-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    // 同 id 对账（ledger remove intent + 权威列表无 wt-1）→ 确认成功。
    expect(find.byKey(const Key('worktree-strip-error')), findsNothing);
    expect(find.byKey(const Key('worktree-remove-wt-1')), findsNothing);
    expect(find.textContaining('已移除 worktree「feat」'), findsOneWidget);
    // 让 SnackBar 自动消失，避免残留 Timer。
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });

  testWidgets('B1 shell strip 移除传输异常 → 条上错误条 + 重新对账入口', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.8:62116',
      probe: (_) async => throw Exception('offline'),
      forceIfUnreachable: true,
    );
    final http = _RoutingHttp([
      {
        'id': 'wt-main',
        'name': 'main',
        'branch': 'main',
        'isMain': true,
        'path': '/repo',
      },
      {
        'id': 'wt-1',
        'name': 'feat',
        'branch': 'feat/app',
        'isMain': false,
        'path': '/repo/.worktrees/feat-app',
      },
    ]);
    http.removeError = const SocketException('network unreachable');
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchHome(book: book, http: http),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-remove-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    // 传输异常按 unknown 处理：条上错误条出现，且无「移除失败」SnackBar。
    expect(find.byKey(const Key('worktree-strip-error')), findsOneWidget);
    expect(find.byKey(const Key('worktree-retry-reconcile')), findsOneWidget);
    expect(find.textContaining('移除失败'), findsNothing);
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });
}
