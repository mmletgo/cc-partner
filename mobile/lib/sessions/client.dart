import '../core/lan_http.dart';

class SessionSummary {
  const SessionSummary({
    required this.id,
    required this.projectId,
    required this.name,
    required this.status,
    this.worktreeId,
    this.cols,
    this.rows,
    this.supportsPanes = false,
    this.paneCount = 0,
  });

  final String id;
  final String projectId;
  final String name;
  final String status;
  final String? worktreeId;

  /// 服务端持久化的 PTY 列数；旧后端不下发时为 null。
  ///
  /// UI 应以此作为 resize 基线：首次 fit 尺寸与该基线相同就不回传 resize（后端把同尺寸
  /// resize 当强制重绘，会把 TUI 末屏抖进 tmux history，web 同语义）。
  final int? cols;

  /// 服务端持久化的 PTY 行数；解析口径同 [cols]。
  final int? rows;

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
        cols: (json['cols'] as num?)?.toInt(),
        rows: (json['rows'] as num?)?.toInt(),
        supportsPanes:
            json['supportsPanes'] == true || json['supports_panes'] == true,
        paneCount: (json['paneCount'] as num?)?.toInt() ??
            (json['pane_count'] as num?)?.toInt() ??
            0,
      );

  /// 展示名：后端未命名时回退到 id。
  String get displayName => name.isEmpty ? id : name;
}

/// 业务逻辑：进入终端页时应优先恢复指定会话；否则按 web selectPreferredMobileSession
/// 的优先级自动落到最合理的窗口，而不是盲选第一个。
///
/// Code Logic：依次选择——(1) preferredId 精确匹配（既有恢复语义，保留）；(2) 同 worktree
/// 且 running；(3) 同 worktree 任意；(4) 全局任意 running；(5) 列表第一个（可为 exited，
/// 与 web sessions[0] 对齐）；空列表返回 null，由调用方决定新建。[worktreeId] 缺省 null 时
/// 跳过 worktree 作用域，保持全局口径。
SessionSummary? pickPreferredSession(
  List<SessionSummary> sessions, {
  String? preferredId,
  String? worktreeId,
}) {
  if (preferredId != null && preferredId.isNotEmpty) {
    for (final session in sessions) {
      if (session.id == preferredId) {
        return session;
      }
    }
  }
  if (worktreeId != null && worktreeId.isNotEmpty) {
    for (final session in sessions) {
      if (session.worktreeId == worktreeId && session.status == 'running') {
        return session;
      }
    }
    for (final session in sessions) {
      if (session.worktreeId == worktreeId) {
        return session;
      }
    }
  }
  for (final session in sessions) {
    if (session.status == 'running') {
      return session;
    }
  }
  if (sessions.isNotEmpty) {
    return sessions.first;
  }
  return null;
}

/// 业务逻辑：终端 tab 需按 project/worktree 作用域过滤（web scopedSessions：
/// projectId 相同且 worktree 匹配；无 worktree 上下文时只按 project 过滤）。
///
/// Code Logic：[projectId] 提供时必须与 session.projectId 相等；[worktreeId] 提供时必须与
/// session.worktreeId 相等；任一为 null 表示该维度不过滤（与 web `!worktree ||` 兜底一致）。
bool sessionMatchesWorktree(
  SessionSummary session,
  String? worktreeId, {
  String? projectId,
}) {
  if (projectId != null && session.projectId != projectId) {
    return false;
  }
  if (worktreeId != null && session.worktreeId != worktreeId) {
    return false;
  }
  return true;
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

  /// replay 快照（签名保持不变：既有 Fake 覆写兼容）。
  Future<Map<String, dynamic>> replay(String sessionId, {bool refreshHistory = false}) {
    return _http.postJson(
      baseUrl,
      '/api/mobile/workbench/sessions/replay',
      {'sessionId': sessionId, 'refreshHistory': refreshHistory},
    );
  }

  /// 历史 hydration（回看 tmux 旧消息）专用变体：等价 `replay(refreshHistory: true)`，
  /// 与 web sessions.hydrateScrollback 同名同语义。
  ///
  /// [timeout] 非空时超时抛 TimeoutException（web SCROLLBACK_HYDRATION_TIMEOUT_MS 10s
  /// abort 同语义，失败可重试）；请求失败同样经 Future 错误上抛，由 UI 展示并允许重试。
  Future<Map<String, dynamic>> hydrateScrollback(
    String sessionId, {
    Duration? timeout,
  }) {
    final future = replay(sessionId, refreshHistory: true);
    return timeout == null ? future : future.timeout(timeout);
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
