import 'dart:async';

import 'package:cc_partner_mobile/address_book/book.dart';
import 'package:cc_partner_mobile/browser/client.dart';
import 'package:cc_partner_mobile/core/lan_http.dart';
import 'package:cc_partner_mobile/projects/client.dart';
import 'package:cc_partner_mobile/ui/browser_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
// webview_flutter 把平台接口作为 transitive 依赖带入；widget test 替身必须实现其抽象类。
// ignore: depend_on_referenced_packages
import 'package:webview_flutter_platform_interface/webview_flutter_platform_interface.dart';

/// WebView 平台替身：widget test 无 native WebView 实现（pigeon 通道无人应答会抛
/// channel-error），自动打开预览路径改挂内存替身，不触达平台通道。
class _FakeWebViewPlatform extends WebViewPlatform {
  @override
  PlatformWebViewController createPlatformWebViewController(
    PlatformWebViewControllerCreationParams params,
  ) => _FakePlatformWebViewController(params);

  @override
  PlatformNavigationDelegate createPlatformNavigationDelegate(
    PlatformNavigationDelegateCreationParams params,
  ) => _FakePlatformNavigationDelegate(params);

  @override
  PlatformWebViewWidget createPlatformWebViewWidget(
    PlatformWebViewWidgetCreationParams params,
  ) => _FakePlatformWebViewWidget(params);
}

/// 只实现 BrowserPage 用到的最小面：JS 模式、导航代理、加载 URL。
class _FakePlatformWebViewController extends PlatformWebViewController {
  _FakePlatformWebViewController(super.params) : super.implementation();

  @override
  Future<void> setJavaScriptMode(JavaScriptMode javaScriptMode) async {}

  @override
  Future<void> setPlatformNavigationDelegate(
    PlatformNavigationDelegate handler,
  ) async {}

  @override
  Future<void> loadRequest(LoadRequestParams params) async {}
}

/// 吞掉页面注册的导航回调（进度/完成/资源错误）。
class _FakePlatformNavigationDelegate extends PlatformNavigationDelegate {
  _FakePlatformNavigationDelegate(super.params) : super.implementation();

  @override
  Future<void> setOnNavigationRequest(
    NavigationRequestCallback onNavigationRequest,
  ) async {}

  @override
  Future<void> setOnPageStarted(PageEventCallback onPageStarted) async {}

  @override
  Future<void> setOnPageFinished(PageEventCallback onPageFinished) async {}

  @override
  Future<void> setOnProgress(ProgressCallback onProgress) async {}

  @override
  Future<void> setOnWebResourceError(
    WebResourceErrorCallback onWebResourceError,
  ) async {}
}

/// 渲染占位视图，避免真实 PlatformView 创建。
class _FakePlatformWebViewWidget extends PlatformWebViewWidget {
  _FakePlatformWebViewWidget(super.params) : super.implementation();

  @override
  Widget build(BuildContext context) => const SizedBox.expand();
}

/// discover/preview 路径的假客户端；WebViewWidget 在 widget test 无平台实现，
/// 需要渲染预览的用例通过真平台渠道 mock（见 _pumpPageWithWebView）打开。
class _FakeBrowserClient extends BrowserClient {
  _FakeBrowserClient({BrowserDiscovery? discovery})
      : _discovery = discovery,
        super(LanHttpClient(), 'http://127.0.0.1:1');

  /// 可变：测试中途修复失败验证「重新探测」恢复。
  bool failDiscover = false;
  bool failCreatePreview = false;

  /// 缺省 discovery：selected 候选无 source（旧后端宽容口径）→ 不自动打开。
  final BrowserDiscovery? _discovery;

  int discoverCalls = 0;
  int createPreviewCalls = 0;
  String? createdTargetUrl;

  /// 非空时 discover 停在门闩上，供 busy 态断言。
  Completer<void>? discoverGate;

