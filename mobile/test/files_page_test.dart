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
}

class _FilesHarness extends StatefulWidget {
  const _FilesHarness({required this.book, required this.client});

  final AddressBook book;
  final _RecordingFilesClient client;

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
                client: widget.client,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

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
}
