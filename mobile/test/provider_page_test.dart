import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/provider/client.dart';
import 'package:cc_partner_mobile/ui/provider_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 首次 probe 可配置失败，用于验证错误态与重试；summary 返回固定 app 列表，
/// 可切换为空 apps（验证 noProviders 空态提示）。
class _FakeProviderClient extends ProviderClient {
  _FakeProviderClient({
    this.failFirstProbe = true,
    this.cliAvailable,
    this.emptyApps = false,
    this.appName = 'claude',
    this.currentProviderId = 'p-a',
  }) : super(LanHttpClient(), 'http://127.0.0.1:1');

  final bool failFirstProbe;

  /// 非 null 时 summary 带 `cli.available`；null 时缺 cli 字段（旧后端）。
  final bool? cliAvailable;

  /// true 时 summary 的 apps 为空数组（cc-switch 未配置任何 provider）。
  final bool emptyApps;

  /// summary 返回的 app 枚举值（产品名映射用例可换成未知枚举）。
  final String appName;

  /// summary 返回的当前 provider id；置 null 验证副标题不渲染。
  final String? currentProviderId;
  int probeCalls = 0;

  @override
  Future<ProviderSupport> probe() async {
    probeCalls += 1;
    if (failFirstProbe && probeCalls == 1) {
      throw LanHttpException(503, 'health down');
    }
    return ProviderSupport.ready;
  }

  @override
  Future<Map<String, dynamic>> summary() async => {
        if (cliAvailable != null)
          'cli': {'available': cliAvailable},
        'apps': [
          if (!emptyApps)
            {
              'app': appName,
              'currentProviderId': currentProviderId,
              'providers': [
                {'id': 'p-a', 'name': 'A', 'isCurrent': true},
                {'id': 'p-b', 'name': 'B', 'isCurrent': false},
              ],
            },
        ],
      };
}

Future<void> _pumpPage(WidgetTester tester, ProviderClient client) async {
  final book = AddressBook(store: MemoryAddressBookStore());
  await book.addFromInput(
    '127.0.0.1:62116',
    probe: (_) async => throw Exception('skip'),
    forceIfUnreachable: true,
  );
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ProviderPage(book: book, http: LanHttpClient(), client: client),
      ),
    ),
  );
}

void main() {
  testWidgets('probe failure shows 重新检测 and retry recovers the list', (tester) async {
    final client = _FakeProviderClient();
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('provider-retry')), findsOneWidget);
    expect(find.text('重新检测'), findsOneWidget);

    await tester.tap(find.byKey(const Key('provider-retry')));
    await tester.pumpAndSettle();
    expect(client.probeCalls, 2);
    expect(find.text('A'), findsOneWidget);
    expect(find.text('B'), findsOneWidget);
    expect(find.byKey(const Key('provider-retry')), findsNothing);
  });

  testWidgets('refresh button re-runs the probe', (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();
    expect(client.probeCalls, 1);
    expect(find.byKey(const Key('provider-refresh')), findsOneWidget);

    await tester.tap(find.byKey(const Key('provider-refresh')));
    await tester.pumpAndSettle();
    expect(client.probeCalls, 2);
    expect(find.text('A'), findsOneWidget);
  });

  testWidgets('cli missing shows warning card and disables every switch button',
      (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false, cliAvailable: false);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 顶部警示卡：标题 + hint（对齐 web cliMissing / cliMissingHint 文案）。
    expect(find.byKey(const Key('provider-cli-missing')), findsOneWidget);
    expect(find.text('未安装 cc-switch CLI，切换功能已禁用。'), findsOneWidget);
    expect(find.text('安装 cc-switch CLI 会与你现有的 cc-switch 共享同一份数据，不会影响 GUI。'),
        findsOneWidget);

    // 全部「切换」按钮禁用；已知当前 provider 的「当前」chip 保留。
    final switchButton = tester.widget<TextButton>(
      find.ancestor(
        of: find.text('切换'),
        matching: find.byType(TextButton),
      ),
    );
    expect(switchButton.onPressed, isNull);
    expect(find.text('当前'), findsOneWidget);
  });

  testWidgets('cli present keeps switch buttons enabled', (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false, cliAvailable: true);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('provider-cli-missing')), findsNothing);
    final switchButton = tester.widget<TextButton>(
      find.ancestor(
        of: find.text('切换'),
        matching: find.byType(TextButton),
      ),
    );
    expect(switchButton.onPressed, isNotNull);
  });

  testWidgets('summary without any provider shows the noProviders hint', (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false, emptyApps: true);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 对齐 web providerManager:noProviders 的 info 文案；与顶部刷新按钮并存。
    expect(find.byKey(const Key('provider-empty')), findsOneWidget);
    expect(
      find.text('未找到已配置的 provider，请先在 cc-switch 中配置 provider。'),
      findsOneWidget,
    );
    expect(find.byKey(const Key('provider-refresh')), findsOneWidget);
  });

  testWidgets('summary with providers keeps the noProviders hint hidden', (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('provider-empty')), findsNothing);
    expect(
      find.text('未找到已配置的 provider，请先在 cc-switch 中配置 provider。'),
      findsNothing,
    );
    expect(find.text('A'), findsOneWidget);
  });

  testWidgets('app group title shows the product name with a current-provider subtitle',
      (tester) async {
    final client = _FakeProviderClient(failFirstProbe: false);
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 分组标题用产品名（对齐 web providerManager:apps.claude = Claude Code），
    // 不再直接显示原始 app 枚举。
    expect(find.text('claude'), findsNothing);
    expect(find.byKey(const Key('provider-app-claude')), findsOneWidget);
    expect(find.text('Claude Code'), findsOneWidget);
    // 副标题「当前：X」按 currentProviderId 解析（对齐 web AppSection）。
    expect(find.byKey(const Key('provider-current-claude')), findsOneWidget);
    expect(find.text('当前：A'), findsOneWidget);
  });

  testWidgets('unknown app falls back to the raw enum and missing current hides the subtitle',
      (tester) async {
    final client = _FakeProviderClient(
      failFirstProbe: false,
      appName: 'someNewApp',
      currentProviderId: 'p-missing',
    );
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 未知 app 枚举回退原值；currentProviderId 解析不到条目时副标题不渲染。
    expect(find.text('someNewApp'), findsOneWidget);
    expect(find.byKey(const Key('provider-current-someNewApp')), findsNothing);
    expect(find.textContaining('当前：'), findsNothing);
  });
}