  BrowserDiscovery get effectiveDiscovery =>
      _discovery ??
      const BrowserDiscovery(
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
            source: 'portProbe',
          ),
        ],
        selectedTargetId: 't-1',
      );

  @override
  Future<BrowserDiscovery> discover({
    required String projectId,
    String? worktreeId,
  }) async {
    discoverCalls += 1;
    final gate = discoverGate;
    if (gate != null) {
      await gate.future;
    }
    if (failDiscover) {
      throw LanHttpException(500, 'discover down');
    }
    return effectiveDiscovery;
  }

  @override
  Future<BrowserPreview> createPreview({
    required String projectId,
    String? worktreeId,
    required String targetUrl,
  }) async {
    createPreviewCalls += 1;
    createdTargetUrl = targetUrl;
    if (failCreatePreview) {
      throw LanHttpException(500, 'preview down');
    }
    return BrowserPreview(
      previewId: 'prev-$createPreviewCalls',
      mobileProxyPath: BrowserPreviewPolicy.proxyPath('prev-$createPreviewCalls'),
      targetUrl: targetUrl,
    );
  }
}

/// 选中候选带 remembered 来源的假客户端：discover 成功后应自动打开默认目标。
class _AutoOpenBrowserClient extends _FakeBrowserClient {
  _AutoOpenBrowserClient()
      : super(
          discovery: const BrowserDiscovery(
            targets: [
              BrowserTarget(
                id: 't-1',
                url: 'http://127.0.0.1:5173',
                displayUrl: '127.0.0.1:5173',
                source: 'remembered',
                reachable: true,
              ),
              BrowserTarget(
                id: 't-2',
                url: 'http://127.0.0.1:3000',
                displayUrl: '127.0.0.1:3000',
                source: 'portProbe',
              ),
            ],
            selectedTargetId: 't-1',
          ),
        );
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
  // 自动打开预览会构造 WebViewController 并渲染 WebViewWidget：
  // 全部用例统一挂 WebView 平台替身（平台接口为全局静态，本文件内不还原，
  // 后续用例 setUp 会重新设置同一替身；仅替身可避免 pigeon 通道无应答抛错）。
  setUp(() {
    WebViewPlatform.instance = _FakeWebViewPlatform();
  });

  testWidgets('discover candidates render as chips with source labels and tapping fills the URL field',
      (tester) async {
    await _pumpPage(tester, _FakeBrowserClient());
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('browser-target-chip-t-1')), findsOneWidget);
    expect(find.byKey(const Key('browser-target-chip-t-2')), findsOneWidget);
    // 候选 chip 补来源文案（t-2 为 portProbe → 端口探测）。
    expect(find.byKey(const Key('browser-target-source-t-2')), findsOneWidget);
    expect(find.text('端口探测'), findsOneWidget);
    // 无 source 的旧后端候选不渲染来源行。
    expect(find.byKey(const Key('browser-target-source-t-1')), findsNothing);

