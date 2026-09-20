import '../core/lan_http.dart';

const kProviderManagerCapability = 'provider-manager.v1';

/// Business Logic: 分组标题要显示用户认识的产品名，而不是后端 app 枚举小写串；
/// 映射对齐 web `providerManager:apps.*`（权威表见 web/src/i18n/locales/zh/
/// providerManager.json，与后端 AgentApp 枚举一一对应）。
/// Code Logic: 已知 app 枚举 → 产品名的固定映射；未知 app 回退原值。
String providerAppLabel(String app) {
  switch (app) {
    case 'claude':
      return 'Claude Code';
    case 'codex':
      return 'Codex';
    case 'gemini':
      return 'Gemini CLI';
    case 'opencode':
      return 'OpenCode';
    case 'hermes':
      return 'Hermes';
    case 'openclaw':
      return 'OpenClaw';
    default:
      return app;
  }
}

enum ProviderSupport { ready, unsupported }

class ProviderApp {
  const ProviderApp({
    required this.app,
    required this.providers,
    this.currentProviderId,
  });

  final String app;
  final List<ProviderEntry> providers;
  final String? currentProviderId;

  /// Business Logic: 分组副标题「当前：X」要把 currentProviderId 解析成 provider 名
  /// （对齐 web AppSection：current 是什么显示什么，找不到不渲染副标题）。
  /// Code Logic: providers 中查 id == currentProviderId 的条目取 name；未命中返回 null。
  String? get currentProviderName {
    final id = currentProviderId;
    if (id == null || id.isEmpty) {
      return null;
    }
    for (final provider in providers) {
      if (provider.id == id) {
        return provider.name;
      }
    }
    return null;
  }
}

class ProviderEntry {
  const ProviderEntry({
    required this.id,
    required this.name,
    required this.isCurrent,
    this.category,
  });

  final String id;
  final String name;
  final bool isCurrent;
  final String? category;
}

/// 后端 cc-switch CLI 可用性（对齐 web ProviderManagerSummary.cli）。
class ProviderCliStatus {
  const ProviderCliStatus({required this.available});

  final bool available;
}

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
      {'app': app, 'providerId': providerId},
    );
  }

  /// Business Logic: 手机不装 CLI；CLI 缺失时切换必须整体禁用（对齐 web
  /// MobileProviderPanel 的 `summary.cli.available=false` 门控）。
  /// Code Logic: 宽容解析 summary 的 `cli` 对象——缺 cli 字段视为旧后端，
  /// 按 available=true 降级放行；cli 内 available 非 true 一律视为缺失。
  ProviderCliStatus cliFromSummary(Map<String, dynamic> summary) {
    final cli = summary['cli'];
    if (cli is! Map) {
      return const ProviderCliStatus(available: true);
    }
    return ProviderCliStatus(available: cli['available'] == true);
  }

  List<ProviderApp> appsFromSummary(Map<String, dynamic> summary) {
    return asObjectList(summary['apps']).map((app) {
      final providers = asObjectList(app['providers']).map((item) {
        return ProviderEntry(
          id: item['id'] as String? ?? '',
          name: item['name'] as String? ?? item['id'] as String? ?? '',
          isCurrent: item['isCurrent'] == true,
          category: item['category'] as String?,
        );
      }).toList();
      return ProviderApp(
        app: app['app'] as String? ?? '',
        providers: providers,
        currentProviderId: app['currentProviderId'] as String?,
      );
    }).toList();
  }
}
