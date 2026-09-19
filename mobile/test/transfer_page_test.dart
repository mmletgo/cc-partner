import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/transfer/api.dart';
import 'package:cc_partner_mobile/transfer/client.dart';
import 'package:cc_partner_mobile/ui/transfer_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _FakeTransferApi extends TransferApi {
  _FakeTransferApi() : super(LanHttpClient(), 'http://127.0.0.1:1');

  @override
  Future<List<TransferTask>> listTasks() async => [
        TransferTask(
          id: 't-recv',
          direction: 'Receive',
          status: 'completed',
          fileName: 'notes.txt',
        ),
      ];

  @override
  Future<List<Map<String, dynamic>>> listDevices() async => [
        {'id': 'peer', 'isSelf': false, 'name': 'Other'},
        {'id': 'host', 'isSelf': true, 'name': 'This PC'},
      ];

  @override
  Future<List<int>> download(String taskId) async => [9, 8, 7];
}

void main() {
  testWidgets('transfer target list pins the host PC first and allows picking a peer', (
    tester,
  ) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '127.0.0.1:62116',
      probe: (_) async => throw Exception('skip'),
      forceIfUnreachable: true,
    );
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: TransferPage(
            book: book,
            http: LanHttpClient(),
            api: _FakeTransferApi(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('transfer-target')), findsOneWidget);
    expect(find.text('This PC · 主机'), findsOneWidget);

    await tester.tap(find.byKey(const Key('transfer-target')));
    await tester.pumpAndSettle();
    expect(find.text('Other').hitTestable(), findsWidgets);
    await tester.tap(find.text('Other').last);
    await tester.pumpAndSettle();
    expect(find.text('Other'), findsOneWidget);
  });

  testWidgets('download writes fetched bytes to the save sink', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '127.0.0.1:62116',
      probe: (_) async => throw Exception('skip'),
      forceIfUnreachable: true,
    );
    String? savedName;
    List<int>? savedBytes;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: TransferPage(
            book: book,
            http: LanHttpClient(),
            api: _FakeTransferApi(),
            saveSink: ({required fileName, required bytes}) async {
              savedName = fileName;
              savedBytes = List<int>.from(bytes);
            },
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('下载'));
    await tester.pumpAndSettle();
    expect(savedName, 'notes.txt');
    expect(savedBytes, [9, 8, 7]);
    expect(find.text('已保存到本机'), findsOneWidget);
  });
}
