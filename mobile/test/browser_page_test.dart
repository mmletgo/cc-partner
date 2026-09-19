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

/// 一键验证 fake：create 先回 running，逐次 get 推进 [states] 至末位终态；
/// 终态为 succeeded 时附带 evidence + screenshotId，artifact 返回 1x1 PNG。
class _FakeVerificationClient extends BrowserClient {
  _FakeVerificationClient({
    this.states = const ['succeeded'],
    this.failCreate = false,
  }) : super(LanHttpClient(), 'http://127.0.0.1:1');

  /// get 轮询依次进入的状态序列；不足时重复末位。
  final List<String> states;
  final bool failCreate;
  int getCalls = 0;
  int artifactCalls = 0;

  @override
  Future<BrowserVerificationRun> startVerification({
    required String previewId,
    required String requestId,
  }) async {
    if (failCreate) {
      throw LanHttpException(500, 'create down');
    }
    return const BrowserVerificationRun(
      session: BrowserVerificationSession(
        id: 'run-1',
        previewId: 'prev-1',
        state: 'running',
      ),
    );
  }

  @override
  Future<BrowserVerificationRun> getVerification({
    required String runId,
  }) async {
    getCalls += 1;
    final state = getCalls < states.length ? states[getCalls] : states.last;
    final succeeded = state == 'succeeded';
    return BrowserVerificationRun(
      session: BrowserVerificationSession(
        id: runId,
        previewId: 'prev-1',
        state: state,
      ),
      evidence: succeeded
          ? const BrowserVerificationEvidence(
              urlPath: '/?tab=home',
              assertions: [
                BrowserVerificationAssertion(name: 'title', passed: true),
                BrowserVerificationAssertion(
                  name: 'button',
                  passed: false,
                  detail: 'missing',
                ),
              ],
              consoleErrorCount: 2,
              screenshotId: 'shot-1',
            )
          : null,
    );
  }

  @override
  Future<BrowserVerificationArtifact> getVerificationArtifact({
    required String runId,
    required String artifactId,
  }) async {
    artifactCalls += 1;
    return BrowserVerificationArtifact(
      runId: runId,
      artifactId: artifactId,
      contentType: 'image/png',
      base64:
          'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==',
    );
  }
}

Future<void> _pumpCard(WidgetTester tester, BrowserClient client,
    {int maxPolls = 60}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: BrowserVerificationCard(
          previewId: 'prev-1',
          client: client,
          pollInterval: Duration.zero,
          maxPolls: maxPolls,
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
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

  testWidgets('one-click verification polls to terminal state and renders summary',
      (tester) async {
    final client = _FakeVerificationClient(states: ['running', 'succeeded']);
    await _pumpCard(tester, client);
    expect(find.byKey(const Key('browser-verify')), findsOneWidget);
    expect(find.text('验证当前预览'), findsOneWidget);

    // busy 防重：轮询进行中按钮禁用。
    await tester.tap(find.byKey(const Key('browser-verify')));
    await tester.pump();
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('browser-verify')))
          .onPressed,
      isNull,
    );
    await tester.pumpAndSettle();

    expect(client.getCalls, 1);
    expect(find.text('成功'), findsOneWidget);
    expect(find.byKey(const Key('browser-verify-summary')), findsOneWidget);
    expect(find.text('路径：/?tab=home'), findsOneWidget);
    expect(find.text('控制台错误：2'), findsOneWidget);
    expect(find.text('断言失败：1'), findsOneWidget);
    // 成功且带 screenshotId：拉取 artifact 渲染截图。
    expect(client.artifactCalls, 1);
    expect(find.byKey(const Key('browser-verify-screenshot')), findsOneWidget);
    // busy 解除后可重新验证。
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('browser-verify')))
          .onPressed,
      isNotNull,
    );
  });

  testWidgets('verification create failure shows error bar and allows retry',
      (tester) async {
    final client = _FakeVerificationClient(failCreate: true);
    await _pumpCard(tester, client);
    await tester.tap(find.byKey(const Key('browser-verify')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('browser-verify-error')), findsOneWidget);
    expect(find.textContaining('create down'), findsOneWidget);
    expect(find.byKey(const Key('browser-verify-summary')), findsNothing);
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('browser-verify')))
          .onPressed,
      isNotNull,
    );
  });

  testWidgets('verification that never reaches terminal state times out',
      (tester) async {
    final client = _FakeVerificationClient(states: ['running']);
    await _pumpCard(tester, client, maxPolls: 2);
    await tester.tap(find.byKey(const Key('browser-verify')));
    await tester.pumpAndSettle();

    expect(client.getCalls, 2);
    expect(find.text('验证超时，请重新验证'), findsOneWidget);
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('browser-verify')))
          .onPressed,
      isNotNull,
    );
  });
}
