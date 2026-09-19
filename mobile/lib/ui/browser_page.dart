import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../address_book/book.dart';
import '../browser/client.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';

class BrowserPage extends StatefulWidget {
  const BrowserPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.worktreeId,
    this.client,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;

  /// 测试注入点；为空时按当前设备 baseUrl 构造。
  final BrowserClient? client;

  @override
  State<BrowserPage> createState() => _BrowserPageState();
}

class _BrowserPageState extends State<BrowserPage> {
  late final BrowserClient _client;
  final _url = TextEditingController(text: 'http://127.0.0.1:5173');
  WebViewController? _web;
  String? _error;
  String? _previewId;
  List<BrowserTarget> _targets = [];
  String? _selectedTargetId;
  int? _loadProgress;
  String? _loadError;

  @override
  void initState() {
    super.initState();
    _client = widget.client ?? BrowserClient(widget.http, widget.book.active!.baseUrl);
    _discover();
  }

  @override
  void dispose() {
    _url.dispose();
    super.dispose();
  }

  /// 进入页面自动探测 dev server 候选；失败静默降级（仅手动输入）。
  Future<void> _discover() async {
    try {
      final discovery = await _client.discover(
        projectId: widget.project.id,
        worktreeId: widget.worktreeId,
      );
      if (!mounted) {
        return;
      }
      setState(() {
        _targets = discovery.targets;
        _selectedTargetId = discovery.selectedTargetId;
        final selected = discovery.selectedTarget;
        if (selected != null && selected.url.isNotEmpty) {
          _url.text = selected.url;
        }
      });
    } catch (_) {
      // 静默降级：无候选时仍可手动输入地址打开预览。
    }
  }

  Future<void> _open() async {
    setState(() {
      _error = null;
      _loadError = null;
      _loadProgress = null;
    });
    try {
      final preview = await _client.createPreview(
        projectId: widget.project.id,
        worktreeId: widget.worktreeId,
        targetUrl: _url.text.trim(),
      );
      final uri = Uri.parse('${widget.book.active!.baseUrl}${preview.mobileProxyPath}');
      final web = WebViewController()
        ..setJavaScriptMode(JavaScriptMode.unrestricted)
        ..setNavigationDelegate(
          NavigationDelegate(
            onProgress: (progress) {
              if (mounted) {
                setState(() => _loadProgress = progress >= 100 ? null : progress);
              }
            },
            onPageFinished: (_) {
              if (mounted) {
                setState(() => _loadProgress = null);
              }
            },
            onWebResourceError: (resourceError) {
              // iOS 不回传 isForMainFrame，缺省按主框架处理；子资源错误不惊扰用户。
              if (!(resourceError.isForMainFrame ?? true)) {
                return;
              }
              if (mounted) {
                setState(() => _loadError = '页面加载失败：${resourceError.description}');
              }
            },
          ),
        )
        ..loadRequest(uri);
      if (mounted) {
        setState(() {
          _web = web;
          _previewId = preview.previewId;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  /// 刷新当前预览：只 reload 已有 previewId，不新建会话。
  Future<void> _reloadPreview() async {
    final web = _web;
    if (web == null) {
      return;
    }
    setState(() => _loadError = null);
    await web.reload();
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _url,
                  decoration: const InputDecoration(hintText: 'http://127.0.0.1:5173'),
                ),
              ),
              FilledButton(
                onPressed: _open,
                child: Text(_previewId == null ? '打开预览' : '重新打开'),
              ),
            ],
          ),
        ),
        if (_targets.isNotEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Wrap(
                spacing: 8,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  const Text('候选 dev server：', style: TextStyle(fontSize: 12)),
                  for (final target in _targets)
                    ChoiceChip(
                      key: Key('browser-target-chip-${target.id}'),
                      label: Text(target.label),
                      selected: target.id == _selectedTargetId,
                      onSelected: (_) => setState(() {
                        _selectedTargetId = target.id;
                        _url.text = target.url;
                      }),
                    ),
                ],
              ),
            ),
          ),
        if (_error != null) Text(_error!),
        if (_previewId != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    'live preview · JS on · $_previewId',
                    key: const Key('browser-live-preview'),
                  ),
                ),
                IconButton(
                  key: const Key('browser-refresh'),
                  tooltip: '刷新',
                  onPressed: _reloadPreview,
                  icon: const Icon(Icons.refresh),
                ),
              ],
            ),
          ),
        if (_previewId != null)
          BrowserVerificationCard(previewId: _previewId!, client: _client),
        if (_loadProgress != null)
          LinearProgressIndicator(
            key: const Key('browser-load-progress'),
            value: _loadProgress! / 100,
          ),
        if (_loadError != null) Text(_loadError!),
        Expanded(
          child: _web == null
              ? const Center(child: Text('输入本机 dev server 地址后打开 live preview。'))
              : WebViewWidget(controller: _web!),
        ),
      ],
    );
  }
}

/// 一键验证当前 live preview 的摘要卡（对齐 web WorkbenchBrowserVerificationPanel）。
///
/// Business Logic（为什么需要这个组件）:
///   用户需要确认手机上的 live preview 是否真的可用：默认自动 smoke，不写脚本/选元素；
///   完成后展示状态、最终路径、console 错误数、断言失败数与截图，失败可重新验证。
///
/// Code Logic（这个组件做什么）:
///   「验证当前预览」→ client.startVerification（默认 smoke）→ 有界轮询
///   getVerification（默认 250ms × 60 次，对齐 web）→ 终态渲染摘要卡；
///   succeeded 且带 screenshotId 时拉取 artifact 渲染截图；
///   创建/轮询异常或超时上错误条并解锁按钮；busy 期间禁止重复触发；
///   previewId 变化时清空上一轮结果。
class BrowserVerificationCard extends StatefulWidget {
  const BrowserVerificationCard({
    super.key,
    required this.previewId,
    required this.client,
    this.pollInterval = const Duration(milliseconds: 250),
    this.maxPolls = 60,
  });

