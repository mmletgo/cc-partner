import 'dart:convert';
import 'dart:io';

import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/provider/client.dart';
import 'package:test/test.dart';

void main() {
  test('phone never installs cc-switch CLI', () {
    expect(ProviderClient(LanHttpClient(), 'http://127.0.0.1:1').allowsPhoneCliInstall, isFalse);
  });

  test('providerAppLabel maps the cc-switch app table and falls back to raw', () {
    // 已知枚举 → 产品名（与 web providerManager:apps.* 与后端 AgentApp 一一对应）。
    expect(providerAppLabel('claude'), 'Claude Code');
    expect(providerAppLabel('codex'), 'Codex');
    expect(providerAppLabel('gemini'), 'Gemini CLI');
    expect(providerAppLabel('opencode'), 'OpenCode');
    expect(providerAppLabel('hermes'), 'Hermes');
    expect(providerAppLabel('openclaw'), 'OpenClaw');
    // 未知 app 回退原值。
    expect(providerAppLabel('someNewApp'), 'someNewApp');
    expect(providerAppLabel(''), '');
  });

  test('currentProviderName resolves the subtitle target or returns null', () {
    const app = ProviderApp(
      app: 'claude',
      currentProviderId: 'p-b',
      providers: [
        ProviderEntry(id: 'p-a', name: 'A', isCurrent: true),
        ProviderEntry(id: 'p-b', name: 'B', isCurrent: false),
      ],
    );
    // current 是什么显示什么（对齐 web：按 currentProviderId 解析，而非 isCurrent 行）。
    expect(app.currentProviderName, 'B');
    // currentProviderId 缺失或解析不到：null（副标题不渲染）。
    expect(
      const ProviderApp(app: 'claude', providers: [
        ProviderEntry(id: 'p-a', name: 'A', isCurrent: true),
      ]).currentProviderName,
      isNull,
    );
    expect(
      const ProviderApp(
        app: 'claude',
        currentProviderId: 'p-missing',
        providers: [ProviderEntry(id: 'p-a', name: 'A', isCurrent: true)],
      ).currentProviderName,
      isNull,
    );
  });

  test('cliFromSummary parses cli.available tolerantly', () {
    final client = ProviderClient(LanHttpClient(), 'http://127.0.0.1:1');
    // cli.available=false：CLI 缺失，切换必须禁用。
    expect(
      client.cliFromSummary({
        'cli': {'available': false},
      }).available,
      isFalse,
    );
    // cli.available=true：可用。
    expect(
      client.cliFromSummary({
        'cli': {'available': true},
      }).available,
      isTrue,
    );
    // 缺 cli 对象：旧后端宽容降级为可用。
    expect(client.cliFromSummary({'apps': <dynamic>[]}).available, isTrue);
    // cli 内缺 available：按缺失处理。
    expect(
      client.cliFromSummary({
        'cli': <String, dynamic>{},
      }).available,
      isFalse,
    );
  });

  test('missing capability is unsupported', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'ok': true,
          'protocol_version': 1,
          'capabilities': ['attention.v2'],
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    expect(await client.probe(), ProviderSupport.unsupported);
  });

  test('provider-manager.v1 is ready', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'ok': true,
          'protocol_version': 1,
          'capabilities': ['provider-manager.v1'],
        }),
      );
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    expect(await client.probe(), ProviderSupport.ready);
  });

  test('switch posts app and providerId and never calls install-cli', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final paths = <String>[];
    Map<String, dynamic>? switchBody;
    server.listen((request) async {
      paths.add(request.uri.path);
      final raw = await utf8.decodeStream(request);
      if (request.uri.path.endsWith('/switch') && raw.isNotEmpty) {
        switchBody = jsonDecode(raw) as Map<String, dynamic>;
      }
      request.response.headers.contentType = ContentType.json;
      if (request.uri.path.endsWith('/summary')) {
        request.response.write(
          jsonEncode({
            'apps': [
              {
                'app': 'claude',
                'currentProviderId': 'p-a',
                'providers': [
                  {'id': 'p-a', 'name': 'A', 'isCurrent': true},
                  {'id': 'p-b', 'name': 'B', 'isCurrent': false},
                ],
              },
            ],
          }),
        );
      } else {
        request.response.write(jsonEncode({'ok': true}));
      }
      await request.response.close();
    });
    final http = LanHttpClient();
    addTearDown(http.close);
    final client = ProviderClient(http, 'http://127.0.0.1:${server.port}');
    final summary = await client.summary();
    expect(client.appsFromSummary(summary).single.app, 'claude');
    await client.switchProvider(app: 'claude', providerId: 'p-b');
    expect(paths, isNot(contains(ProviderClient.installCliPath)));
    expect(switchBody, {'app': 'claude', 'providerId': 'p-b'});
  });
}
