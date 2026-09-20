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

/// worktrees 页删除收敛测试假 HTTP：单项目 p1 三棵树（wt-main 主树 + wt-1/wt-2
/// 功能树）+ 绑定 wt-1 的会话；worktrees/remove 返回成功 envelope 并同步把该树
/// 从权威列表移除，计数 worktrees/sessions 请求次数（终端会话刷新令牌断言）。
class _WorktreesMutationHttp extends LanHttpClient {
  int worktreesListCalls = 0;
  int sessionsListCalls = 0;

  List<Map<String, dynamic>> trees = [
    {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo'},
    {
      'id': 'wt-1',
      'name': 'feat',
      'branch': 'feat/app',
      'isMain': false,
      'path': '/repo/.worktrees/feat-app',
    },
    {
      'id': 'wt-2',
      'name': 'chore',
      'branch': 'chore/x',
      'isMain': false,
      'path': '/repo/.worktrees/chore-x',
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
      sessionsListCalls += 1;
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running', 'worktreeId': 'wt-1'},
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
        'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)],
      };
    }
    if (path == '/api/mobile/workbench/worktrees/remove') {
      final id = body['worktreeId'] as String?;
      trees = [
        for (final tree in trees)
          if (tree['id'] != id) Map<String, dynamic>.from(tree),
      ];
      return {'kind': 'succeeded'};
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
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

  /// 一次性 merge 挂起闸门：非空时 merge 请求挂起直到 complete
  /// （壳层 worktree 操作互斥锁的时序测试用；null 表示不挂起）。
  Completer<void>? mergeGate;

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
      // 在途挂起闸门：模拟 merge 请求仍在网络中（互斥锁测试控制完成时机）。
      final gate = mergeGate;
      if (gate != null) {
        await gate.future;
      }
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

/// low Git 页 commit 收敛测试假 HTTP：两棵树（wt-main 主树 + wt-1 功能树）+
/// 绑定 wt-1 的会话；worktrees/commit 返回成功 envelope，计数 worktrees/list
/// 与 sessions/list 调用次数（断言 commit 成功后壳层 _loadWorktrees 被触发 +
/// 终端会话刷新令牌 bump 驱动 prune 拉取）。
class _GitMutationShellHttp extends LanHttpClient {
  int worktreesListCalls = 0;
  int sessionsListCalls = 0;
  int commitCalls = 0;

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
      sessionsListCalls += 1;
      return [
        {'id': 's1', 'projectId': 'p1', 'name': 's1', 'status': 'running', 'worktreeId': 'wt-1'},
      ];
    }
    if (path == '/api/mobile/workbench/git/commits') {
      return <Map<String, dynamic>>[];
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
          {'id': 'wt-main', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo'},
          {
            'id': 'wt-1',
            'name': 'feat',
            'branch': 'feat/app',
            'isMain': false,
            'path': '/repo/.worktrees/feat-app',
            'status': {
              'branch': 'feat/app',
              'changed': 2,
              'ahead': 0,
              'behind': 0,
              'conflicts': 0,
              'clean': false,
              'canPush': true,
            },
          },
        ],
      };
    }
    if (path == '/api/mobile/workbench/worktrees/commit') {
      commitCalls += 1;
      return {'kind': 'succeeded', 'value': <String, dynamic>{}};
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
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
/// 跨项目终端缓冲常驻 + 状态行会话名测试假 HTTP：两个项目各一棵主树 + 一个会话
/// （会话 name 与 id 不同，用于断言状态行显示会话名而非原始 id）；
/// sessions/list 按请求 projectId 返回对应会话并记录，sessions/replay 记录重放的
/// sessionId（跨项目缓冲命中时不重放）；focus/zoom 幂等成功；openWebSocket 挂起。
class _CrossProjectHttp extends LanHttpClient {
  final List<String> listedProjectIds = [];
  final List<String> replayedSessionIds = [];

  static const Map<String, List<Map<String, dynamic>>> _treesByProject = {
    'p1': [
      {'id': 'wt-p1', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo1'},
    ],
    'p2': [
      {'id': 'wt-p2', 'name': 'main', 'branch': 'main', 'isMain': true, 'path': '/repo2'},
    ],
  };

  static const Map<String, String> _sessionNameByProject = {
    'p1': '会话一',
    'p2': '会话二',
  };

  Map<String, dynamic> _sessionFor(String projectId) => {
        'id': 's-$projectId',
        'projectId': projectId,
        'name': _sessionNameByProject[projectId],
        'status': 'running',
        'worktreeId': 'wt-$projectId',
      };

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
          {'id': 'p1', 'name': 'demo', 'path': '/repo1', 'kind': 'local'},
          {'id': 'p2', 'name': 'other', 'path': '/repo2', 'kind': 'local'},
        ],
      };
    }
    throw LanHttpException(404, 'not found: $path');
  }

