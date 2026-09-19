import 'package:flutter/material.dart';
import 'package:flutter_markdown/flutter_markdown.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../files/client.dart';
import '../files/workspace.dart';
import 'html_preview_page.dart';
import '../projects/client.dart';

class FilesPage extends StatefulWidget {
  const FilesPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.worktreeId,
    this.workspace,
    this.client,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? worktreeId;
  final FileWorkspaceController? workspace;
  final FilesClient? client;

  @override
  State<FilesPage> createState() => _FilesPageState();
}

class _FilesPageState extends State<FilesPage> {
  late final FilesClient _client;
  late final FileWorkspaceController _workspace;
  final List<String> _stack = [''];
  List<Map<String, dynamic>> _nodes = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _client = widget.client ?? FilesClient(widget.http, widget.book.active!.baseUrl);
    _workspace = widget.workspace ?? FileWorkspaceController();
    _reload();
  }

  @override
  void didUpdateWidget(covariant FilesPage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.project.id != widget.project.id ||
        oldWidget.worktreeId != widget.worktreeId) {
      _stack
        ..clear()
        ..add('');
      _reload();
    }
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
        worktreeId: widget.worktreeId,
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
    final opened = await _client.open(
      projectId: widget.project.id,
      path: path,
      worktreeId: widget.worktreeId,
    );
    if (!mounted) {
      return;
    }
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => FilePreviewPage(
          client: _client,
          projectId: widget.project.id,
          worktreeId: widget.worktreeId ?? '',
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
          child: RefreshIndicator(
            onRefresh: _reload,
            child: ListView.builder(
              physics: const AlwaysScrollableScrollPhysics(),
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
    required this.worktreeId,
    required this.path,
    required this.opened,
    required this.workspace,
  });

  final FilesClient client;
  final String projectId;
  final String worktreeId;
  final String path;
  final Map<String, dynamic> opened;
  final FileWorkspaceController workspace;

  @override
  State<FilePreviewPage> createState() => _FilePreviewPageState();
}

class _FilePreviewPageState extends State<FilePreviewPage> {
  late String _text;
  late String _hash;
  String _markdownMode = 'render';
  late SqlitePreviewState _sqlite;
  late final TextEditingController _editor;

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
    _editor = TextEditingController(text: _text);
    final sqlite = widget.opened['sqlite'];
    _sqlite = SqlitePreviewState.fromOpen(sqlite is Map ? Map<String, dynamic>.from(sqlite) : const {});
  }

  @override
  void dispose() {
    _editor.dispose();
    super.dispose();
  }

  void _markDirty() {
    widget.workspace.markDirty(
      projectId: widget.projectId,
      worktreeId: widget.worktreeId,
      path: widget.path,
    );
  }

  Future<void> _save() async {
    await widget.client.saveText(
      projectId: widget.projectId,
      path: widget.path,
      content: _text,
      baseHash: _hash,
      worktreeId: widget.worktreeId,
    );
    widget.workspace.markClean();
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已保存')));
    }
  }

  Future<void> _selectSqliteTable(String table) async {
    setState(() => _sqlite = _sqlite.selectTable(table));
    final preview = await widget.client.previewSqlite(
      projectId: widget.projectId,
      path: widget.path,
      worktreeId: widget.worktreeId,
      table: table,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _sqlite = SqlitePreviewState.fromOpen(preview);
    });
  }

  Widget _markdownBody() {
    final source = TextField(
      controller: _editor,
      maxLines: null,
      expands: true,
      onChanged: (value) {
        _text = value;
        _markDirty();
      },
    );
    final render = Markdown(data: _text);
    if (_markdownMode == 'source') {
      return source;
    }
    if (_markdownMode == 'split') {
      return Row(
        children: [
          Expanded(child: source),
          const VerticalDivider(width: 1),
          Expanded(child: render),
        ],
      );
    }
    return render;
  }

  @override
  Widget build(BuildContext context) {
    final kind = detectFileKind(widget.path);
    final csv = widget.opened['csv'];
    final imageBytes = imageBytesFromOpenFile(widget.opened);
    Widget body;
    if (imageBytes != null) {
      body = Image.memory(imageBytes);
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
            for (final row in rows)
              DataRow(
                cells: [
                  for (final cell in (row is List ? row : [row]))
                    DataCell(Text('$cell')),
                ],
              ),
          ],
        ),
      );
    } else if (widget.opened['sqlite'] is Map) {
      body = Column(
        children: [
          DropdownButton<String>(
            value: _sqlite.selectedTable,
            hint: const Text('选择表'),
            items: [
              for (final table in _sqlite.tables)
                DropdownMenuItem(value: table, child: Text(table)),
            ],
            onChanged: (value) {
              if (value != null) {
                _selectSqliteTable(value);
              }
            },
          ),
          Expanded(
            child: ListView(
              children: [
                for (final row in _sqlite.rows)
                  ListTile(title: Text('$row')),
              ],
            ),
          ),
        ],
      );
    } else if (kind == FileKind.markdown) {
      body = Column(
        children: [
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'source', label: Text('源码')),
              ButtonSegment(value: 'render', label: Text('渲染')),
              ButtonSegment(value: 'split', label: Text('分栏')),
            ],
            selected: {_markdownMode},
            onSelectionChanged: (value) => setState(() => _markdownMode = value.first),
          ),
          Expanded(child: _markdownBody()),
        ],
      );
    } else if (kind == FileKind.html) {
      body = HtmlFilePreview(
        client: widget.client,
        projectId: widget.projectId,
        path: widget.path,
        initialText: _text,
        onTextChanged: (value) {
          _text = value;
          _markDirty();
        },
      );
    } else {
      body = TextField(
        maxLines: null,
        expands: true,
        style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
        controller: _editor,
        onChanged: (value) {
          _text = value;
          _markDirty();
        },
      );
    }
    return PopScope(
      canPop: !widget.workspace.snapshot.dirty,
      onPopInvokedWithResult: (didPop, _) async {
        if (didPop) {
          return;
        }
        final choice = await showDialog<String>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text('未保存的文件'),
            actions: [
              TextButton(onPressed: () => Navigator.pop(context, 'cancel'), child: const Text('取消')),
              TextButton(onPressed: () => Navigator.pop(context, 'discard'), child: const Text('丢弃')),
              FilledButton(onPressed: () => Navigator.pop(context, 'save'), child: const Text('保存')),
            ],
          ),
        );
        if (!context.mounted) {
          return;
        }
        if (choice == 'save') {
          await _save();
          if (!context.mounted) {
            return;
          }
          Navigator.of(context).pop();
        } else if (choice == 'discard') {
          widget.workspace.markClean();
          Navigator.of(context).pop();
        }
      },
      child: Scaffold(
        appBar: AppBar(
          title: Text(widget.path.split('/').last),
          actions: [
            if (kind == FileKind.code || kind == FileKind.markdown || kind == FileKind.html)
              IconButton(onPressed: _save, icon: const Icon(Icons.save)),
          ],
        ),
        body: Padding(padding: const EdgeInsets.all(8), child: body),
      ),
    );
  }
}
