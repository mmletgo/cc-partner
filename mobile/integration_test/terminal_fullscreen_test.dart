import 'dart:io';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/address_book/models.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/sessions/client.dart';
import 'package:cc_partner_mobile/ui/terminal_page.dart';
import 'package:cc_partner_mobile/ui/workbench_shell.dart';
import 'package:cc_partner_mobile/workbench/nav.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// 模拟器上核对终端全屏：按钮在屏内、全屏层盖住整屏、终端画面占屏幕大部分，
/// 并把更大的行数交给远端 PTY。不连真机后端。
void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('终端全屏盖住标题栏并放大画面', (tester) async {
    final sessions = _FakeSessions([
      const SessionSummary(
        id: 's0',
        projectId: 'p1',
        name: 'shell',
        status: 'running',
        worktreeId: 'w1',
        cols: 80,
        rows: 24,
      ),
    ]);
    final book = AddressBook(store: MemoryAddressBookStore());
    book.servers.add(
      ServerRecord(
        id: 'pc',
        host: '127.0.0.1',
        port: 1,
        baseUrl: 'http://127.0.0.1:1',
      ),
    );
    book.activeServerId = 'pc';

    await tester.pumpWidget(
      MaterialApp(
        home: _FullscreenShellHost(book: book, sessions: sessions),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    final screen = tester.getSize(find.byType(Scaffold));
    final fullscreenButton = find.byTooltip('全屏');
    expect(fullscreenButton, findsOneWidget);
    final buttonTopLeft = tester.getTopLeft(fullscreenButton);
    expect(buttonTopLeft.dx, greaterThanOrEqualTo(0));
    expect(buttonTopLeft.dx + 24, lessThan(screen.width));

    final before = tester.getSize(find.byKey(const Key('terminal-view')));
    final resizeBefore = sessions.resizeCalls.length;
    await binding.takeScreenshot('terminal-before-fullscreen');

    await tester.tap(fullscreenButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const Key('terminal-fullscreen-overlay')), findsOneWidget);
    expect(find.byType(AppBar), findsNothing);
    final overlay = tester.getSize(find.byKey(const Key('terminal-fullscreen-overlay')));
    final after = tester.getSize(find.byKey(const Key('terminal-view')));
    expect(overlay.width, screen.width);
    expect(overlay.height, screen.height);
    expect(tester.getTopLeft(find.byKey(const Key('terminal-fullscreen-overlay'))), Offset.zero);
    expect(after.height, greaterThan(before.height));
    expect(
      after.height,
      greaterThan(screen.height * 0.62),
      reason: '全屏后终端画面应盖住标题栏并占屏幕大部分，'
          '画面 ${after.height.toStringAsFixed(0)} / 屏幕 ${screen.height.toStringAsFixed(0)}，'
          '全屏前 ${before.height.toStringAsFixed(0)}',
    );
    expect(sessions.resizeCalls.length, greaterThan(resizeBefore));
    expect(
      sessions.resizeCalls.last.$3,
      greaterThan(20),
      reason: '全屏后行数应大于默认小窗，实际 ${sessions.resizeCalls.last}',
    );

    final shot = await binding.takeScreenshot('terminal-after-fullscreen');
    File('/tmp/terminal-after-fullscreen.png').writeAsBytesSync(shot);
    // ignore: avoid_print
    print(
      'FULLSCREEN_OK screen=${screen.width.toStringAsFixed(0)}x${screen.height.toStringAsFixed(0)} '
      'before=${before.height.toStringAsFixed(0)} after=${after.height.toStringAsFixed(0)} '
      'rows=${sessions.resizeCalls.last.$3}',
    );
  });
}

class _FullscreenShellHost extends StatefulWidget {
  const _FullscreenShellHost({required this.book, required this.sessions});

  final AddressBook book;
  final SessionsClient sessions;

  @override
  State<_FullscreenShellHost> createState() => _FullscreenShellHostState();
}

class _FullscreenShellHostState extends State<_FullscreenShellHost> {
  bool _fullscreen = false;

  @override
  Widget build(BuildContext context) {
    return WorkbenchShell(
      mode: WorkbenchNavMode.project,
      panel: WorkbenchPanel.terminal,
      projectLabel: 'demo',
      onSelect: (_) {},
      hideAppBar: _fullscreen,
      hideWorktreeStrip: _fullscreen,
      worktreeStrip: const Text('wt-strip'),
      child: TerminalPage(
        key: const ValueKey('terminal'),
        book: widget.book,
        http: LanHttpClient(),
        project: const ProjectSummary(id: 'p1', name: 'demo', path: '/tmp/demo'),
        worktreeId: 'w1',
        sessionsClient: widget.sessions,
        backgroundTimersDisabled: true,
        onFullscreenChanged: (fullscreen) {
          if (!mounted) {
            return;
          }
          setState(() => _fullscreen = fullscreen);
        },
      ),
    );
  }
}

class _FakeSessions extends SessionsClient {
  _FakeSessions(this.sessions) : super(LanHttpClient(), 'http://127.0.0.1:1');

  List<SessionSummary> sessions;
  final List<(String, int, int)> resizeCalls = [];

  @override
  Future<List<SessionSummary>> list(String projectId) async => sessions;

  @override
  Future<Map<String, dynamic>> replay(
    String sessionId, {
    bool refreshHistory = false,
  }) async {
    return {'sessionId': sessionId, 'buffer': 'hello\n', 'lastSeq': 1};
  }

  @override
  Future<void> focus(String sessionId, {bool streamActive = true}) async {}

  @override
  Future<void> zoomPane(String sessionId) async {}

  @override
  Future<void> resize(String sessionId, int cols, int rows) async {
    resizeCalls.add((sessionId, cols, rows));
  }
}
