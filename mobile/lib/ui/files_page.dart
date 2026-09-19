import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_markdown/flutter_markdown.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../files/client.dart';
import '../files/html_preview.dart';
import '../files/workspace.dart';
import '../projects/client.dart';

class FilesPage extends StatefulWidget {
  const FilesPage({super.key, required this.book, required this.http, required this.project});

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;

  @override
  State<FilesPage> createState() => _FilesPageState();
}

class _FilesPageState extends State<FilesPage> {
  late final FilesClient _client;
  final _workspace = FileWorkspaceController();
  final List<String> _stack = [''];
  List<Map<String, dynamic>> _nodes = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _client = FilesClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  String get _path => _stack.last;

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final nodes = await _client.listDir(
        projectId: widget.project.id,
        path: _path.isEmpty ? null : _path,
      );
      if (mounted) {
        setState(() {
          _nodes = nodes;
          _loading = false;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      }
    }
  }

  Future<void> _open(Map<String, dynamic> node) async {
    final kind = node['kind'] as String? ?? node['type'] as String? ?? 'file';
    final path = node['path'] as String? ?? node['name'] as String? ?? '';
    if (kind == 'dir' || kind == 'directory') {
      _stack.add(path);
      await _reload();
      return;
    }
    final opened = await _client.open(projectId: widget.project.id, path: path);
    if (!mounted) {
      return;
    }
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => FilePreviewPage(
          client: _client,
          projectId: widget.project.id,
          path: path,
          opened: opened,
          workspace: _workspace,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    return Column(
      children: [
        if (_stack.length > 1)
          ListTile(
            leading: const Icon(Icons.arrow_upward),
            title: const Text('上级目录'),
            onTap: () {
              _stack.removeLast();
              _reload();
            },
          ),
        Expanded(
          child: ListView.builder(
            itemCount: _nodes.length,
            itemBuilder: (context, index) {
              final node = _nodes[index];
              final name = node['name'] as String? ?? node['path'] as String? ?? '';
              final kind = node['kind'] as String? ?? 'file';
              return ListTile(
                leading: Icon(kind == 'dir' || kind == 'directory' ? Icons.folder : Icons.insert_drive_file),
                title: Text(name),
                onTap: () => _open(node),
              );
            },
          ),
        ),
      ],
    );
  }
}

class FilePreviewPage extends StatefulWidget {
  const FilePreviewPage({
    super.key,
    required this.client,
    required this.projectId,
    required this.path,
    required this.opened,
    required this.workspace,
  });

  final FilesClient client;
  final String projectId;
  final String path;
  final Map<String, dynamic> opened;
  final FileWorkspaceController workspace;

  @override
  State<FilePreviewPage> createState() => _FilePreviewPageState();
}

class _FilePreviewPageState extends State<FilePreviewPage> {
  late String _text;
  late String _hash;
  bool _htmlPreview = true;

  @override
  void initState() {
    super.initState();
    final text = widget.opened['text'];
    if (text is Map) {
      _text = text['content'] as String? ?? '';
      _hash = text['hash'] as String? ?? text['sha256'] as String? ?? '';
    } else {
      _text = '';
      _hash = '';
    }
  }

  Future<void> _save() async {
    await widget.client.saveText(
      projectId: widget.projectId,
      path: widget.path,
      content: _text,
      baseHash: _hash,
    );
    widget.workspace.markClean();
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已保存')));
    }
  }

  @override
  Widget build(BuildContext context) {
    final kind = detectFileKind(widget.path);
    final image = widget.opened['image'];
    final csv = widget.opened['csv'];
    final sqlite = widget.opened['sqlite'];
    Widget body;
    if (image is Map && image['dataUrl'] is String) {
      body = Image.network(image['dataUrl'] as String);
    } else if (image is Map && image['base64'] is String) {
      body = Image.memory(base64Decode(image['base64'] as String));
    } else if (csv is Map && csv['rows'] is List) {
      final rows = csv['rows'] as List;
      body = SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: DataTable(
          columns: [
            for (final header in (csv['headers'] as List? ?? ['col']))
              DataColumn(label: Text('$header')),
          ],
          rows: [
            for (final row in rows.take(200))
              DataRow(
                cells: [
                  for (final cell in (row is List ? row : [row]))
                    DataCell(Text('$cell')),
                ],
              ),
          ],
        ),
      );
    } else if (sqlite is Map) {
      body = SelectableText(const JsonEncoder.withIndent('  ').convert(sqlite));
    } else if (kind == FileKind.markdown) {
      body = Markdown(data: _text);
    } else if (kind == FileKind.html) {
      final html = rewriteRelativeAssetsToDataUrls(_text, loadAssetDataUrl: (path) => path);
      body = Column(
        children: [
          SwitchListTile(
            title: const Text('预览（脚本关闭）'),
            value: _htmlPreview,
            onChanged: (value) => setState(() => _htmlPreview = value),
          ),
          Expanded(
            child: _htmlPreview
                ? SingleChildScrollView(child: SelectableText(html))
                : TextField(
                    maxLines: null,
                    expands: true,
                    controller: TextEditingController(text: _text),
                    onChanged: (value) {
                      _text = value;
                      widget.workspace.markDirty(
                        projectId: widget.projectId,
                        worktreeId: '',
                        path: widget.path,
                      );
                    },
                  ),
          ),
        ],
      );
    } else {
      body = TextField(
        maxLines: null,
        expands: true,
        style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
        controller: TextEditingController(text: _text),
        onChanged: (value) {
          _text = value;
          widget.workspace.markDirty(
            projectId: widget.projectId,
            worktreeId: '',
            path: widget.path,
          );
        },
      );
    }
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.path.split('/').last),
        actions: [
          if (kind == FileKind.code || kind == FileKind.markdown || kind == FileKind.html)
            IconButton(onPressed: _save, icon: const Icon(Icons.save)),
        ],
      ),
      body: Padding(padding: const EdgeInsets.all(8), child: body),
    );
  }
}
