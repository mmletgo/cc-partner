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
