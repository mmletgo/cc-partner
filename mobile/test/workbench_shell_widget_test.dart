import 'dart:async';
import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/files/workspace.dart';
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

/// worktrees 列表请求序号守卫测试用的假 HTTP：第 2 次 worktrees/list 挂起到
/// [secondListGate] 放行（模拟旧响应慢），其余调用立即返回同一份两树列表；
/// projects list / experimental config 按壳层测试惯例最小供给，其余端点 404
/// （调用方均为宽容解析，静默降级）。
class _WorktreeSeqHttp extends LanHttpClient {
  /// 第 2 次 worktrees/list 的完成闸门：complete 后旧响应才落地。
  final Completer<void> secondListGate = Completer<void>();

  int listCalls = 0;

  static const List<Map<String, dynamic>> _trees = [
    {
      'id': 'wt-a',
      'name': 'wt-a',
      'branch': 'feat/a',
      'isMain': false,
      'path': '/repo-a',
    },
    {
      'id': 'wt-main',
      'name': 'main',
      'branch': 'main',
      'isMain': true,
      'path': '/repo',
    },
  ];

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
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
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    if (path == '/api/mobile/workbench/worktrees/list') {
      listCalls += 1;
      if (listCalls == 2) {
        // 先点的树（旧请求）挂起，后点的树（新请求）先完成。
        await secondListGate.future;
      }
      return {
        'ok': true,
        'worktrees': [for (final tree in _trees) Map<String, dynamic>.from(tree)],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }
}

/// 面板常驻挂载 / Drawer 重试 / 徽章轮询测试用的假 HTTP：
/// 按 path 路由 workbench 相关端点并计数关键调用（sessions boot、files listDir、
/// attention 拉取、experimentalFeatures 配置）；openWebSocket 返回永不完成的
/// Future，模拟局域网建连悬挂，避免测试内发起真实 IO 或产生重连 Timer。
class _PanelHttp extends LanHttpClient {
  int configCalls = 0;
  int attentionCalls = 0;
  int sessionsListCalls = 0;
  int replayCalls = 0;
  int listDirCalls = 0;
  int worktreesListCalls = 0;

