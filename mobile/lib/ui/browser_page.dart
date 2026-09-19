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
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;

  @override
  State<BrowserPage> createState() => _BrowserPageState();
}

class _BrowserPageState extends State<BrowserPage> {
  late final BrowserClient _client;
  final _url = TextEditingController(text: 'http://127.0.0.1:5173');
  WebViewController? _web;
  String? _error;
  String? _previewId;

  @override
  void initState() {
    super.initState();
    _client = BrowserClient(widget.http, widget.book.active!.baseUrl);
  }

  @override
  void dispose() {
    _url.dispose();
    super.dispose();
  }

  Future<void> _open() async {
    setState(() => _error = null);
    try {
      final preview = await _client.createPreview(
        projectId: widget.project.id,
        worktreeId: widget.worktreeId,
        targetUrl: _url.text.trim(),
      );
      final uri = Uri.parse('${widget.book.active!.baseUrl}${preview.mobileProxyPath}');
      final web = WebViewController()
        ..setJavaScriptMode(JavaScriptMode.unrestricted)
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
              FilledButton(onPressed: _open, child: const Text('打开预览')),
            ],
          ),
        ),
        if (_error != null) Text(_error!),
        if (_previewId != null)
          Text('live preview · JS on · $_previewId', key: const Key('browser-live-preview')),
        Expanded(
          child: _web == null
              ? const Center(child: Text('输入本机 dev server 地址后打开 live preview。'))
              : WebViewWidget(controller: _web!),
        ),
      ],
    );
  }
}
