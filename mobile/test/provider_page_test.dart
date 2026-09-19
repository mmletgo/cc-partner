import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/provider/client.dart';
import 'package:cc_partner_mobile/ui/provider_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 首次 probe 可配置失败，用于验证错误态与重试；summary 返回固定 app 列表。
class _FakeProviderClient extends ProviderClient {
  _FakeProviderClient({this.failFirstProbe = true})
      : super(LanHttpClient(), 'http://127.0.0.1:1');

  final bool failFirstProbe;
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
}