    await tester.tap(find.byKey(const Key('browser-target-chip-t-2')));
    await tester.pump();
    final field = tester.widget<TextField>(find.byType(TextField));
    expect(field.controller!.text, 'http://127.0.0.1:3000');
  });

  testWidgets('discover failure shows the error area and re-discover recovers', (
    tester,
  ) async {
    final client = _FakeBrowserClient()..failDiscover = true;
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();
    // 探测失败不再静默：错误区上屏（key 断言），候选为空仍可手动输入。
    expect(find.byType(ChoiceChip), findsNothing);
    expect(find.byKey(const Key('browser-error')), findsOneWidget);
    expect(find.textContaining('候选探测失败'), findsOneWidget);
    expect(find.text('打开预览'), findsOneWidget);
    expect(find.byKey(const Key('browser-rediscover')), findsOneWidget);

    // fake 修复后点「重新探测」→ 候选渲染、错误清除。
    client.failDiscover = false;
    await tester.tap(find.byKey(const Key('browser-rediscover')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('browser-target-chip-t-1')), findsOneWidget);
    expect(find.byKey(const Key('browser-error')), findsNothing);
    expect(client.discoverCalls, 2);
  });

  testWidgets('discover success auto-opens the remembered default candidate', (tester) async {
    final client = _AutoOpenBrowserClient();
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // discover 成功且选中候选来源为 remembered → 自动创建预览打开。
    expect(client.createPreviewCalls, 1);
    expect(client.createdTargetUrl, 'http://127.0.0.1:5173');
    expect(find.byKey(const Key('browser-live-preview')), findsOneWidget);
    expect(find.text('live preview · JS on · prev-1'), findsOneWidget);
    // chip 高亮按 preview.targetUrl 匹配：目标候选亮、其余不亮。
    final selected = tester.widget<ChoiceChip>(
      find.byKey(const Key('browser-target-chip-t-1')),
    );
    expect(selected.selected, isTrue);
    final unselected = tester.widget<ChoiceChip>(
      find.byKey(const Key('browser-target-chip-t-2')),
    );
    expect(unselected.selected, isFalse);
  });

  testWidgets('auto-open failure surfaces the error but keeps discoverable chips', (
    tester,
  ) async {
    final client = _AutoOpenBrowserClient()..failCreatePreview = true;
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 自动建预览失败：错误区上屏、无预览，但候选 chips 仍可手动打开。
    expect(client.createPreviewCalls, 1);
    expect(find.byKey(const Key('browser-error')), findsOneWidget);
    expect(find.byKey(const Key('browser-live-preview')), findsNothing);
    expect(find.byKey(const Key('browser-target-chip-t-1')), findsOneWidget);
  });

  testWidgets('discover does not auto-open portProbe-only or manual candidates', (tester) async {
    final client = _FakeBrowserClient();
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();

    // 默认 discovery 的选中候选缺 source（旧后端宽容口径）→ 不自动建预览。
    expect(client.createPreviewCalls, 0);
    expect(find.byKey(const Key('browser-live-preview')), findsNothing);
    // 手动打开仍然可用。
    await tester.tap(find.byKey(const Key('browser-open')));
    await tester.pump();
    expect(client.createPreviewCalls, 1);
    expect(find.byKey(const Key('browser-live-preview')), findsOneWidget);
  });

  testWidgets('manual re-discover keeps the opened preview', (tester) async {
    final client = _AutoOpenBrowserClient();
    await _pumpPage(tester, client);
    await tester.pumpAndSettle();
    expect(client.createPreviewCalls, 1);

    // 重新探测只刷新候选，不销毁已打开预览（不重复建 preview 会话）。
    await tester.tap(find.byKey(const Key('browser-rediscover')));
    await tester.pumpAndSettle();
    expect(client.createPreviewCalls, 1);
    expect(find.byKey(const Key('browser-live-preview')), findsOneWidget);
    expect(client.discoverCalls, 2);
  });

  testWidgets('open button is disabled while busy or with a blank URL', (tester) async {
    final client = _FakeBrowserClient()..discoverGate = Completer<void>();
    await _pumpPage(tester, client);
    await tester.pump();

    // 探测在途（busy）：「打开预览」与「重新探测」都禁用。
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('browser-open'))).onPressed,
      isNull,
    );
    expect(
      tester.widget<IconButton>(find.byKey(const Key('browser-rediscover'))).onPressed,
      isNull,
    );

    client.discoverGate!.complete();
    await tester.pumpAndSettle();
    // busy 解除后恢复可用（默认 URL 非空）。
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('browser-open'))).onPressed,
      isNotNull,
    );

    // 空白 URL → 打开按钮禁用；输入有效地址恢复。
    await tester.enterText(find.byType(TextField), '   ');
    await tester.pump();
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('browser-open'))).onPressed,
      isNull,
    );
    await tester.enterText(find.byType(TextField), 'http://127.0.0.1:8080');
    await tester.pump();
    expect(
      tester.widget<FilledButton>(find.byKey(const Key('browser-open'))).onPressed,
      isNotNull,
    );
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
