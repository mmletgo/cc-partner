import 'dart:async';
import 'dart:typed_data';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/transfer/api.dart';
import 'package:cc_partner_mobile/transfer/client.dart';
import 'package:cc_partner_mobile/transfer/polling.dart';
import 'package:cc_partner_mobile/ui/transfer_page.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _FakeTransferApi extends TransferApi {
  _FakeTransferApi({List<TransferTask>? tasks})
      : _tasks = tasks,
        super(LanHttpClient(), 'http://127.0.0.1:1');

  final List<TransferTask>? _tasks;
  final resumeCalls = <String>[];
  final retryCalls = <String>[];
  final cancelCalls = <String>[];
  int listTasksCalls = 0;
  bool failResumeWithNetworkError = false;
  bool failCompleteWithNetworkError = false;
  bool failRetryWithLanError = false;
  bool failCancelWithLanError = false;
  int getOperationCalls = 0;

  /// 第 N 次 get-operation 起返回 succeeded；调大可模拟一直 pending。
  int operationSucceedsAfterCalls = 2;

  @override
  Future<List<TransferTask>> listTasks() async {
    listTasksCalls += 1;
    return _tasks ?? [
      TransferTask(
        id: 't-recv',
        direction: 'Receive',
        status: 'completed',
        fileName: 'notes.txt',
      ),
    ];
  }

  @override
  Future<List<Map<String, dynamic>>> listDevices() async => [
        {'id': 'peer', 'isSelf': false, 'name': 'Other'},
        {'id': 'host', 'isSelf': true, 'name': 'This PC'},
      ];

  @override
  Future<List<int>> download(String taskId) async => [9, 8, 7];

  @override
  Future<void> cancel(String taskId) async {
    cancelCalls.add(taskId);
    if (failCancelWithLanError) {
      throw LanHttpException(500, 'boom');
    }
  }

  @override
  Future<void> resume(String taskId, String clientOperationId) async {
    resumeCalls.add(taskId);
    if (failResumeWithNetworkError) {
      throw LanHttpException(0, 'network unreachable');
    }
  }

  @override
  Future<void> retry(String taskId, String clientOperationId) async {
    retryCalls.add(taskId);
    if (failRetryWithLanError) {
      throw LanHttpException(500, 'boom');
    }
  }

  @override
  Future<Map<String, dynamic>> uploadInit({
    required String filename,
    required int size,
    required String deviceId,
    required String clientOperationId,
  }) async =>
      {'id': 'up-9', 'receivedBytes': 0};

  @override
  Future<void> uploadFileInChunks({
    required String id,
    required Uint8List bytes,
    required TransferProgressCallback onProgress,
    TransferChunkSender? chunkSender,
    int chunkSize = transferChunkSize,
  }) async {
    onProgress(0, bytes.length);
    onProgress(bytes.length, bytes.length);
  }

  @override
  Future<void> uploadComplete(String id) async {
    if (failCompleteWithNetworkError) {
      throw LanHttpException(0, 'network unreachable');
    }
  }

  @override
  Future<TransferOperationStatus> getOperation(String clientOperationId) async {
    getOperationCalls += 1;
    return TransferOperationStatus(
      status: getOperationCalls < operationSucceedsAfterCalls ? 'pending' : 'succeeded',
      taskId: 't-9',
    );
  }
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

  TransferTask failedTask({int transferredBytes = 2048, String logicalId = 'lt-1'}) =>
      TransferTask(
        id: 't-fail',
        direction: 'Send',
        status: 'failed',
        fileName: 'clip.bin',
        peer: 'peer',
        transferredBytes: transferredBytes,
        logicalTransferId: logicalId,
      );

  testWidgets('failed send with resume-capable peer offers resume and reloads', (tester) async {
    final api = _FakeRecoveryDevicesApi(
      tasks: [failedTask()],
      peerCapabilities: ['transfer.resume.v1'],
    );
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    // peer 带能力 + 有已确认字节 → 只显示「继续传输」。
    expect(find.byTooltip('继续传输'), findsOneWidget);
    expect(find.byTooltip('重新传输'), findsNothing);

    await tester.tap(find.byTooltip('继续传输'));
    await tester.pumpAndSettle();
    expect(api.resumeCalls, ['t-fail']);
    expect(find.text('已继续传输'), findsOneWidget);
    // 成功后重载任务列表。
    expect(api.listTasksCalls, greaterThanOrEqualTo(2));
  });

  testWidgets('failed send without resume capability falls back to retry', (tester) async {
    final api = _FakeRecoveryDevicesApi(tasks: [failedTask()], peerCapabilities: null);
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    expect(find.byTooltip('重新传输'), findsOneWidget);
    expect(find.byTooltip('继续传输'), findsNothing);

    await tester.tap(find.byTooltip('重新传输'));
    await tester.pumpAndSettle();
    expect(api.retryCalls, ['t-fail']);
    expect(find.text('已重新传输'), findsOneWidget);
  });

  testWidgets('retry failure shows an inline row error with alert semantics', (tester) async {
    final api = _FakeRecoveryDevicesApi(tasks: [failedTask()], peerCapabilities: null);
    api.failRetryWithLanError = true;
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('重新传输'));
    await tester.pumpAndSettle();
    // 错误显示在行内（liveRegion），而不是只有 SnackBar。
    expect(find.byKey(const Key('transfer-error-t-fail')), findsOneWidget);
    expect(find.textContaining('重新传输失败'), findsOneWidget);
    final errorSemantics = tester
        .getSemantics(find.byKey(const Key('transfer-error-t-fail')))
        .getSemanticsData();
    expect(errorSemantics.flagsCollection.isLiveRegion, isTrue);
  });

  testWidgets('uncertain upload reconciles to success via get-operation', (tester) async {
    FilePicker.platform = _FakeFilePicker();
    addTearDown(() => FilePicker.platform = _FakeFilePicker());
    final api = _FakeTransferApi();
    api.failCompleteWithNetworkError = true;
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    await tester.tap(find.text('选择文件并立即发送'));
    await tester.idle();
    // complete 抛 network 类错误 → 对账：第 1 次 pending，等待 1.5s 后第 2 次 succeeded。
    await tester.pump(const Duration(milliseconds: 1600));
    await tester.idle();
    await tester.pump();

    expect(api.getOperationCalls, 2);
    // 确认成功：不报发送失败，并重载任务列表。
    expect(find.byKey(const Key('transfer-send-error')), findsNothing);
    expect(api.listTasksCalls, greaterThanOrEqualTo(2));
  });

  testWidgets('uncertain upload gives up after bounded pending reconciliation', (tester) async {
    FilePicker.platform = _FakeFilePicker();
    addTearDown(() => FilePicker.platform = _FakeFilePicker());
    final api = _FakeTransferApi();
    api.failCompleteWithNetworkError = true;
    api.operationSucceedsAfterCalls = 999; // 一直 pending。
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    await tester.tap(find.text('选择文件并立即发送'));
    await tester.idle();
    // 默认 12 次 × 1.5s；用大步进 fake 时钟推进到对账耗尽。
    for (var i = 0; i < 14; i++) {
      await tester.pump(const Duration(milliseconds: 1600));
    }
    await tester.idle();
    await tester.pump();

    expect(api.getOperationCalls, 12);
    expect(find.byKey(const Key('transfer-send-error')), findsOneWidget);
    expect(find.text('发送失败：操作仍在处理中，请稍后重试'), findsOneWidget);
  });

  testWidgets('cancel keeps double-click guard and shows inline error on failure', (tester) async {
    final api = _FakeTransferApi(tasks: [
      TransferTask(id: 't-act', direction: 'Send', status: 'transferring'),
    ]);
    api.failCancelWithLanError = true;
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('取消'));
    await tester.pumpAndSettle();
    expect(api.cancelCalls, ['t-act']);
    expect(find.byKey(const Key('transfer-error-t-act')), findsOneWidget);
    expect(find.textContaining('取消失败'), findsOneWidget);
  });

  testWidgets('visibility poller drives silent task/device refresh', (tester) async {
    final api = _FakeTransferApi();
    await _pumpPage(tester, api);
    await tester.pumpAndSettle();
    final initialTasks = api.listTasksCalls;

    // 3s 任务轮询 tick。
    await tester.pump(const Duration(seconds: 3));
    await tester.pump();
    expect(api.listTasksCalls, greaterThan(initialTasks));
  });

  pollerTests();
}