  final String previewId;
  final BrowserClient client;

  /// 轮询间隔；默认对齐 web 的 250ms，测试可注入更小值。
  final Duration pollInterval;

  /// 轮询次数上限；默认对齐 web 的 60 次，超过视为超时。
  final int maxPolls;

  @override
  State<BrowserVerificationCard> createState() => _BrowserVerificationCardState();
}

class _BrowserVerificationCardState extends State<BrowserVerificationCard> {
  bool _busy = false;
  String? _error;
  BrowserVerificationRun? _run;
  Uint8List? _screenshot;

  @override
  void didUpdateWidget(covariant BrowserVerificationCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    // preview 切换时上一轮结果失效，与 web 面板同语义。
    if (oldWidget.previewId != widget.previewId) {
      _run = null;
      _screenshot = null;
      _error = null;
    }
  }

  /// Business Logic: 用户需要「验证当前预览」一键拿到 smoke 结果，失败/超时要能重试。
  /// Code Logic: create → 未达终态时有界轮询 get → 终态后（成功且有 screenshotId）
  /// 拉取 artifact 解码截图；任何异常上错误条；timeout 单独给中文提示。
  Future<void> _start() async {
    if (_busy) {
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
      _run = null;
      _screenshot = null;
    });
    try {
      final requestId = 'req-${DateTime.now().microsecondsSinceEpoch}';
      var current = await widget.client.startVerification(
        previewId: widget.previewId,
        requestId: requestId,
      );
      if (!mounted) {
        return;
      }
      setState(() => _run = current);
      var polls = 0;
      while (!isBrowserVerificationTerminalState(current.session.state) &&
          polls < widget.maxPolls) {
        await Future<void>.delayed(widget.pollInterval);
        current = await widget.client.getVerification(runId: current.session.id);
        polls += 1;
        if (!mounted) {
          return;
        }
        setState(() => _run = current);
      }
      if (!isBrowserVerificationTerminalState(current.session.state)) {
        setState(() => _error = '验证超时，请重新验证');
      } else {
        await _loadScreenshot(current);
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = '$error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// Business Logic: 截图是验证成功的加分项，拉取失败不应吞掉整份摘要。
  /// Code Logic: 仅 succeeded 且 evidence 带 screenshotId 时拉 artifact；
  /// base64 解码失败静默跳过截图（摘要卡照常渲染）。
  Future<void> _loadScreenshot(BrowserVerificationRun run) async {
    final screenshotId = run.evidence?.screenshotId;
    if (run.session.state != 'succeeded' ||
        screenshotId == null ||
        screenshotId.isEmpty) {
      return;
    }
    try {
      final artifact = await widget.client.getVerificationArtifact(
        runId: run.session.id,
        artifactId: screenshotId,
      );
      if (artifact.base64.isEmpty || !mounted) {
        return;
      }
      final bytes = base64Decode(artifact.base64);
      setState(() => _screenshot = Uint8List.fromList(bytes));
    } catch (_) {
      // 截图拉取失败不影响状态/计数摘要。
    }
  }

  /// Business Logic: 状态徽章需要中文文案，与 web i18n 同语义。
  /// Code Logic: state → 中文标签；未知 state 原样展示。
  String _statusLabel(String state) {
    const labels = {
      'queued': '排队中',
      'running': '运行中',
      'succeeded': '成功',
      'failed': '失败',
      'canceled': '已取消',
    };
    return labels[state] ?? state;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final run = _run;
    final evidence = run?.evidence;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
      child: Column(
        key: const Key('browser-verification-panel'),
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              FilledButton.icon(
                key: const Key('browser-verify'),
                onPressed: _busy ? null : _start,
                icon: _busy
                    ? const SizedBox(
                        width: 14,
                        height: 14,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.verified_outlined, size: 16),
                label: const Text('验证当前预览'),
              ),
              const SizedBox(width: 8),
              if (run != null)
                Text(
                  _statusLabel(run.session.state),
                  key: const Key('browser-verify-status'),
                  style: theme.textTheme.labelLarge?.copyWith(
                    color: run.session.state == 'succeeded'
                        ? theme.colorScheme.primary
                        : run.session.state == 'failed'
                            ? theme.colorScheme.error
                            : theme.colorScheme.onSurface,
                  ),
                ),
            ],
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                _error!,
                key: const Key('browser-verify-error'),
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.error),
              ),
            ),
          if (evidence != null) ...[
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Column(
                key: const Key('browser-verify-summary'),
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('路径：${evidence.urlPath ?? '—'}',
                      style: theme.textTheme.bodySmall),
                  Text('控制台错误：${evidence.consoleErrorCount}',
                      style: theme.textTheme.bodySmall),
                  Text('断言失败：${evidence.assertionFailedCount}',
                      style: theme.textTheme.bodySmall),
                ],
              ),
            ),
            if (_screenshot != null)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxHeight: 220),
                  child: Image.memory(
                    _screenshot!,
                    key: const Key('browser-verify-screenshot'),
                    fit: BoxFit.contain,
                  ),
                ),
              ),
          ],
        ],
      ),
    );
  }
}
