import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/settings/risk_copy.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('address book shows risk copy and force-saves an offline PC', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await tester.pumpWidget(CcPartnerApp(book: book, http: LanHttpClient()));
    await tester.pumpAndSettle();
    expect(find.text(kLanRiskStatement), findsOneWidget);

    await tester.tap(find.byKey(const Key('add-server')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('server-host')), '10.255.255.1');
    await tester.tap(find.byKey(const Key('force-save')));
    await tester.tap(find.byKey(const Key('confirm-add')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 6));
    await tester.pumpAndSettle();

    expect(book.servers, isNotEmpty);
    expect(book.servers.single.isOnline, isFalse);
    expect(find.textContaining('10.255.255.1'), findsWidgets);
    expect(find.textContaining('离线'), findsWidgets);
  });
}
