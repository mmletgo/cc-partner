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

  /// Business Logic: 点击文件后应进入预览；打开失败（网络断开/文件已删除/权限受限）
  /// 必须显式提示并停留在列表页，不能静默无反应（对齐 web 失败提示语义）。
  /// Code Logic: 目录节点入栈并刷新；文件节点调 client.open，成功 push FilePreviewPage；
  /// 失败 SnackBar「打开文件失败：{原因}」后直接返回。
  Future<void> _open(Map<String, dynamic> node) async {
    final kind = node['kind'] as String? ?? node['type'] as String? ?? 'file';
    final path = node['path'] as String? ?? node['name'] as String? ?? '';
    if (kind == 'dir' || kind == 'directory') {
      _stack.add(path);
      await _reload();
      return;
    }
    final Map<String, dynamic> opened;
    try {
      opened = await _client.open(
        projectId: widget.project.id,
        path: path,
        worktreeId: widget.worktreeId,
      );
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('打开文件失败：$error')),
        );
      }
      return;
    }
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

  /// Business Logic: 目录列表加载失败时不能只给一行裸错误——用户需要明确的失败卡与
  /// 重试入口，且错误态同样可下拉刷新（对齐 web filesPanel 的 error + reload 语义）。
  /// Code Logic: RefreshIndicator 包住 AlwaysScrollable 列表，错误卡展示具体原因与
  /// 重试按钮，结构与项目页错误卡一致。
  Widget _errorPanel() {
    final theme = Theme.of(context);
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        children: [
          Card(
            key: const Key('files-error-card'),
            margin: const EdgeInsets.all(16),
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Icon(Icons.error_outline, color: theme.colorScheme.error),
                      const SizedBox(width: 8),
                      Text('加载失败', style: theme.textTheme.titleMedium),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Text(_error ?? '', style: theme.textTheme.bodySmall),
                  const SizedBox(height: 12),
                  FilledButton.icon(
                    key: const Key('files-error-retry'),
                    onPressed: _reload,
                    icon: const Icon(Icons.refresh),
                    label: const Text('重试'),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// Business Logic: 文件行需要元信息对齐 web filesPanel（名称 + 目录/大小）；
  /// 后端 size 字段缺失时退化为类型文案，不能渲染「null」。
  /// Code Logic: 目录显示「目录」；文件复用 workspace 的 B/KB/MB 分级格式化 size，
  /// size 缺失时显示「文件」。
  String _nodeMetaLabel(Map<String, dynamic> node) {
    final kind = node['kind'] as String? ?? node['type'] as String? ?? 'file';
    if (kind == 'dir' || kind == 'directory') {
      return '目录';
    }
    final size = (node['size'] as num?)?.toInt();
    return formatFileSizeLabel(size) ?? '文件';
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
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
          child: _error != null
              ? _errorPanel()
              : _nodes.isEmpty
                  ? RefreshIndicator(
                      onRefresh: _reload,
                      child: ListView(
                        physics: const AlwaysScrollableScrollPhysics(),
                        children: const [
                          SizedBox(height: 120),
                          Center(child: Text('空目录')),
                        ],
                      ),
                    )
                  : RefreshIndicator(
                      onRefresh: _reload,
                      child: ListView.builder(
                        physics: const AlwaysScrollableScrollPhysics(),
                        itemCount: _nodes.length,
                        itemBuilder: (context, index) {
                          final node = _nodes[index];
                          final name = node['name'] as String? ??
                              node['path'] as String? ??
                              '';
                          final kind = node['kind'] as String? ??
                              node['type'] as String? ??
                              'file';
                          return ListTile(
                            leading: Icon(kind == 'dir' || kind == 'directory'
                                ? Icons.folder
                                : Icons.insert_drive_file),
                            title: Text(name),
                            trailing: Text(
                              _nodeMetaLabel(node),
                              style: Theme.of(context).textTheme.bodySmall,
                            ),
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

  /// open 响应的 canEdit 门控：文本文件且后端明确允许编辑才能改；
  /// 缺 capabilities/canEdit 字段按 web falsy 语义处理为只读（fail-closed，
  /// 对齐 web canEditOpenedFile = text && capabilities.canEdit）。
  late final bool _canEdit;
  String _markdownMode = 'render';
  late SqlitePreviewState _sqlite;
  late final TextEditingController _editor;

  /// 保存失败的 inline 错误条；null 表示无错误。
  String? _saveError;

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
    final capabilities = widget.opened['capabilities'];
    final backendCanEdit =
        capabilities is Map && capabilities['canEdit'] == true;
    _canEdit = text is Map && backendCanEdit;
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

  /// Business Logic: 保存失败（含 baseHash 乐观锁冲突）必须 inline 上屏且文件保持
  /// dirty，否则用户以为已保存而丢失改动；成功才清 dirty 并提示。
  /// Code Logic: 调 client.saveText；成功 markClean + SnackBar + 返回 true；
  /// 失败按 409 冲突给「文件已在磁盘上变化」语义文案，其余展示原始错误，返回 false。
  Future<bool> _save() async {
    setState(() => _saveError = null);
    try {
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
      return true;
    } catch (error) {
      final conflict =
          error is LanHttpException && error.statusCode == 409;
      if (mounted) {
        setState(() {
          _saveError = conflict
              ? '保存文件失败：文件已在磁盘上变化，请刷新后再试'
              : '保存文件失败：$error';
        });
      }
      return false;
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
    final metadataText = fileMetadataText(widget.opened);
    final csv = widget.opened['csv'];
    final imageBytes = imageBytesFromOpenFile(widget.opened);
    Widget body;
    if (!_canEdit) {
      // canEdit=false：隐藏编辑器与保存（对齐 web canEditOpenedFile 门控），
      // 文本文件也只读展示说明。
      body = const _ReadonlyFileNote();
    } else if (imageBytes != null) {
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
          final saved = await _save();
          if (!saved || !context.mounted) {
            // 保存失败：错误条已在预览页上屏且文件保持 dirty，留在当前页。
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
            if (_canEdit &&
                (kind == FileKind.code ||
                    kind == FileKind.markdown ||
                    kind == FileKind.html))
              IconButton(
                key: const Key('files-save'),
                tooltip: '保存',
                onPressed: _save,
                icon: const Icon(Icons.save),
              ),
          ],
        ),
        body: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            ..._openBanners(),
            if (metadataText != null)
              Padding(
                key: const Key('files-open-metadata'),
                padding: const EdgeInsets.fromLTRB(12, 6, 12, 0),
                child: Text(
                  metadataText,
                  style: Theme.of(context).textTheme.labelSmall,
                ),
              ),
            if (_saveError != null)
              Material(
                key: const Key('files-save-error'),
                color: Theme.of(context).colorScheme.errorContainer,
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                  child: Row(
                    children: [
                      Icon(
                        Icons.error_outline,
                        size: 16,
                        color: Theme.of(context).colorScheme.onErrorContainer,
                      ),
                      const SizedBox(width: 6),
                      Expanded(
                        child: Text(
                          _saveError!,
                          style: Theme.of(context).textTheme.bodySmall?.copyWith(
                                color: Theme.of(context).colorScheme.onErrorContainer,
                              ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            Expanded(child: Padding(padding: const EdgeInsets.all(8), child: body)),
          ],
        ),
      ),
    );
  }

  /// Business Logic: 打开文件的 notice 与截断提示必须上屏，避免用户把节流后的
  /// 内容当成全文，或漏看服务端告警。
  /// Code Logic: notice 原样展示；truncated 用 web 同款文案；元信息行
  /// 「类型 · 大小 · 修改时间」来自 open 响应（宽容解析）。
  List<Widget> _openBanners() {
    final theme = Theme.of(context);
    final notice = openFileNotice(widget.opened);
    final truncated = openFileTruncated(widget.opened);
    Widget banner(String text, {Color? background, Color? foreground, String? key}) {
      return Material(
        key: key == null ? null : Key(key),
        color: background ?? theme.colorScheme.surfaceContainerHighest,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          child: Row(
            children: [
              Icon(Icons.info_outline, size: 16, color: foreground ?? theme.colorScheme.onSurface),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  text,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: foreground ?? theme.colorScheme.onSurface,
                  ),
                ),
              ),
            ],
          ),
        ),
      );
    }

    return [
      if (notice != null)
        banner(
          notice,
          background: theme.colorScheme.tertiaryContainer,
          foreground: theme.colorScheme.onTertiaryContainer,
          key: 'files-open-notice',
        ),
      if (truncated)
        banner(
          '文件内容已截断显示',
          background: theme.colorScheme.errorContainer,
          foreground: theme.colorScheme.onErrorContainer,
          key: 'files-open-truncated',
        ),
    ];
  }
}

/// canEdit=false 时的只读说明占位（对齐 web 只读预览语义：隐藏编辑器与保存）。
class _ReadonlyFileNote extends StatelessWidget {
  const _ReadonlyFileNote();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Column(
        key: const Key('files-readonly-note'),
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.lock_outline, size: 24, color: theme.colorScheme.onSurface),
          const SizedBox(height: 8),
          const Text('该文件不支持在手机上编辑，仅可查看。'),
        ],
      ),
    );
  }
}