/// 可自定义 peer 能力的假 API：用于续传/重传回退判定。
class _FakeRecoveryDevicesApi extends _FakeTransferApi {
  _FakeRecoveryDevicesApi({required List<TransferTask> tasks, List<String>? peerCapabilities})
      : _peerCapabilities = peerCapabilities,
        super(tasks: tasks);

  final List<String>? _peerCapabilities;

  @override
  Future<List<Map<String, dynamic>>> listDevices() async => [
        {
          'id': 'peer',
          'isSelf': false,
          'name': 'Other',
          if (_peerCapabilities != null) 'capabilities': _peerCapabilities,
        },
        {'id': 'host', 'isSelf': true, 'name': 'This PC'},
      ];
}

/// 假 Timer：记录取消状态与 tick，供手动驱动轮询节奏。
class _FakeTimer implements Timer {
  _FakeTimer(this._callback);

  final void Function() _callback;
  bool cancelled = false;
  int fired = 0;

  void fire() {
    if (cancelled) {
      return;
    }
    fired += 1;
    _callback();
  }

  @override
  void cancel() => cancelled = true;

  @override
  bool get isActive => !cancelled;

  @override
  int get tick => fired;
}

void pollerTests() {
  testWidgets('poller ticks only while resumed and refreshes immediately on resume', (tester) async {
    final timers = <_FakeTimer>[];
    var runs = 0;
    final poller = VisibilityPoller(
      interval: const Duration(seconds: 3),
      task: () async => runs++,
      timerFactory: (interval, onTick) {
        final timer = _FakeTimer(onTick);
        timers.add(timer);
        return timer;
      },
    );
    poller.start(runImmediately: false);
    expect(runs, 0);
    expect(timers, hasLength(1));

    // resumed：tick 驱动任务（idle 冲掉 runNow 的 microtask，释放 single-flight 门闩）。
    timers.single.fire();
    await tester.idle();
    expect(runs, 1);

    // 后台（paused）：停表，tick 失效。
    poller.handleLifecycle(AppLifecycleState.paused);
    expect(timers.single.cancelled, isTrue);
    timers.single.fire();
    expect(runs, 1);

    // 回前台：立即补拉一次并重启周期表。
    poller.handleLifecycle(AppLifecycleState.resumed);
    await tester.idle();
    expect(runs, 2);
    expect(timers, hasLength(2));
    timers.last.fire();
    await tester.idle();
    expect(runs, 3);

    poller.dispose();
    // dispose 后全部 timer 已取消，不留后台任务。
    for (final timer in timers) {
      expect(timer.cancelled, isTrue);
    }
  });

  testWidgets('poller runNow is single-flight', (tester) async {
    final gate = Completer<void>();
    var runs = 0;
    final poller = VisibilityPoller(
      interval: const Duration(seconds: 3),
      task: () async {
        runs += 1;
        await gate.future;
      },
      timerFactory: (_, __) => _FakeTimer(() {}),
    );
    final first = poller.runNow();
    final second = poller.runNow();
    expect(runs, 1);
    gate.complete();
    await first;
    await second;
    expect(runs, 1);
    poller.dispose();
  });
}
