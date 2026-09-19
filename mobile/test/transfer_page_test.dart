import 'dart:async';
import 'dart:typed_data';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/transfer/api.dart';
import 'package:cc_partner_mobile/transfer/client.dart';
import 'package:cc_partner_mobile/ui/transfer_page.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _FakeTransferApi extends TransferApi {
  _FakeTransferApi({List<TransferTask>? tasks})
      : _tasks = tasks,
        super(LanHttpClient(), 'http://127.0.0.1:1');

  final List<TransferTask>? _tasks;

  @override
  Future<List<TransferTask>> listTasks() async => _tasks ?? [
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

/// 假文件选择器：绕开平台通道，直接返回一份 3 MB 内存文件。
class _FakeFilePicker extends FilePicker {
  @override
  Future<FilePickerResult?> pickFiles({
    String? dialogTitle,
    String? initialDirectory,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
    Function(FilePickerStatus)? onFileLoading,
    bool allowCompression = true,
    int compressionQuality = 30,
    bool allowMultiple = false,
    bool withData = false,
    bool withReadStream = false,
    bool lockParentWindow = false,
    bool readSequential = false,
  }) async {
    return FilePickerResult([
      PlatformFile(
        name: 'clip.bin',
        size: 3 * 1024 * 1024,
        bytes: Uint8List(3 * 1024 * 1024),
      ),
    ]);
  }
}

/// 上传停在半途等测试放行，用于截获进度条中间态。
class _GatedUploadApi extends _FakeTransferApi {
  _GatedUploadApi()
      : super(
          tasks: [
            TransferTask(
              id: 't-done',
              direction: 'Receive',
              status: 'completed',
              fileName: 'old.txt',
            ),
          ],
        );

  final Completer<void> gate = Completer<void>();

  @override
  Future<Map<String, dynamic>> uploadInit({
    required String filename,
    required int size,
    required String deviceId,
    required String clientOperationId,
  }) async =>
      {'id': 'up-9'};

  @override
  Future<void> uploadFileInChunks({
    required String id,
    required Uint8List bytes,
    required TransferProgressCallback onProgress,
    TransferChunkSender? chunkSender,
    int chunkSize = transferChunkSize,
  }) async {
    onProgress(0, bytes.length);
    onProgress(bytes.length ~/ 2, bytes.length);
    await gate.future;
    onProgress(bytes.length, bytes.length);
  }

  @override
  Future<void> uploadComplete(String id) async {}
}

Future<void> _pumpPage(WidgetTester tester, TransferApi api) async {
  final book = AddressBook(store: MemoryAddressBookStore());
  await book.addFromInput(
    '127.0.0.1:62116',
    probe: (_) async => throw Exception('skip'),
    forceIfUnreachable: true,
  );
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: TransferPage(book: book, http: LanHttpClient(), api: api),
      ),
    ),
  );
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

  testWidgets('task list groups into three headers', (tester) async {
    await _pumpPage(
      tester,
      _FakeTransferApi(tasks: [
        TransferTask(id: 't-act', direction: 'Send', status: 'transferring'),
        TransferTask(id: 't-fail', direction: 'Send', status: 'failed'),
        TransferTask(id: 't-done', direction: 'Receive', status: 'completed'),
      ]),
    );
    await tester.pumpAndSettle();
    expect(find.text('进行中'), findsOneWidget);
    expect(find.text('需注意'), findsOneWidget);
    expect(find.text('已完成'), findsOneWidget);
    expect(find.byKey(const Key('transfer-task-t-act')), findsOneWidget);
    expect(find.byKey(const Key('transfer-task-t-fail')), findsOneWidget);
    expect(find.byKey(const Key('transfer-task-t-done')), findsOneWidget);
  });

  testWidgets('empty groups are hidden and tasks keep their actions', (tester) async {
    await _pumpPage(
      tester,
      _FakeTransferApi(tasks: [
        TransferTask(
          id: 't-only',
          direction: 'Receive',
          status: 'completed',
          fileName: 'only.txt',
        ),
      ]),
    );
    await tester.pumpAndSettle();
    expect(find.text('已完成'), findsOneWidget);
    expect(find.text('进行中'), findsNothing);
    expect(find.text('需注意'), findsNothing);
    expect(find.byTooltip('下载'), findsOneWidget);
  });

  testWidgets('upload shows determinate progress bar and byte text', (tester) async {
    // 测试环境里 FilePicker.platform 的 late static 未初始化，不能先读后还原，
    // 直接注入假 picker 并在 tearDown 换回一个新的假实例。
    FilePicker.platform = _FakeFilePicker();
    addTearDown(() => FilePicker.platform = _FakeFilePicker());
    final api = _GatedUploadApi();
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    await tester.tap(find.text('选择文件并立即发送'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.byKey(const Key('transfer-upload-progress')), findsOneWidget);
    expect(find.text('1.5 MB / 3.0 MB'), findsOneWidget);
    expect(find.text('选择文件并立即发送'), findsOneWidget);

    api.gate.complete();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('transfer-upload-progress')), findsNothing);
    expect(find.text('已完成'), findsOneWidget);
  });
}