  /// true 时 /api/orchestrator/config 抛错（experimentalFeatures 拉取失败）。
  bool failConfig = false;

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
      configCalls += 1;
      if (failConfig) {
        throw LanHttpException(500, 'config unavailable');
      }
      // experimentalFeatures 全开：成功时 Drawer 应恢复 automation/browser 入口。
      return {
        'experimentalFeatures': {'automation': true, 'browser': true},
      };
    }
    if (path == '/api/health') {
      // attention 徽章拉取先探测能力（attention.v2 含 Agent 投影）。
      return {
        'protocol_version': 2,
        'capabilities': ['attention.v1', 'attention.v2'],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> getDynamic(String baseUrl, String path) async {
    if (path == '/api/mobile/attention/v2' || path == '/api/mobile/attention') {
      attentionCalls += 1;
      return {'items': <Map<String, dynamic>>[]};
    }
    if (path == '/api/mobile/workbench/projects/list') {
      return {
        'projects': [
          {'id': 'p1', 'name': 'demo', 'path': '/repo', 'kind': 'local'},
        ],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    if (path == '/api/mobile/workbench/sessions/list') {
      sessionsListCalls += 1;
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running'},
      ];
    }
    if (path == '/api/mobile/workbench/files/list-dir') {
      listDirCalls += 1;
      final dirPath = body['path'] as String?;
      if (dirPath == null || dirPath.isEmpty) {
        return [
          {'name': 'src', 'kind': 'dir', 'path': 'src'},
        ];
      }
      return [
        {'name': 'main.rs', 'kind': 'file', 'path': 'src/main.rs'},
      ];
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    if (path == '/api/mobile/workbench/worktrees/list') {
      worktreesListCalls += 1;
      return {
        'ok': true,
        'worktrees': [
          {
            'id': 'wt-main',
            'name': 'main',
            'branch': 'main',
            'isMain': true,
            'path': '/repo',
          },
        ],
      };
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
      replayCalls += 1;
      return {'sessionId': body['sessionId'], 'snapshot': 'boot-ok', 'lastSeq': 0};
    }
    if (path == '/api/mobile/workbench/sessions/focus' ||
        path == '/api/mobile/workbench/sessions/zoom-pane') {
      return <String, dynamic>{};
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    return Completer<WebSocket>().future;
  }
}

/// 上下文保留 / 连接态 / lastLocation 测试用的假 HTTP：
/// 两个项目（p1 demo 两棵树、p2 other 一棵树）+ 关键端点计数；
/// worktrees/list 可用 [failWorktreesList] 切换失败模拟断线；
/// worktrees/create 挂起到 [createGate] 放行，模拟创建全程在途。
class _CtxHttp extends LanHttpClient {
  final Completer<void> createGate = Completer<void>();

  int worktreesListCalls = 0;
  int sessionsListCalls = 0;
  int replayCalls = 0;

  /// true 时 worktrees/list 抛错（壳层应进入离线态）。
  bool failWorktreesList = false;

  static const Map<String, List<Map<String, dynamic>>> _treesByProject = {
    'p1': [
      {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo'},
      {
        'id': 'wt-1',
        'name': 'feat',
        'branch': 'feat/app',
        'isMain': false,
        'path': '/repo/.worktrees/feat-app',
      },
    ],
    'p2': [
      {'id': 'wt-p2-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo2'},
    ],
  };

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
      return {'experimentalFeatures': {'automation': true, 'browser': true}};
    }
    if (path == '/api/health') {
      // attention 徽章拉取先探测能力（attention.v2 含 Agent 投影）。
      return {
        'protocol_version': 2,
        'capabilities': ['attention.v1', 'attention.v2'],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> getDynamic(String baseUrl, String path) async {
    if (path == '/api/mobile/attention/v2' || path == '/api/mobile/attention') {
      return {'items': <Map<String, dynamic>>[]};
    }
    if (path == '/api/mobile/workbench/projects/list') {
      return {
        'projects': [
          {'id': 'p1', 'name': 'demo', 'path': '/repo', 'kind': 'local'},
          {'id': 'p2', 'name': 'other', 'path': '/repo2', 'kind': 'local'},
        ],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    if (path == '/api/mobile/workbench/sessions/list') {
      sessionsListCalls += 1;
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running'},
      ];
    }
    if (path == '/api/mobile/workbench/files/list-dir') {
      return [
        {'name': 'src', 'kind': 'dir', 'path': 'src'},
      ];
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<Map<String, dynamic>> postJson(
    String baseUrl,
    String path,
    Map<String, dynamic> body,
  ) async {
    if (path == '/api/mobile/workbench/worktrees/list') {
      worktreesListCalls += 1;
      if (failWorktreesList) {
        throw LanHttpException(503, 'worktrees unavailable');
      }
      final trees = _treesByProject[body['projectId'] as String? ?? 'p1']!;
      return {
        'ok': true,
        'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)],
      };
    }
    if (path == '/api/mobile/workbench/worktrees/create') {
      // 创建全程在途：挂起直到测试放行（对齐 web beginWorktreeOperation 全程持锁）。
      await createGate.future;
      return {
        'id': 'wt-new',
        'name': 'feat/task1',
        'branch': 'feat/task1',
        'isMain': false,
        'path': '/repo/.worktrees/feat-task1',
      };
    }
    if (path == '/api/mobile/workbench/sessions/create') {
      return {'id': 's-new', 'projectId': 'p1', 'name': 's-new', 'status': 'running'};
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
      replayCalls += 1;
      return {'sessionId': body['sessionId'], 'snapshot': 'boot-ok', 'lastSeq': 0};
    }
    if (path == '/api/mobile/workbench/sessions/focus' ||
        path == '/api/mobile/workbench/sessions/zoom-pane') {
      return <String, dynamic>{};
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    return Completer<WebSocket>().future;
  }
}

/// P1-3「看见即已读」测试假 HTTP：单树单会话列表 + 两条未读 agentNeedsInput 条目
/// （一条命中当前会话 s1，一条属于其它会话 s9）；记录 markRead 请求体与
/// sessions/focus 的 streamActive 取值，验证切会话/离开终端面板时的 unfocus 补发。
class _AutoReadHttp extends LanHttpClient {
  /// 已执行的 markRead itemIds 请求体列表。
  final List<List<String>> markReadBodies = [];

  /// sessions/focus 请求记录 (sessionId, streamActive)。
  final List<(String, bool)> focusCalls = [];

  /// attention list 可见条目（getDynamic 每次返回同一份，模拟快照未变化）。
  final List<Map<String, dynamic>> attentionItems = [
    {
      'id': 'a1',
      'sourceKind': 'agentNeedsInput',
      'title': 'Agent 等待输入',
      'target': {'kind': 'agentSession', 'projectId': 'p1', 'terminalSessionId': 's1'},
    },
    {
      'id': 'a2',
      'sourceKind': 'agentNeedsInput',
      'title': '其它会话等待输入',
      'target': {'kind': 'agentSession', 'projectId': 'p1', 'terminalSessionId': 's9'},
    },
  ];

  int attentionListCalls = 0;

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
      return <String, dynamic>{};
    }
    if (path == '/api/health') {
      return {
        'protocol_version': 2,
        'capabilities': ['attention.v1', 'attention.v2'],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> getDynamic(String baseUrl, String path) async {
    if (path == '/api/mobile/attention/v2' || path == '/api/mobile/attention') {
      attentionListCalls += 1;
      return {'items': [for (final item in attentionItems) Map<String, dynamic>.from(item)]};
    }
    if (path == '/api/mobile/workbench/projects/list') {
      return {
        'projects': [
          {'id': 'p1', 'name': 'demo', 'path': '/repo', 'kind': 'local'},
        ],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    if (path == '/api/mobile/workbench/sessions/list') {
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running', 'worktreeId': 'wt-main'},
        {'id': 's2', 'projectId': 'p1', 'name': 's2', 'status': 'running', 'worktreeId': 'wt-main'},
      ];
    }
    throw LanHttpException(404, 'not found: $path');
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
        'worktrees': [
          {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo'},
        ],
      };
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
      return {'sessionId': body['sessionId'], 'snapshot': 'boot-ok', 'lastSeq': 0};
    }
    if (path == '/api/mobile/workbench/sessions/focus') {
      focusCalls.add((body['sessionId'] as String, body['streamActive'] as bool));
      return <String, dynamic>{};
    }
    if (path == '/api/mobile/workbench/sessions/zoom-pane') {
      return <String, dynamic>{};
    }
    if (path == '/api/mobile/attention/mark-read') {
      markReadBodies.add([for (final id in body['itemIds'] as List) id as String]);
      return {'items': <Map<String, dynamic>>[]};
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    return Completer<WebSocket>().future;
  }
}

/// P1-2 终端合并 dirty 预检/清快照测试假 HTTP：两棵树（wt-main 主树 + wt-1 功能树）、
/// 绑定 wt-1 的会话 s1、可编辑文本文件；worktrees/merge 成功并把 wt-1 从权威列表移除
/// （模拟合并删源），记录 merge 调用数。
class _TerminalMergeHttp extends LanHttpClient {
  int mergeCalls = 0;

  List<Map<String, dynamic>> trees = [
    {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo'},
    {
      'id': 'wt-1',
      'name': 'feat',
      'branch': 'feat/app',
      'isMain': false,
      'path': '/repo/.worktrees/feat-app',
    },
  ];

  @override
  Future<Map<String, dynamic>> getJson(String baseUrl, String path) async {
    if (path == '/api/orchestrator/config') {
      return <String, dynamic>{};
    }
    if (path == '/api/health') {
      return {
        'protocol_version': 2,
        'capabilities': ['attention.v1', 'attention.v2'],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> getDynamic(String baseUrl, String path) async {
    if (path == '/api/mobile/attention/v2' || path == '/api/mobile/attention') {
      return {'items': <Map<String, dynamic>>[]};
    }
    if (path == '/api/mobile/workbench/projects/list') {
      return {
        'projects': [
          {'id': 'p1', 'name': 'demo', 'path': '/repo', 'kind': 'local'},
        ],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    if (path == '/api/mobile/workbench/sessions/list') {
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running', 'worktreeId': 'wt-1'},
      ];
    }
    if (path == '/api/mobile/workbench/files/list-dir') {
      final dirPath = body['path'] as String?;
      if (dirPath == null || dirPath.isEmpty) {
        return [
          {'name': 'src', 'kind': 'dir', 'path': 'src'},
        ];
      }
      return [
        {'name': 'main.rs', 'kind': 'file', 'path': 'src/main.rs'},
      ];
    }
    throw LanHttpException(404, 'not found: $path');
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
    if (path == '/api/mobile/workbench/worktrees/merge') {
      mergeCalls += 1;
      // 合并删源：权威列表同步移除 wt-1。
      trees = [
        for (final tree in trees)
          if (tree['id'] != 'wt-1') Map<String, dynamic>.from(tree),
      ];
      return {'kind': 'succeeded', 'value': <String, dynamic>{}};
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
      return {'sessionId': body['sessionId'], 'snapshot': 'boot-ok', 'lastSeq': 0};
    }
    if (path == '/api/mobile/workbench/sessions/focus' ||
        path == '/api/mobile/workbench/sessions/zoom-pane') {
      return <String, dynamic>{};
    }
    if (path == '/api/mobile/workbench/files/open') {
      return {
        'metadata': {'name': 'main.rs', 'path': body['path']},
        'text': {'content': 'hello', 'baseHash': 'h1'},
        'capabilities': {'canEdit': true},
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<WebSocket> openWebSocket(
    String baseUrl,
    String path, {
    Iterable<String>? protocols,
  }) {
    return Completer<WebSocket>().future;
  }
}

/// 构造带一个离线 server 的地址簿（面板级测试共用）。
Future<AddressBook> _panelBook() async {
  final book = AddressBook(store: MemoryAddressBookStore());
  await book.addFromInput(
    '10.0.0.8:62116',
    probe: (_) async => throw Exception('offline'),
    forceIfUnreachable: true,
  );
  return book;
}/// 挂载 WorkbenchHome 并等初始化请求全部落地。
Future<void> _pumpHome(WidgetTester tester, LanHttpClient http) async {
  final book = await _panelBook();
  await tester.pumpWidget(
    MaterialApp(home: WorkbenchHome(book: book, http: http)),
  );
  await tester.pumpAndSettle();
}

/// 挂载 WorkbenchHome（注入测试用 Files 草稿控制器）并等初始化请求落地。
Future<AddressBook> _pumpHomeWithFiles(
  WidgetTester tester,
  LanHttpClient http,
  FileWorkspaceController files,
) async {
  final book = await _panelBook();
  await tester.pumpWidget(
    MaterialApp(home: WorkbenchHome(book: book, http: http, filesWorkspace: files)),
  );
  await tester.pumpAndSettle();
  return book;
}

/// 挂载 WorkbenchHome 并返回地址簿（lastLocation 持久化断言用）。
Future<AddressBook> _pumpHomeWithBook(WidgetTester tester, _CtxHttp http) async {
  final book = await _panelBook();
  await tester.pumpWidget(
    MaterialApp(home: WorkbenchHome(book: book, http: http)),
  );
  await tester.pumpAndSettle();
  return book;
}

/// 打开项目「demo」进入 project 模式（终端面板）。
Future<void> _openDemoProject(WidgetTester tester) async {
  await tester.tap(find.text('demo'));
  await tester.pumpAndSettle();
}

/// 经 Drawer 切换到目标面板（走真实导航路径）。
Future<void> _gotoPanelViaDrawer(WidgetTester tester, String panelName) async {
  await tester.tap(find.byTooltip('Open navigation menu'));
  await tester.pumpAndSettle();
  await tester.tap(find.byKey(Key('nav-$panelName')));
  await tester.pumpAndSettle();
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
    // 「项目」同时是分组标题与导航项文案；「待处理」只是导航项（分组标题已是收件箱）。
    expect(find.text('项目'), findsWidgets);
    expect(find.text('待处理'), findsOneWidget);
    expect(find.text('收件箱'), findsOneWidget);
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
    expect(find.text('Worktrees'), findsOneWidget);
    expect(find.text('自动化'), findsOneWidget);
    expect(find.text('预览'), findsOneWidget);
    // Drawer 分组标题已对齐 web zh navGroups（工作台/快捷）。
    expect(find.text('工作台'), findsOneWidget);
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
    expect(find.text('待处理'), findsOneWidget);
    expect(find.text('收件箱'), findsOneWidget);
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
    // 显示名口径为 branch 优先（worktreeDisplayName = branch ?? name，对齐 web）。
    expect(find.byKey(const Key('worktree-strip-error')), findsNothing);
    expect(find.byKey(const Key('worktree-remove-wt-1')), findsNothing);
    expect(find.textContaining('已移除 worktree「feat/app」'), findsOneWidget);
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

  testWidgets('快速连点两个 worktree：旧 list 响应晚到不覆盖新选中（请求序号守卫）', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.8:62116',
      probe: (_) async => throw Exception('offline'),
      forceIfUnreachable: true,
    );
    final http = _WorktreeSeqHttp();
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchHome(book: book, http: http),
      ),
    );
    await tester.pumpAndSettle();

    Future<void> expectSelected(String id, {required bool selected}) async {
      final chip = tester.widget<ChoiceChip>(find.byKey(Key('worktree-$id')));
      expect(chip.selected, selected, reason: 'chip $id 选中态应为 $selected');
    }

    // 打开项目 → worktrees/list #1 → 默认选中主树。
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(http.listCalls, 1);
    await expectSelected('wt-main', selected: true);

    // 选中 wt-a：list #2 挂起（旧请求慢），选中态先本地生效。
    await tester.tap(find.byKey(const Key('worktree-wt-a')));
    await tester.pump();
    await expectSelected('wt-a', selected: true);

    // 再选回 wt-main：list #3 立即完成，新选中生效。
    await tester.tap(find.byKey(const Key('worktree-wt-main')));
    await tester.pump();
    await expectSelected('wt-main', selected: true);
    await expectSelected('wt-a', selected: false);

    // 放行 wt-a 的旧响应：被请求序号守卫丢弃，选中保持 wt-main 不被回写。
    http.secondListGate.complete();
    await tester.pump();
    await tester.pump();
    await expectSelected('wt-main', selected: true);
    await expectSelected('wt-a', selected: false);
    expect(http.listCalls, 3);
  });

  testWidgets('面板常驻挂载：files 切走再切回保留目录栈且不重新拉取', (tester) async {
    final http = _PanelHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);
    expect(http.sessionsListCalls, 1);
    expect(http.replayCalls, 1);

    // 进文件面板并进入 src 子目录（files 首次激活后常驻挂载）。
    await _gotoPanelViaDrawer(tester, 'files');
    expect(find.text('src'), findsOneWidget);
    await tester.tap(find.text('src'));
    await tester.pumpAndSettle();
    expect(find.text('main.rs'), findsOneWidget);
    expect(find.text('上级目录'), findsOneWidget);
    expect(http.listDirCalls, 2);

    // 切终端再切回：文件页 State 保留（目录栈不重置、不重新拉取）——
    // 草稿上下文不再因切面板销毁重建。
    await _gotoPanelViaDrawer(tester, 'terminal');
    await _gotoPanelViaDrawer(tester, 'files');
    expect(find.text('main.rs'), findsOneWidget);
    expect(find.text('上级目录'), findsOneWidget);
    expect(http.listDirCalls, 2, reason: 'files 面板常驻挂载不应重新拉取目录');
    // 终端在文件面板停留期间也未被销毁重建。
    expect(http.sessionsListCalls, 1);
    expect(http.replayCalls, 1);
  });

  testWidgets('面板常驻挂载：terminal 切走再切回不重新 boot（replay 只调一次）', (tester) async {
    final http = _PanelHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);
    expect(http.sessionsListCalls, 1, reason: '打开项目时终端 boot 一次');
    expect(http.replayCalls, 1);

    // 切到传输再切回终端：终端页 State 常驻，不重走 boot/replay。
    await _gotoPanelViaDrawer(tester, 'transfer');
    await _gotoPanelViaDrawer(tester, 'terminal');
    expect(http.sessionsListCalls, 1, reason: '终端面板常驻挂载不应重新 boot');
    expect(http.replayCalls, 1, reason: '终端面板常驻挂载不应重新 replay');
  });

  testWidgets('非常驻面板：git 切走再切回重新挂载并重新拉取（对齐 web 卸载重挂）', (tester) async {
    final http = _PanelHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);
    // 打开项目时壳层拉一次 worktrees；进入 Git 页 initState 再拉一次。
    await _gotoPanelViaDrawer(tester, 'git');
    await tester.pumpAndSettle();
    final callsAfterFirstEnter = http.worktreesListCalls;
    expect(callsAfterFirstEnter, 2, reason: 'Git 页 initState 应重新拉取 worktrees');

    // 切到传输（Git 页卸载）再切回：重新 initState 拉取，保证进入即权威新鲜。
    await _gotoPanelViaDrawer(tester, 'transfer');
    await _gotoPanelViaDrawer(tester, 'git');
    await tester.pumpAndSettle();
    expect(
      http.worktreesListCalls,
      greaterThan(callsAfterFirstEnter),
      reason: 'git 非常驻面板：切走卸载，切回重新挂载并重拉',
    );
  });

  testWidgets('global Drawer 提供「断开并返回地址簿」，pop 走既有路由回上一页', (tester) async {
    final http = _PanelHttp();
    final book = await _panelBook();
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => WorkbenchHome(book: book, http: http),
              ),
            ),
            child: const Text('open-workbench'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open-workbench'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-disconnect')), findsOneWidget);
    expect(find.text('断开并返回地址簿'), findsOneWidget);

    await tester.tap(find.byKey(const Key('nav-disconnect')));
    await tester.pumpAndSettle();
    // 工作台路由被 pop 回入口页（宿主页 PopScope 保存 lastLocation 的既有路径）。
    expect(find.text('open-workbench'), findsOneWidget);
    expect(find.byKey(const Key('workbench-shell')), findsNothing);
  });

  testWidgets('project 模式 Drawer 不放「断开并返回地址簿」入口', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          onSelect: (_) {},
          onBackToProjects: () {},
          child: const Text('project-body'),
        ),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-disconnect')), findsNothing);
  });

  testWidgets('experimentalFeatures 拉取失败后打开 Drawer 静默重试并恢复入口', (tester) async {
    final http = _PanelHttp()..failConfig = true;
    await _pumpHome(tester, http);
    expect(http.configCalls, 1);

    // 打开项目进入 project 模式（automation/browser 入口在项目级「工作」组）。
    await _openDemoProject(tester);

    // 第一次打开 Drawer：上次失败 → 静默重试，仍失败则入口不出现。
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-automation')), findsNothing);
    expect(find.byKey(const Key('nav-browser')), findsNothing);
    expect(http.configCalls, 2);

    // 关闭 Drawer（点当前终端项），修复配置接口。
    await tester.tap(find.byKey(const Key('nav-terminal')));
    await tester.pumpAndSettle();
    http.failConfig = false;

    // 第二次打开 Drawer：静默重试成功 → automation/browser 入口恢复。
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-automation')), findsOneWidget);
    expect(find.byKey(const Key('nav-browser')), findsOneWidget);
    expect(http.configCalls, 3, reason: '成功后 Drawer 打开不再重复请求');
  });

  testWidgets('attention 徽章 10s 轮询：前台每周期拉取，待处理面板与退后台暂停', (tester) async {
    final http = _PanelHttp();
    await _pumpHome(tester, http);
    final baseline = http.attentionCalls; // 含 initState 首拉

    // projects 面板：壳层每 10s 恰好拉一次。
    await tester.pump(const Duration(seconds: 10));
    expect(http.attentionCalls, baseline + 1);
    await tester.pump(const Duration(seconds: 10));
    expect(http.attentionCalls, baseline + 2);

    // 停在待处理面板：页面自身 VisibilityPoller 每 10s 回写（含首拉），
    // 30s 内恰好 +3——壳层轮询必须暂停，否则会是 +6。
    await _gotoPanelViaDrawer(tester, 'attention');
    final onAttention = http.attentionCalls;
    await tester.pump(const Duration(seconds: 10));
    await tester.pump(const Duration(seconds: 10));
    await tester.pump(const Duration(seconds: 10));
    expect(http.attentionCalls, onAttention + 3, reason: '只有页面轮询在工作，壳层应已暂停');

    // 退后台：壳层与页面轮询全部停表。
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    final pausedBase = http.attentionCalls;
    await tester.pump(const Duration(seconds: 30));
    expect(http.attentionCalls, pausedBase);

    // 回前台：页面 runNow 立即补拉一次（壳层周期 Timer 10s 后才首次触发）。
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump(const Duration(milliseconds: 50));
    expect(http.attentionCalls, pausedBase + 1);
  });

  testWidgets('resumed 边沿（非待处理面板）壳层立即强刷一次未读徽章', (tester) async {
    final http = _PanelHttp();
    await _pumpHome(tester, http);
    final baseline = http.attentionCalls;

    // 退后台：壳层轮询停表。
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump(const Duration(seconds: 30));
    expect(http.attentionCalls, baseline);

    // 回前台边沿：不等 10s 周期，立即强刷一次（对齐 web useAttention focus 强刷）。
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump(const Duration(milliseconds: 50));
    expect(http.attentionCalls, baseline + 1, reason: 'resumed 边沿应立即拉取一次');

    // 随后周期轮询恢复：10s 后恰好再 +1。
    await tester.pump(const Duration(seconds: 10));
    expect(http.attentionCalls, baseline + 2);
  });

  testWidgets('创建 worktree 在途时点 chip 被拒绝并提示（创建全程持锁）', (tester) async {
    final http = _CtxHttp();
    await _pumpHomeWithBook(tester, http);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);

    Future<void> expectSelected(String id, {required bool selected}) async {
      final chip = tester.widget<ChoiceChip>(find.byKey(Key('worktree-$id')));
      expect(chip.selected, selected, reason: 'chip $id 选中态应为 $selected');
    }

    // 打开条上创建表单并确认创建：worktrees/create 挂起，创建全程在途。
    await tester.tap(find.byKey(const Key('worktree-create')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('worktree-create-suffix')), 'task1');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-create-confirm')));
    await tester.pump();

    // 在途窗口内点其它 chip：守卫拒绝并提示，选中态不变。
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    expect(find.textContaining('正在处理 worktree 操作，请稍候'), findsOneWidget);
    await expectSelected('wt-main', selected: true);
    await expectSelected('wt-1', selected: false);

    // 放行创建：后续链路（sessions/create → list → 自动选中进入终端）正常收尾。
    http.createGate.complete();
    await tester.pumpAndSettle();
    // 让 SnackBar（含排队提示）自动消失，避免残留 Timer。
    await tester.pump(const Duration(seconds: 10));
    await tester.pumpAndSettle();
  });

  testWidgets('返回项目列表保留上下文：同一项目秒回原 worktree，不同项目走完整切换', (tester) async {
    final http = _CtxHttp();
    await _pumpHomeWithBook(tester, http);

    Future<void> backToProjects() async {
      await tester.tap(find.byTooltip('Open navigation menu'));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('nav-back-projects')));
      await tester.pumpAndSettle();
    }

    // 打开 demo → 终端面板，选中 wt-1（列表请求 #1 + #2 resume）。
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 2);
    // Flutter 既有行为：worktreeId 是终端 ValueKey 的一部分，切树重挂载并重新 boot。
    expect(http.sessionsListCalls, 2);

    // 返回项目列表：只切面板，上下文全保留（对齐 web handleBackToProjects）。
    await backToProjects();
    expect(http.sessionsListCalls, 2, reason: '终端常驻挂载不因返回列表销毁');
    // AppBar 仍显示激活项目名 + 列表行「demo」，共两处。
    expect(find.text('demo'), findsNWidgets(2));

    // 点同一项目：同项目早退——直接回原 worktree/终端，不重拉、不重 boot。
    await tester.tap(find.byKey(const Key('project-row-p1')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
    expect(http.worktreesListCalls, 2, reason: '同项目早退不应重拉 worktrees');
    expect(http.sessionsListCalls, 2, reason: '同项目早退不应重新 boot 终端');
    final chip = tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-1')));
    expect(chip.selected, isTrue, reason: '原 worktree 选中态保留');

    // 再返回列表，点不同项目：走完整切换（拉新列表 + 终端换 key 重 boot）。
    await backToProjects();
    await tester.tap(find.byKey(const Key('project-row-p2')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 3, reason: '切换项目应拉取新项目 worktrees');
    expect(http.sessionsListCalls, 3, reason: '切换项目终端重挂载并 boot');
    final p2Chip = tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-p2-main')));
    expect(p2Chip.selected, isTrue);
  });

  testWidgets('lastLocation 防抖即时持久化，paused 时强制 flush', (tester) async {
    final http = _CtxHttp();
    final book = await _pumpHomeWithBook(tester, http);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();

    // 切 worktree → 500ms 防抖到期后 lastLocation 已含最新位置（无需离开页面）。
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    await tester.pump(const Duration(milliseconds: 600));
    var location = book.active!.lastLocation;
    expect(location?.projectId, 'p1');
    expect(location?.panel, 'terminal');
    expect(location?.worktreeId, 'wt-1');

    // 再切回主树后立刻退后台：不等防抖，paused 强制 flush 最新位置。
    await tester.tap(find.byKey(const Key('worktree-wt-main')));
    await tester.pumpAndSettle();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump();
    location = book.active!.lastLocation;
    expect(location?.worktreeId, 'wt-main', reason: 'paused 应绕过防抖立即落盘');
    // 终端页持有 AppLifecycleListener，必须按合法序列回前台（paused→hidden→inactive→resumed）。
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
  });

  testWidgets('连接态：请求失败显示离线+最近错误，恢复在线自动重拉当前项目', (tester) async {
    final http = _CtxHttp();
    await _pumpHomeWithBook(tester, http);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();

    // 首次成功：状态行显示「已连接」。
    expect(find.text('已连接'), findsOneWidget);

    // worktrees/list 失败：进入离线态——连接药丸含缓存时间，下方整行最近错误。
    http.failWorktreesList = true;
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    expect(find.textContaining('离线 · 缓存于'), findsOneWidget);
    expect(find.textContaining('最后错误：'), findsOneWidget);
    expect(find.text('已连接'), findsNothing);

    // 恢复：下一次成功即视为恢复边沿——自动重拉当前项目权威列表（#3 成功 + #4 恢复刷新）。
    http.failWorktreesList = false;
    await tester.tap(find.byKey(const Key('worktree-wt-main')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 4, reason: '恢复在线后应对当前项目自动重拉一次');
    expect(find.text('已连接'), findsOneWidget);
    expect(find.textContaining('最后错误：'), findsNothing);
  });

  testWidgets('状态行：worktree/session/连接态药丸 + 缓存时间 + 离线整行错误', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          connection: WorkbenchConnectionState.offline(
            lastError: 'boom',
            cachedSince: DateTime(2026, 9, 20, 10, 30),
          ),
          worktreeLabel: 'feat',
          sessionLabel: 's1',
          onSelect: (_) {},
          onBackToProjects: () {},
          child: const Text('project-body'),
        ),
      ),
    );
    expect(find.text('feat'), findsOneWidget);
    expect(find.text('s1'), findsOneWidget);
    expect(find.text('离线 · 缓存于 10:30'), findsOneWidget);
    expect(find.text('最后错误：boom'), findsOneWidget);
    expect(find.byKey(const Key('shell-status-connection')), findsOneWidget);
  });

  testWidgets('状态行：无连接记录不显示连接药丸，标签缺省回落占位', (tester) async {
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
    expect(find.text('worktree'), findsOneWidget);
    expect(find.text('session'), findsOneWidget);
    expect(find.byKey(const Key('shell-status-connection')), findsNothing);
    expect(find.byKey(const Key('shell-status-error')), findsNothing);
  });

  testWidgets('终端全屏（hideAppBar）盖住 AppBar 与状态行，退出恢复', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          hideWorktreeStrip: true,
          hideAppBar: true,
          connection: WorkbenchConnectionState.online(
            lastSucceededAt: DateTime(2026, 9, 20, 10, 30),
          ),
          worktreeStrip: const Text('wt-main'),
          onSelect: (_) {},
          onBackToProjects: () {},
          child: const Text('project-body'),
        ),
      ),
    );
    // 全屏：无 AppBar（Scaffold 无 AppBar 分支），状态行与切换条一并隐藏。
    expect(find.byType(AppBar), findsNothing);
    expect(find.byKey(const Key('shell-status-worktree')), findsNothing);
    expect(find.byKey(const Key('shell-status-connection')), findsNothing);
    expect(find.byKey(const Key('worktree-strip')), findsNothing);
    expect(find.text('project-body'), findsOneWidget);

    // 退出全屏：AppBar + 状态行 + 切换条恢复。
    await tester.pumpWidget(
      MaterialApp(
        home: WorkbenchShell(
          mode: WorkbenchNavMode.project,
          panel: WorkbenchPanel.terminal,
          projectLabel: 'demo',
          connection: WorkbenchConnectionState.online(
            lastSucceededAt: DateTime(2026, 9, 20, 10, 30),
          ),
          worktreeStrip: const Text('wt-main'),
          onSelect: (_) {},
          onBackToProjects: () {},
          child: const Text('project-body'),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byType(AppBar), findsOneWidget);
    expect(find.byKey(const Key('shell-status-worktree')), findsOneWidget);
    expect(find.text('已连接'), findsOneWidget);
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
  });

  testWidgets('P1-3 切到等待输入的终端自动把对应未读标已读；无匹配不调用 markRead', (tester) async {
    final http = _AutoReadHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);

    // boot 聚焦 s1（同 worktree running 优先）→ 命中未读 a1 被 markRead，
    // 成功后刷新徽章（再拉一次列表）。
    await tester.pumpAndSettle();
    await tester.pump();
    await tester.pump();
    expect(http.markReadBodies, [
      ['a1'],
    ]);
    // initState 首拉 + 自动已读 listVisible + 成功后徽章刷新。
    expect(http.attentionListCalls, greaterThanOrEqualTo(3));

    // 切到 s2：epoch 变化，但无匹配 s2 的未读 → 不再 markRead。
    await tester.tap(find.text('s2 · 0 pane'));
    await tester.pumpAndSettle();
    await tester.pump();
    await tester.pump();
    expect(http.markReadBodies, [
      ['a1'],
    ], reason: '无匹配未读时不得重复调用 markRead');

    // low-1 壳层接缝：切会话先对旧会话 unfocus，再 focus 新会话。
    expect(http.focusCalls, contains(('s1', true)));
    expect(http.focusCalls, contains(('s1', false)));
    expect(http.focusCalls, contains(('s2', true)));

    // 离开终端面板（terminal Offstage 常驻不 dispose）：壳层对当前会话补发 unfocus。
    await _gotoPanelViaDrawer(tester, 'files');
    expect(http.focusCalls, contains(('s2', false)));
  });

  testWidgets('P1-2 终端合并激活树：dirty 预检丢弃清快照，合并成功后不对已删树弹确认', (tester) async {
    final http = _TerminalMergeHttp();
    final files = FileWorkspaceController();
    await _pumpHomeWithFiles(tester, http, files);

    await _openDemoProject(tester);

    // 构造 Files 草稿（p1/wt-1 上下文 dirty），随后切到功能树 wt-1。
    files.markDirty(projectId: 'p1', worktreeId: 'wt-1', path: 'src/main.rs');
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    expect(find.text('s1 · 0 pane'), findsOneWidget);

    // 终端发起合并：壳层预检先于合并确认框，按「合并后回落树」比较 → 弹「未保存的文件」。
    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    expect(find.text('未保存的文件'), findsOneWidget);

    // 选择丢弃：清 dirty 快照并放行 → 合并确认框 → 合并成功。
    await tester.tap(find.widgetWithText(TextButton, '丢弃'));
    await tester.pumpAndSettle();
    expect(find.text('确定把「feat/app」合并到主工作区？'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pumpAndSettle();
    expect(http.mergeCalls, 1);
    expect(find.text('合并成功'), findsOneWidget);

    // 权威列表已无 wt-1、激活回落 wt-main；草稿快照已在预检丢弃时清理——
    // 再切一次 worktree 不得对已删 worktree 的残留草稿弹「请保存或丢弃」。
    await tester.tap(find.byKey(const Key('worktree-wt-main')));
    await tester.pumpAndSettle();
    expect(find.text('未保存的文件'), findsNothing);
    expect(find.textContaining('请保存或丢弃'), findsNothing);

    // 让 SnackBar 自动消失，避免测试残留 Timer。
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });
}
