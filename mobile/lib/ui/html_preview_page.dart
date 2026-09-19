import 'package:flutter/material.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../files/client.dart';
import '../files/html_preview.dart';

/// Desktop-style HTML preview: source / preview / split, scripts off.
class HtmlFilePreview extends StatefulWidget {
  const HtmlFilePreview({
    super.key,
    required this.client,
    required this.projectId,
    required this.path,
    required this.initialText,
    required this.onTextChanged,
  });

  final FilesClient client;
  final String projectId;
  final String path;
  final String initialText;
  final ValueChanged<String> onTextChanged;

  @override
  State<HtmlFilePreview> createState() => _HtmlFilePreviewState();
}

class _HtmlFilePreviewState extends State<HtmlFilePreview> {
  late final TextEditingController _source;
  late final WebViewController _web;
  String _mode = 'wysiwyg';
  bool _loadingPreview = false;
  String? _previewError;

  @override
  void initState() {
    super.initState();
    _source = TextEditingController(text: widget.initialText);
    _web = WebViewController()
      ..setJavaScriptMode(JavaScriptMode.disabled)
      ..setNavigationDelegate(
        NavigationDelegate(
          onNavigationRequest: (request) => NavigationDecision.prevent,
        ),
      );
    _refreshPreview();
  }

  @override
  void dispose() {
    _source.dispose();
    super.dispose();
  }

  Future<void> _refreshPreview() async {
    setState(() {
      _loadingPreview = true;
      _previewError = null;
    });
    try {
      final rewritten = await _rewrite(_source.text);
      await _web.loadHtmlString(rewritten, baseUrl: 'about:blank');
    } catch (error) {
      _previewError = error.toString();
    }
    if (mounted) {
      setState(() => _loadingPreview = false);
    }
  }

  Future<String> _rewrite(String html) async {
    final matches = RegExp(
      r'''(src|href)\s*=\s*(['"])(?!https?:|data:|//|#)([^'"]+)\2''',
      caseSensitive: false,
    ).allMatches(html);
    var result = html;
    for (final match in matches) {
      final relative = match.group(3)!;
      try {
        final asset = await widget.client.previewHtmlAsset(
          projectId: widget.projectId,
          documentPath: widget.path,
          assetPath: relative,
        );
        final dataUrl = asset['dataUrl'] as String? ?? '';
        if (dataUrl.isNotEmpty) {
          result = result.replaceFirst(match.group(0)!, '${match.group(1)!}=${match.group(2)!}$dataUrl${match.group(2)!}');
        }
      } catch (_) {
        // Keep original reference; preview may miss the asset.
      }
    }
    return result;
  }

  @override
  Widget build(BuildContext context) {
    assert(HtmlPreviewPolicy.scriptsEnabled == false);
    final sourceField = TextField(
      controller: _source,
      maxLines: null,
      expands: true,
      style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
      onChanged: (value) {
        widget.onTextChanged(value);
      },
    );
    final preview = Stack(
      children: [
        WebViewWidget(controller: _web),
        if (_loadingPreview) const LinearProgressIndicator(),
        if (_previewError != null)
          Align(
            alignment: Alignment.bottomCenter,
            child: Material(
              color: Theme.of(context).colorScheme.errorContainer,
              child: Padding(
                padding: const EdgeInsets.all(8),
                child: Text('预览失败，已留在源码：$_previewError'),
              ),
            ),
          ),
      ],
    );
    return Column(
      children: [
        SegmentedButton<String>(
          segments: const [
            ButtonSegment(value: 'source', label: Text('源码')),
            ButtonSegment(value: 'wysiwyg', label: Text('预览')),
            ButtonSegment(value: 'split', label: Text('分栏')),
          ],
          selected: {_mode},
          onSelectionChanged: (value) {
            setState(() => _mode = value.first);
            if (_mode != 'source') {
              _refreshPreview();
            }
          },
        ),
        Expanded(
          child: switch (_mode) {
            'source' => sourceField,
            'split' => Row(
                children: [
                  Expanded(child: sourceField),
                  const VerticalDivider(width: 1),
                  Expanded(child: preview),
                ],
              ),
            _ => preview,
          },
        ),
      ],
    );
  }
}
