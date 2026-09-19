import '../core/lan_http.dart';

const kProviderManagerCapability = 'provider-manager.v1';

enum ProviderSupport { ready, unsupported }

class ProviderClient {
  ProviderClient(this._http, this.baseUrl);

  final LanHttpClient _http;
  final String baseUrl;

  /// Never call install-cli from the phone.
  static const installCliPath = '/api/provider-manager/install-cli';
  bool get allowsPhoneCliInstall => false;

  Future<ProviderSupport> probe() async {
    final health = await _http.getJson(baseUrl, '/api/health');
    final version = health['protocol_version'] as int? ?? 0;
    final caps = (health['capabilities'] as List<dynamic>? ?? const [])
        .map((e) => e as String)
        .toList();
    if (version < 1 || !caps.contains(kProviderManagerCapability)) {
      return ProviderSupport.unsupported;
    }
    return ProviderSupport.ready;
  }

  Future<Map<String, dynamic>> summary() {
    return _http.getJson(baseUrl, '/api/provider-manager/summary');
  }

  Future<Map<String, dynamic>> switchProvider({
    required String app,
    required String providerId,
  }) {
    return _http.postJson(
      baseUrl,
      '/api/provider-manager/switch',
      {'app': app, 'id': providerId},
    );
  }
}