  @override
  Future<dynamic> postDynamic(String baseUrl, String path, Map<String, dynamic> body) async {
    if (path == '/api/mobile/workbench/sessions/list') {
      final projectId = body['projectId'] as String? ?? 'p1';
      listedProjectIds.add(projectId);
      return [_sessionFor(projectId)];
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
      final trees = _treesByProject[body['projectId'] as String? ?? 'p1']!;
      return {
        'ok': true,
        'worktrees': [for (final tree in trees) Map<String, dynamic>.from(tree)],
      };
    }
    if (path == '/api/mobile/workbench/sessions/replay') {
      final sessionId = body['sessionId'] as String;
      replayedSessionIds.add(sessionId);
      return {'sessionId': sessionId, 'buffer': 'boot-ok', 'lastSeq': 0};
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
Future<AddressBook> _pumpHomeWithBook(WidgetTester tester, LanHttpClient http) async {
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

/// 经 Drawer「项目」返回项目列表（只切面板，项目上下文保留）。
Future<void> _backToProjectsViaDrawer(WidgetTester tester) async {
  await tester.tap(find.byTooltip('Open navigation menu'));
  await tester.pumpAndSettle();
  await tester.tap(find.byKey(const Key('nav-back-projects')));
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
    // 切树：终端页常驻不重建，didUpdateWidget 检测 worktree 变化重新 boot。
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

    // 再返回列表，点不同项目：走完整切换（拉新列表 + 终端 didUpdateWidget 重新 boot）。
    await backToProjects();
    await tester.tap(find.byKey(const Key('project-row-p2')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 3, reason: '切换项目应拉取新项目 worktrees');
    expect(http.sessionsListCalls, 3, reason: '切换项目终端按新项目重新 boot');
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

  testWidgets('P1-3 聚焦同一会话期间快照新增的 needsInput 未读也立即标已读', (tester) async {
    final http = _AutoReadHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);

    // boot 聚焦 s1，初始未读 a1 已被标已读。
    await tester.pumpAndSettle();
    await tester.pump();
    await tester.pump();
    expect(http.markReadBodies, [
      ['a1'],
    ]);

    // 用户停留在 s1（epoch 不变）期间 Agent 新发起等待输入：快照新增 a3。
    http.attentionItems.add({
      'id': 'a3',
      'sourceKind': 'agentNeedsInput',
      'title': '新到达等待输入',
      'target': {'kind': 'agentSession', 'projectId': 'p1', 'terminalSessionId': 's1'},
    });

    // 无任何会话/面板切换：10s 徽章轮询 tick 拉到新快照即重查并标已读（对齐 web
    // 快照驱动分支），成功后徽章刷新为空跑收敛。
    await tester.pump(const Duration(seconds: 10));
    await tester.pumpAndSettle();
    await tester.pump();
    await tester.pump();
    expect(http.markReadBodies, [
      ['a1'],
      ['a3'],
    ]);
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

  testWidgets('跨项目终端缓冲常驻：切回原项目不清屏不重放；状态行显示会话名而非 id',
      (tester) async {
    final http = _CrossProjectHttp();
    await _pumpHome(tester, http);

    // 打开 p1：终端 boot（list p1 + replay s-p1）。
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(http.listedProjectIds, ['p1']);
    expect(http.replayedSessionIds, ['s-p1']);
    // 状态行会话药丸 = 激活会话显示名（对齐 web session={activeSession?.name}），
    // 不再是原始会话 id。
    expect(
      tester.widget<Text>(
        find.descendant(
          of: find.byKey(const Key('shell-status-session')),
          matching: find.byType(Text),
        ),
      ).data,
      '会话一',
    );
    expect(find.text('s-p1'), findsNothing);

    // 返回项目列表（终端面板常驻不销毁），打开 p2：终端页不卸载，
    // didUpdateWidget 断开旧上下文并按 p2 重新 boot。
    await _backToProjectsViaDrawer(tester);
    await tester.tap(find.text('other'));
    await tester.pumpAndSettle();
    expect(http.listedProjectIds, ['p1', 'p2'], reason: '切项目后终端页按新项目重新拉会话');
    expect(http.replayedSessionIds, ['s-p1', 's-p2'], reason: 'p2 会话无缓冲 → replay');
    expect(
      tester.widget<Text>(
        find.descendant(
          of: find.byKey(const Key('shell-status-session')),
          matching: find.byType(Text),
        ),
      ).data,
      '会话二',
    );

    // 切回 p1：命中跨项目常驻缓冲 → 不重放（不清屏）。
    await _backToProjectsViaDrawer(tester);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(http.listedProjectIds, ['p1', 'p2', 'p1'], reason: '切回 p1 仍按 p1 重新拉会话列表');
    expect(http.replayedSessionIds, ['s-p1', 's-p2'],
        reason: '切回 p1 命中跨项目常驻缓冲，不重放 s-p1');
    expect(find.text('会话一 · 0 pane'), findsOneWidget, reason: '切回后 chip 列表回到 p1 会话');
  });

  testWidgets('low-1 worktrees 详情失败：同项目行完整重拉；错误条「重试」重拉成功后清除',
      (tester) async {
    final http = _CtxHttp();
    http.failWorktreesList = true;
    await _pumpHomeWithBook(tester, http);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 1, reason: '首次打开加载失败（详情 error）');

    // 返回项目列表：详情 error 态显示错误条 + 重试（对齐 web projectDetailRetry）。
    await _backToProjectsViaDrawer(tester);
    expect(find.byKey(const Key('projects-detail-error')), findsOneWidget);
    expect(find.byKey(const Key('projects-detail-retry')), findsOneWidget);

    // 详情 error 时点同一项目行：不再早退，完整重拉（仍失败）。
    await tester.tap(find.byKey(const Key('project-row-p1')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 2, reason: '详情失败后点同项目应重拉（恢复入口）');

    await _backToProjectsViaDrawer(tester);
    expect(find.byKey(const Key('projects-detail-error')), findsOneWidget);

    // 恢复后点「重试」：重拉成功 → 详情 ready → 错误条与重试入口消失。
    http.failWorktreesList = false;
    await tester.tap(find.byKey(const Key('projects-detail-retry')));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 4,
        reason: '「重试」重跑详情加载（#3），成功后连接态恢复在线边沿再自动重拉一次（#4）');
    expect(find.byKey(const Key('projects-detail-error')), findsNothing);
    expect(find.byKey(const Key('projects-detail-retry')), findsNothing);

    // 详情 ready 后点同项目行：早退——直接回终端且不重拉。
    await tester.tap(find.byKey(const Key('project-row-p1')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
    expect(http.worktreesListCalls, 4, reason: '详情 ready 后同项目早退不重拉');
  });

  testWidgets('low-2 Worktrees 页删除成功回调壳层：激活树删除回落主树、strip 同步移除',
      (tester) async {
    final http = _WorktreesMutationHttp();
    await _pumpHomeWithBook(tester, http);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);

    // 切到功能树 wt-1（激活树）。
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();
    expect(
      tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-1'))).selected,
      isTrue,
    );

    // 进入 Worktrees 页删除激活树 wt-1。
    await _gotoPanelViaDrawer(tester, 'worktrees');
    await tester.tap(find.byKey(const Key('worktree-delete-wt-1')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();

    // 壳层收敛：strip 不再含已删树，激活按主树优先回落 wt-main。
    await _gotoPanelViaDrawer(tester, 'terminal');
    expect(find.byKey(const Key('worktree-wt-1')), findsNothing);
    expect(find.byKey(const Key('worktree-wt-2')), findsOneWidget);
    expect(
      tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-main'))).selected,
      isTrue,
      reason: '删除激活树后壳层应回落主树',
    );
    final sessionsAfterActiveRemove = http.sessionsListCalls;

    // 删除非激活树 wt-2：strip 同步移除；令牌 bump 驱动终端会话权威刷新。
    await _gotoPanelViaDrawer(tester, 'worktrees');
    await tester.tap(find.byKey(const Key('worktree-delete-wt-2')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '移除'));
    await tester.pumpAndSettle();
    await _gotoPanelViaDrawer(tester, 'terminal');
    expect(find.byKey(const Key('worktree-wt-2')), findsNothing);
    expect(find.byKey(const Key('worktree-wt-main')), findsOneWidget);
    expect(http.sessionsListCalls, greaterThan(sessionsAfterActiveRemove),
        reason: 'worktrees 变更后壳层 bump 会话刷新令牌，终端拉权威列表');

    // 泵过 SnackBar 时长，避免残留 Timer。
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });

  testWidgets('low-7 跨项目切换 dirty 三选：取消不切换；丢弃后切换且清 dirty 快照',
      (tester) async {
    final http = _CtxHttp();
    final files = FileWorkspaceController();
    // 模拟打开中的草稿页注册保存委托（三选中的「保存」因此可见）。
    files.saveHandler = () async {
      files.markClean();
      return true;
    };
    await _pumpHomeWithFiles(tester, http, files);
    await tester.tap(find.text('demo'));
    await tester.pumpAndSettle();
    expect(http.worktreesListCalls, 1);
    files.markDirty(projectId: 'p1', worktreeId: 'wt-main', path: 'src/main.rs');

    // 返回项目列表，点不同项目 p2：先弹「取消/丢弃/保存」三选（对齐 web
    // confirmFileContextSwitch 的跨项目预检）。
    await _backToProjectsViaDrawer(tester);
    await tester.tap(find.byKey(const Key('project-row-p2')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('project-switch-dirty-dialog')), findsOneWidget);
    expect(find.byKey(const Key('project-switch-dirty-cancel')), findsOneWidget);
    expect(find.byKey(const Key('project-switch-dirty-discard')), findsOneWidget);
    expect(find.byKey(const Key('project-switch-dirty-save')), findsOneWidget);

    // 取消：中止切换——激活项目仍是 p1，未拉取 p2 的列表。
    await tester.tap(find.byKey(const Key('project-switch-dirty-cancel')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('project-row-active-p1')), findsOneWidget);
    expect(http.worktreesListCalls, 1, reason: '取消后不得发起切换');
    expect(files.snapshot.dirty, isTrue, reason: '取消不清 dirty 快照');

    // 再点 p2 并选择丢弃：清 dirty 快照后走完整切换。
    await tester.tap(find.byKey(const Key('project-row-p2')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('project-switch-dirty-discard')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);
    expect(
      tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-p2-main'))).selected,
      isTrue,
    );
    expect(http.worktreesListCalls, 2, reason: '丢弃确认后切换到 p2 并拉取其列表');
    expect(files.snapshot.dirty, isFalse, reason: '丢弃路径应清 dirty 快照');
  });

  testWidgets('low Git 页 commit 成功触发壳层权威收敛：_loadWorktrees 重拉 + 会话令牌 bump',
      (tester) async {
    final http = _GitMutationShellHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);

    // 切到 Git 面板（非常驻：进入即重挂重拉）。
    await _gotoPanelViaDrawer(tester, 'git');
    await tester.pumpAndSettle();
    final listsBeforeCommit = http.worktreesListCalls;
    final sessionsBeforeCommit = http.sessionsListCalls;

    // Git 页发起提交：说明框留空（AI 生成）→ 提交成功。
    await tester.tap(find.byKey(const Key('git-action-commit')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '提交').last);
    await tester.pumpAndSettle();
    expect(http.commitCalls, 1);

    // commit 成功后：Git 页自身 _refresh（+1）与壳层 onWorktreesMutated →
    // _loadWorktrees（+1）都拉权威列表（对齐 web refreshAfterAction('commit')）；
    // 终端会话刷新令牌 bump 驱动 prune 拉取（strip/状态行/终端合并门控收敛）。
    expect(
      http.worktreesListCalls,
      listsBeforeCommit + 2,
      reason: 'commit 成功后壳层应重拉权威 worktrees（页内刷新 + 壳层收敛各一次）',
    );
    expect(http.sessionsListCalls, greaterThan(sessionsBeforeCommit),
        reason: '壳层 bump 会话刷新令牌，终端拉权威会话列表');

    // 泵过 SnackBar 时长，避免残留 Timer。
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });

  testWidgets('low 终端合并挂起期间壳层互斥：strip chip 拒绝 + worktrees 卡片禁用，完成恢复',
      (tester) async {
    final http = _TerminalMergeHttp();
    await _pumpHome(tester, http);
    await _openDemoProject(tester);

    // 选中功能树 wt-1（终端聚焦绑定会话 s1）。
    await tester.tap(find.byKey(const Key('worktree-wt-1')));
    await tester.pumpAndSettle();

    // 发起合并并停在在途：merge 请求挂起 → 壳层互斥锁置忙。
    http.mergeGate = Completer<void>();
    await tester.tap(find.byTooltip('合并'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '合并'));
    await tester.pump();
    expect(http.mergeCalls, 1);

    // 在途：点 strip 主树 chip 被拒绝并提示，选中态不变（合并将删源树，禁止切走）。
    await tester.tap(find.byKey(const Key('worktree-wt-main')));
    await tester.pumpAndSettle();
    expect(find.textContaining('正在处理 worktree 操作，请稍候'), findsOneWidget);
    expect(
      tester.widget<ChoiceChip>(find.byKey(const Key('worktree-wt-1'))).selected,
      isTrue,
    );

    // 在途：切到 worktrees 面板，卡片选择与合并/移除入口一并禁用（externalBusy）。
    await _gotoPanelViaDrawer(tester, 'worktrees');
    expect(
      tester.widget<ListTile>(find.byKey(const Key('worktree-item-wt-main'))).onTap,
      isNull,
      reason: '合并在途时 worktrees 页卡片选择应禁用',
    );
    expect(
      tester.widget<IconButton>(find.byKey(const Key('worktree-merge-wt-1'))).onPressed,
      isNull,
    );
    expect(
      tester.widget<IconButton>(find.byKey(const Key('worktree-delete-wt-1'))).onPressed,
      isNull,
    );

    // 泵过拒绝提示的停留时长，避免后续断言被旧 SnackBar 干扰。
    await tester.pump(const Duration(seconds: 5));

    // 合并完成（成功）：壳层收敛 + 互斥释放。
    http.mergeGate!.complete();
    await tester.pumpAndSettle();
    expect(
      tester.widget<ListTile>(find.byKey(const Key('worktree-item-wt-main'))).onTap,
      isNotNull,
      reason: '合并完成后卡片选择应恢复',
    );

    // 恢复后点卡片正常切换进终端（无拒绝提示）。
    await tester.tap(find.byKey(const Key('worktree-item-wt-main')));
    await tester.pumpAndSettle();
    expect(find.textContaining('正在处理 worktree 操作，请稍候'), findsNothing);
    expect(find.byKey(const Key('worktree-strip')), findsOneWidget);

    // 泵过「合并成功」SnackBar 时长，避免残留 Timer。
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
  });
}
