import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/files/client.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/files_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class _RecordingFilesClient extends FilesClient {
  _RecordingFilesClient() : super(LanHttpClient(), 'http://127.0.0.1:1');

  final worktreeIds = <String?>[];

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
}
