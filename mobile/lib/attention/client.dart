import '../core/lan_http.dart';
import 'filter.dart';

class AttentionClient {
  AttentionClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  /// Business Logic: 手机 Inbox 需要 Agent 投影，优先 v2；旧后端无 v2 时回落 v1。
  /// Code Logic: GET /api/mobile/attention/v2 失败则 GET /api/mobile/attention，
  /// 解析快照 items 并按移动端口径过滤（隐藏 tmux 依赖与 settings 目标）。
  Future<List<AttentionItem>> listVisible() async {
    Map<String, dynamic> body;
    try {
      body = asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention/v2'));
    } catch (_) {
      body = asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention'));
    }
    return _snapshotItems(body);
  }

  /// Business Logic: 手机 Inbox 标已读必须写本机仓储，不能只改前端状态。
  /// Code Logic: POST /api/mobile/attention/mark-read，body `{itemIds}`，返回更新后快照条目。
  Future<List<AttentionItem>> markRead(List<String> itemIds) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/attention/mark-read',
      {'itemIds': itemIds},
    );
    return _snapshotItems(body);
  }

  /// Business Logic: 误标已读时手机也要能单条撤销。
  /// Code Logic: POST /api/mobile/attention/mark-unread，body `{itemIds}`，返回更新后快照条目。
  Future<List<AttentionItem>> markUnread(List<String> itemIds) async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/attention/mark-unread',
      {'itemIds': itemIds},
    );
    return _snapshotItems(body);
  }

  /// Business Logic: 手机顶部「全部已读」与桌面同语义。
  /// Code Logic: POST /api/mobile/attention/mark-all-read，空 body，返回更新后快照条目。
  Future<List<AttentionItem>> markAllRead() async {
    final body = await _http.postJson(
      baseUrl,
      '/api/mobile/attention/mark-all-read',
      const {},
    );
    return _snapshotItems(body);
  }

  /// 从快照响应（list/mark 共用 AttentionSnapshot 形状）解析可见条目。
  List<AttentionItem> _snapshotItems(Map<String, dynamic> body) {
    final items = asObjectList(body['items'] ?? body['entries']);
    return filterMobileInboxAttentionItems(
      items.map(AttentionItem.fromJson),
    );
  }
}
