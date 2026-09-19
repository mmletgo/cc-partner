import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/files/client.dart';
import 'package:cc_partner_mobile/files/workspace.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/files_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _RecordingFilesClient extends FilesClient {
  _RecordingFilesClient({this.openedPayload})
      : super(LanHttpClient(), 'http://127.0.0.1:1');

  final worktreeIds = <String?>[];

  /// open 响应注入；为空时返回纯文本默认值。
  Map<String, dynamic>? openedPayload;

  /// 非 null 时 saveText 抛出该错误（如 baseHash 冲突）。
  Object? saveError;
  int saveCalls = 0;

  @override
  Future<List<Map<String, dynamic>>> listDir({
    required String projectId,
    String? worktreeId,
    String? path,
  }) async {
    worktreeIds.add(worktreeId);
    return [
      {'name': 'README.md', 'kind': 'file', 'path': 'README.md'},
    ];
  }

  @override
  Future<Map<String, dynamic>> open({
    required String projectId,
    required String path,
    String? worktreeId,
  }) async =>
      openedPayload ??
      {
        'metadata': {'name': 'README.md', 'path': path},
        'text': {'content': 'hello', 'hash': 'h1'},
      };

  @override
  Future<Map<String, dynamic>> saveText({
    required String projectId,
    required String path,
    required String content,
    required String baseHash,
    String? worktreeId,
  }) async {
    saveCalls += 1;
    final error = saveError;
    if (error != null) {
      throw error;
    }
    return {
      'metadata': {'name': 'README.md', 'path': path},
      'baseHash': 'h2',
    };
  }
}

class _FilesHarness extends StatefulWidget {
  const _FilesHarness({required this.book, required this.client, this.workspace});

  final AddressBook book;
  final _RecordingFilesClient client;

  /// 注入共享的 dirty guard，供测试断言 dirty 状态。
  final FileWorkspaceController? workspace;

  @override
  State<_FilesHarness> createState() => _FilesHarnessState();
}

