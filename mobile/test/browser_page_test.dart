import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/browser/client.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/browser_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 只覆盖 discover/chips 路径；WebViewWidget 在 widget test 无平台实现，不打开预览。
class _FakeBrowserClient extends BrowserClient {
  _FakeBrowserClient({this.failDiscover = false})
      : super(LanHttpClient(), 'http://127.0.0.1:1');

  final bool failDiscover;

  @override
  Future<BrowserDiscovery> discover({
    required String projectId,
    String? worktreeId,
  }) async {
    if (failDiscover) {
      throw LanHttpException(500, 'discover down');
    }
    return const BrowserDiscovery(
      targets: [
        BrowserTarget(
          id: 't-1',
          url: 'http://127.0.0.1:5173',
          displayUrl: '127.0.0.1:5173',
          reachable: true,
        ),
        BrowserTarget(
          id: 't-2',
          url: 'http://127.0.0.1:3000',
          displayUrl: '127.0.0.1:3000',
        ),
      ],
      selectedTargetId: 't-1',
    );
  }
}

Future<void> _pumpPage(WidgetTester tester, BrowserClient client) async {
  final book = AddressBook(store: MemoryAddressBookStore());
  await book.addFromInput(
    '127.0.0.1:62116',
    probe: (_) async => throw Exception('skip'),
    forceIfUnreachable: true,
  );
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: BrowserPage(
          book: book,
          http: LanHttpClient(),
          project: const ProjectSummary(id: 'p1', name: 'demo'),
          client: client,
        ),
      ),
    ),
  );
}

void main() {
  testWidgets('discover candidates render as chips and tapping fills the URL field', (
    tester,
  ) async {
    await _pumpPage(tester, _FakeBrowserClient());
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('browser-target-chip-t-1')), findsOneWidget);
    expect(find.byKey(const Key('browser-target-chip-t-2')), findsOneWidget);

    await tester.tap(find.byKey(const Key('browser-target-chip-t-2')));
    await tester.pump();
    final field = tester.widget<TextField>(find.byType(TextField));
    expect(field.controller!.text, 'http://127.0.0.1:3000');
  });

  testWidgets('discover failure degrades silently to manual input', (tester) async {
    await _pumpPage(tester, _FakeBrowserClient(failDiscover: true));
    await tester.pumpAndSettle();
    expect(find.byType(ChoiceChip), findsNothing);
    expect(find.text('打开预览'), findsOneWidget);
    expect(find.text('输入本机 dev server 地址后打开 live preview。'), findsOneWidget);
  });
}
