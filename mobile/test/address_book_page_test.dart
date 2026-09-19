import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/app.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/settings/risk_copy.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('shows LAN risk copy and add button', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await tester.pumpWidget(CcPartnerApp(book: book, http: LanHttpClient()));
    expect(find.byKey(const Key('risk-copy')), findsOneWidget);
    expect(find.text(kLanRiskStatement), findsOneWidget);
    expect(find.byKey(const Key('add-server')), findsOneWidget);
    expect(find.byKey(const Key('scan-qr')), findsOneWidget);
  });

  testWidgets('add dialog force-saves unreachable host as offline', (
    tester,
  ) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await tester.pumpWidget(
      CcPartnerApp(
        book: book,
        http: LanHttpClient(),
        probe: (_) async => throw Exception('timeout'),
      ),
    );
    await tester.tap(find.byKey(const Key('add-server')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('server-host')),
      '10.255.255.1',
    );
    await tester.tap(find.byKey(const Key('force-save')));
    await tester.tap(find.byKey(const Key('confirm-add')));
    await tester.pump();
    await tester.pumpAndSettle();
    expect(book.servers, isNotEmpty);
    expect(book.servers.single.isOnline, isFalse);
    expect(find.textContaining('离线'), findsWidgets);
    expect(find.textContaining('timeout'), findsWidgets);
  });

  testWidgets('re-probes saved PCs when the address book opens', (
    tester,
  ) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.9:62116',
      probe: (_) async => throw Exception('down'),
      forceIfUnreachable: true,
    );
    await tester.pumpWidget(
      CcPartnerApp(
        book: book,
        http: LanHttpClient(),
        probe: (_) async => const HealthSnapshot(
          ok: true,
          deviceId: 'pc-9',
          deviceName: 'Hans Mac',
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(book.servers.single.isOnline, isTrue);
    expect(find.textContaining('在线'), findsOneWidget);
  });

  testWidgets('re-probes when the app returns to foreground', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '10.0.0.9:62116',
      probe: (_) async => throw Exception('down'),
      forceIfUnreachable: true,
    );
    var allowOnline = false;
    await tester.pumpWidget(
      CcPartnerApp(
        book: book,
        http: LanHttpClient(),
        probe: (_) async {
          if (!allowOnline) {
            throw Exception('down');
          }
          return const HealthSnapshot(
            ok: true,
            deviceId: 'pc-9',
            deviceName: 'Hans Mac',
          );
        },
      ),
    );
    await tester.pumpAndSettle();
    expect(book.servers.single.isOnline, isFalse);

    allowOnline = true;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
    expect(book.servers.single.isOnline, isTrue);
    expect(find.textContaining('在线'), findsOneWidget);
  });
}