class _FilesHarnessState extends State<_FilesHarness> {
  String worktreeId = 'wt-a';

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        body: Column(
          children: [
            TextButton(
              key: const Key('switch-worktree'),
              onPressed: () => setState(() => worktreeId = 'wt-b'),
              child: const Text('switch'),
            ),
            Expanded(
              child: FilesPage(
                book: widget.book,
                http: LanHttpClient(),
                project: const ProjectSummary(id: 'p1', name: 'demo'),
                worktreeId: worktreeId,
                workspace: widget.workspace,
                client: widget.client,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

AddressBook _book() => AddressBook(store: MemoryAddressBookStore());

void main() {
  testWidgets('switching worktree reloads listDir with the new worktreeId', (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '127.0.0.1:62116',
      probe: (_) async => throw Exception('skip'),
      forceIfUnreachable: true,
    );
    final client = _RecordingFilesClient();
    await tester.pumpWidget(_FilesHarness(book: book, client: client));
    await tester.pumpAndSettle();
    expect(client.worktreeIds, ['wt-a']);

    await tester.tap(find.byKey(const Key('switch-worktree')));
    await tester.pumpAndSettle();
    expect(client.worktreeIds, ['wt-a', 'wt-b']);
  });

  testWidgets('file preview shows metadata line plus notice and truncated banners',
      (tester) async {
    final book = AddressBook(store: MemoryAddressBookStore());
    await book.addFromInput(
      '127.0.0.1:62116',
      probe: (_) async => throw Exception('skip'),
      forceIfUnreachable: true,
    );
    final client = _RecordingFilesClient(
      openedPayload: {
        'detectedType': 'text',
        'notice': '文件较大，仅加载部分内容',
        'truncated': true,
        'metadata': {
          'name': 'notes.txt',
          'path': 'notes.txt',
          'size': 1536,
          'modifiedAt': '2026-09-20T10:00:00',
        },
        'text': {'content': 'hello', 'hash': 'h1'},
      },
    );
    await tester.pumpWidget(_FilesHarness(book: book, client: client));
    await tester.pumpAndSettle();

    // 打开文件进入预览页。
    await tester.tap(find.text('README.md'));
    await tester.pumpAndSettle();

    // 元信息行：类型 · 大小（B/KB/MB）· 本地修改时间。
    expect(find.byKey(const Key('files-open-metadata')), findsOneWidget);
    expect(find.text('text · 1.5 KB · 2026-9-20 10:00'), findsOneWidget);
    // notice 原样上屏；truncated 用 web 同款文案。
    expect(find.byKey(const Key('files-open-notice')), findsOneWidget);
    expect(find.text('文件较大，仅加载部分内容'), findsOneWidget);
    expect(find.byKey(const Key('files-open-truncated')), findsOneWidget);
    expect(find.text('文件内容已截断显示'), findsOneWidget);
  });

  testWidgets('size formats follow web tiers and invalid fields show placeholder', (tester) async {
    expect(formatFileSizeLabel(512), '512 B');
    expect(formatFileSizeLabel(1536), '1.5 KB');
    expect(formatFileSizeLabel(3 * 1024 * 1024), '3.0 MB');
    expect(formatFileSizeLabel(null), isNull);
    expect(formatFileModifiedAt('not-a-date'), isNull);
    expect(formatFileModifiedAt(null), isNull);
    // 全缺省不渲染元信息行。
    expect(fileMetadataText({'metadata': <String, dynamic>{}}), isNull);
  });

  testWidgets('save conflict shows inline error and keeps the file dirty', (tester) async {
    final workspace = FileWorkspaceController();
    final client = _RecordingFilesClient(
      openedPayload: {
        'metadata': {'name': 'README.md', 'path': 'README.md'},
        'text': {'content': 'hello', 'hash': 'h1'},
        'capabilities': {'canEdit': true},
      },
    )..saveError = LanHttpException(409, '{"message":"baseHash 过期"}');
    await tester.pumpWidget(_FilesHarness(book: _book(), client: client, workspace: workspace));
    await tester.pumpAndSettle();

    // 打开文件 → 切到源码 → 修改内容（进入 dirty）。
    await tester.tap(find.text('README.md'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('源码'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'changed');
    await tester.pump();
    expect(workspace.snapshot.dirty, isTrue);

    // 保存失败：inline 错误条上屏（冲突语义文案），文件保持 dirty。
    await tester.tap(find.byKey(const Key('files-save')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('files-save-error')), findsOneWidget);
    expect(find.text('保存文件失败：文件已在磁盘上变化，请刷新后再试'), findsOneWidget);
    expect(workspace.snapshot.dirty, isTrue);

    // 修复后重新保存成功：错误条消失 + 已保存提示 + dirty 清除。
    client.saveError = null;
    await tester.tap(find.byKey(const Key('files-save')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('files-save-error')), findsNothing);
    expect(find.text('已保存'), findsOneWidget);
    expect(workspace.snapshot.dirty, isFalse);
  });

  testWidgets('non-conflict save failure surfaces the raw error and keeps dirty',
      (tester) async {
    final workspace = FileWorkspaceController();
    final client = _RecordingFilesClient()..saveError = LanHttpException(500, 'disk gone');
    await tester.pumpWidget(_FilesHarness(book: _book(), client: client, workspace: workspace));
    await tester.pumpAndSettle();

    await tester.tap(find.text('README.md'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('源码'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'changed');
    await tester.pump();
    await tester.tap(find.byKey(const Key('files-save')));
    await tester.pumpAndSettle();

    expect(find.text('保存文件失败：LAN HTTP 500: disk gone'), findsOneWidget);
    expect(workspace.snapshot.dirty, isTrue);
  });

  testWidgets('canEdit=false hides the editor and save, showing a readonly note',
      (tester) async {
    final client = _RecordingFilesClient(
      openedPayload: {
        'metadata': {'name': 'README.md', 'path': 'README.md'},
        'text': {'content': 'hello', 'hash': 'h1'},
        'capabilities': {'canEdit': false},
      },
    );
    await tester.pumpWidget(_FilesHarness(book: _book(), client: client));
    await tester.pumpAndSettle();

    await tester.tap(find.text('README.md'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('files-readonly-note')), findsOneWidget);
    expect(find.text('该文件不支持在手机上编辑，仅可查看。'), findsOneWidget);
    expect(find.byKey(const Key('files-save')), findsNothing);
    expect(find.byType(SegmentedButton<String>), findsNothing);
  });
}
