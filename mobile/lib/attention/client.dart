import '../core/lan_http.dart';
import 'filter.dart';

class AttentionClient {
  AttentionClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  Future<List<AttentionItem>> listVisible() async {
    Map<String, dynamic> body;
    try {
      body = asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention/v2'));
    } catch (_) {
      body = asObject(await _http.getDynamic(baseUrl, '/api/mobile/attention'));
    }
    final items = asObjectList(body['items'] ?? body['entries']);
    return filterMobileInboxAttentionItems(
      items.map(AttentionItem.fromJson),
    );
  }
}
