import '../core/lan_http.dart';

class SessionSummary {
  const SessionSummary({
    required this.id,
    required this.projectId,
    required this.name,
    required this.status,
    this.worktreeId,
    this.supportsPanes = false,
    this.paneCount = 0,
  });

  final String id;
  final String projectId;
  final String name;
  final String status;
  final String? worktreeId;

  /// 是否支持真实 tmux pane 操作（split/switch/close/zoom）。
  ///
  /// 后端旧版本可能不下发该字段，宽容解析默认 false，避免误开 pane 菜单。
  final bool supportsPanes;

  /// 当前 terminal window 内的 pane 数量；不支持 panes 的后端回退 0。
  final int paneCount;

  /// 宽容解析：camelCase 为主、snake_case 兜底，缺字段给默认值，旧后端不炸 UI。
  factory SessionSummary.fromJson(Map<String, dynamic> json) => SessionSummary(
        id: json['id'] as String? ?? '',
        projectId: json['projectId'] as String? ?? json['project_id'] as String? ?? '',
        name: json['name'] as String? ?? '',
        status: json['status'] as String? ?? '',
        worktreeId: json['worktreeId'] as String? ?? json['worktree_id'] as String?,
        supportsPanes:
            json['supportsPanes'] == true || json['supports_panes'] == true,
        paneCount: (json['paneCount'] as num?)?.toInt() ??
            (json['pane_count'] as num?)?.toInt() ??
            0,
      );

  /// 展示名：后端未命名时回退到 id。
  String get displayName => name.isEmpty ? id : name;
}

/// 业务逻辑：进入终端页时应优先恢复指定会话，其次选一个仍在运行的会话，而不是盲选第一个。
///
/// Code Logic：优先返回 id 匹配 preferredId 的会话，否则返回首个非 exited 会话；都没有则返回 null，
/// 由调用方决定新建。
SessionSummary? pickPreferredSession(List<SessionSummary> sessions,
    {String? preferredId}) {
  if (preferredId != null && preferredId.isNotEmpty) {
    for (final session in sessions) {
      if (session.id == preferredId) {
        return session;
      }
    }
  }
  for (final session in sessions) {
    if (session.status != 'exited') {
      return session;
    }
  }
  return null;
}

/// sessions/close-pane 的返回：ok 幂等，closedWindow=true 表示最后一个 pane 已关、窗口随之移除。
class ClosePaneResult {
  const ClosePaneResult({required this.sessionId, this.closedWindow = false});

  final String sessionId;
  final bool closedWindow;

  /// 宽容解析：closedWindow 缺失视为 false（仅关 pane 不关窗口）。
  factory ClosePaneResult.fromJson(Map<String, dynamic> json) => ClosePaneResult(
        sessionId: json['sessionId'] as String? ?? json['session_id'] as String? ?? '',
        closedWindow: json['closedWindow'] == true || json['closed_window'] == true,
      );
}

class SessionsClient {
  SessionsClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<SessionSummary>> list(String projectId) async {
    final body = await _http.postDynamic(
      baseUrl,
      '/api/mobile/workbench/sessions/list',
      {'projectId': projectId},
    );
    return asObjectList(body).map(SessionSummary.fromJson).toList();
  }

  /// 创建终端会话；terminal 已完成首帧布局时带实测 cols/rows（clamp 后），
  /// 未布局则两个尺寸都传 null，后端按默认尺寸建 PTY（与 web initialCols/initialRows 语义一致）。
  Future<SessionSummary> create(
    String projectId, {
    String? worktreeId,
    int? initialCols,
    int? initialRows,
  }) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/create',
      {
        'projectId': projectId,
        if (worktreeId != null) 'worktreeId': worktreeId,
        'initialCols': initialCols,
        'initialRows': initialRows,
      },
    );
    return SessionSummary.fromJson(body);
  }

  Future<Map<String, dynamic>> replay(String sessionId, {bool refreshHistory = false}) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/replay',
      {'sessionId': sessionId, 'refreshHistory': refreshHistory},
    );
  }

  Future<void> pasteImage(String sessionId, String dataUrl) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/paste-image',
      {'sessionId': sessionId, 'dataUrl': dataUrl},
    );
  }

  Future<void> focus(String sessionId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/focus',
      {'sessionId': sessionId, 'streamActive': true},
    );
  }

  /// 业务逻辑：手机端也要能关闭终端窗口，释放后端 PTY/tmux attach。
  ///
  /// Code Logic：POST sessions/close，body 仅 sessionId（对齐 web sessions.close）。
  Future<void> close(String sessionId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/close',
      {'sessionId': sessionId},
    );
  }

  /// 业务逻辑：移动端固定向下分屏新增 pane，避免让用户选择方向。
  ///
  /// Code Logic：POST sessions/split-pane，direction 固定 'down'（对齐 web getMobileCreatePaneDirection）。
  Future<void> splitPane(String sessionId, {String direction = 'down'}) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/split-pane',
      {'sessionId': sessionId, 'direction': direction},
    );
  }

  /// 业务逻辑：多 pane window 中需要一键切到下一个 pane。
  ///
  /// Code Logic：POST sessions/switch-pane，仅 sessionId。
  Future<void> switchPane(String sessionId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/switch-pane',
      {'sessionId': sessionId},
    );
  }

  /// 业务逻辑：移动端单屏只能展示一个 pane，切会话后必须把 tmux 分屏收成 zoom 单 pane。
  ///
  /// Code Logic：POST sessions/zoom-pane；后端幂等，失败由调用方静默处理。
  Future<void> zoomPane(String sessionId) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/zoom-pane',
      {'sessionId': sessionId},
    );
  }

  /// 业务逻辑：关闭 pane；返回 closedWindow=true 表示窗口内最后一个 pane 已关、窗口被移除。
  ///
  /// Code Logic：POST sessions/close-pane，body 仅 sessionId。
  Future<ClosePaneResult> closePane(String sessionId) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/close-pane',
      {'sessionId': sessionId},
    );
    return ClosePaneResult.fromJson(body);
  }

  /// 业务逻辑：手机旋转/分屏后本地 xterm 尺寸变化必须同步远端 PTY，否则 TUI 错位。
  ///
  /// Code Logic：POST sessions/resize，body {sessionId, cols, rows}。
  Future<void> resize(String sessionId, int cols, int rows) async {
    await _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/resize',
      {'sessionId': sessionId, 'cols': cols, 'rows': rows},
    );
  }
}
