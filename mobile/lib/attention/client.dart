import '../core/lan_http.dart';
import 'filter.dart';

/// P2P capability token：attention.v1（与后端 CAPABILITY_ATTENTION_V1 一致）。
const attentionCapabilityV1 = 'attention.v1';

/// P2P capability token：attention.v2（含 Agent 投影，与后端 V2 一致）。
const attentionCapabilityV2 = 'attention.v2';

/// unsupported 态的固定中文文案（对齐 web attention:unsupported）。
const attentionUnsupportedMessage = '当前后端不支持全局 Inbox（缺少 attention.v1）';

/// 后端不支持 attention 能力时的专用错误。
///
/// Business Logic: 页面需要区分「后端太旧缺 attention.v1」与普通网络/协议失败，
/// 前者要显示专用横幅并抑制 loading/error/empty，不能靠解析错误文案猜。
/// Code Logic: 带固定中文 message 的异常类型，页面用 `is AttentionUnsupportedError` 判定。
class AttentionUnsupportedError implements Exception {
  AttentionUnsupportedError([this.message = attentionUnsupportedMessage]);

  final String message;

  @override
  String toString() => message;
}

/// health 响应中与能力探测相关的字段（snake_case，对齐 web AttentionHealthProtocolInfo）。
class AttentionHealthInfo {
  const AttentionHealthInfo({required this.protocolVersion, required this.capabilities});

  /// 协议版本；缺省/旧后端按 0（不支持）处理。
  final int protocolVersion;
  final List<String> capabilities;

  /// Business Logic: 探测必须宽容——旧后端可能缺 protocol_version/capabilities，
  /// 安全回落为不支持（对齐 web supportsAttentionV1/V2）。
  /// Code Logic: version>=1 且 capabilities 精确包含对应 token 才算支持。
  bool get supportsV1 => _has(attentionCapabilityV1);

  /// 是否宣告 attention.v2。
  bool get supportsV2 => _has(attentionCapabilityV2);

  bool _has(String token) =>
      protocolVersion >= 1 && capabilities.contains(token);

  /// Business Logic: health 解析必须宽容，缺字段不能让探测崩溃。
  /// Code Logic: 宽容读取 protocol_version（非数字按 0）与 capabilities（非列表按空）。
  factory AttentionHealthInfo.fromJson(Map<String, dynamic> json) {
    final version = json['protocol_version'];
    final caps = json['capabilities'];
    return AttentionHealthInfo(
      protocolVersion: version is num ? version.toInt() : 0,
      capabilities: caps is List ? caps.whereType<String>().toList() : const [],
    );
  }
}

class AttentionClient {
  AttentionClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  /// Business Logic: 手机 Inbox 先探测 /api/health 能力——缺 attention.v1 时明确
  /// 抛 AttentionUnsupportedError（对齐 web assertAttentionCapability，禁止猜测旧接口）；
  /// 能力就绪后优先 v2（Agent 投影），v2 失败回落 v1，仅支持 v1 时直接走 v1。
  /// Code Logic: GET /api/health 解析 AttentionHealthInfo → 双不支持抛 unsupported →
  /// supportsV2 时 try v2 catch 回落 v1 → 否则 GET /api/mobile/attention。
  Future<List<AttentionItem>> listVisible() async {
    // health 不可达是网络/服务故障，原样向上抛（不当作 unsupported）。
    final health = AttentionHealthInfo.fromJson(
      await _http.getJson(baseUrl, '/api/health'),
    );
    if (!health.supportsV1 && !health.supportsV2) {
      throw AttentionUnsupportedError();
    }
    if (health.supportsV2) {
      try {
        return _snapshotItems(
          asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention/v2')),
        );
      } catch (_) {
        // v2 失败回落 v1（对齐 web listAttentionSnapshotHttp）。
      }
    }
    return _snapshotItems(
      asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention')),
    );
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
