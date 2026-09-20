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

  /// 目录列表请求代数：每次 _reload 自增，await 返回后仍是最新代才允许
  /// setState（对齐 web MobileFilesPanel 的 listRequestIdRef 防旧响应覆盖）。
  int _listGeneration = 0;

  /// 文件打开请求代数：快速连点多个文件时只让最新一次 open 导航
  /// （对齐 web openRequestIdRef + context 双校验）。
  int _openGeneration = 0;

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
      // 上下文切换代表面板重建基线：正在返回的旧 open 请求必须失效
      // （对齐 web shouldInvalidateMobileFileOpenOnDirectoryLoad 的根目录语义）。
      _openGeneration += 1;
      _reload();
    }
  }

  String get _path => _stack.last;

  /// Business Logic: 对齐 web MobileFilesPanel 的 mobilePathCrumb——「上级目录」
  /// 旁展示当前目录路径的纯文本小字（root 显示「/」，即 web rootPath 同款），
  /// 长路径横向滚动查看；web 该元素为 span 纯文本，段落不可点击回跳，故同样只读。
  /// Code Logic: _path 为空（根目录）时返回「/」，否则原样返回当前路径。
  String get _crumbLabel => _path.isEmpty ? '/' : _path;

  /// Business Logic: 目录刷新不能让旧响应覆盖新状态——快速连点两个目录/切换
  /// worktree 时，晚到的旧 listDir 响应必须被丢弃（对齐 web listRequestIdRef）；
  /// 且刷新不该整页替换：已有列表数据时保留 crumb 与旧列表，仅显示行内加载指示
  /// （对齐 web loading 为行内状态行），只有首载（无任何列表数据）才整页 spinner。
  /// Code Logic: 自增 _listGeneration 作为本次请求代数；await 返回后代数不一致
  /// 或已卸载直接丢弃；根目录重载额外失效未完成的 open 请求（web 同款语义）。
  Future<void> _reload() async {
    final generation = ++_listGeneration;
    final projectId = widget.project.id;
    final worktreeId = widget.worktreeId;
    if (_path.isEmpty) {
      _openGeneration += 1;
    }
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final nodes = await _client.listDir(
        projectId: projectId,
        worktreeId: worktreeId,
        path: _path.isEmpty ? null : _path,
      );
      if (!mounted || generation != _listGeneration) {
        return;
      }
      setState(() {
        _nodes = nodes;
        _loading = false;
      });
    } catch (error) {
      if (!mounted || generation != _listGeneration) {
        return;
      }
      setState(() {
        _error = error.toString();
        _loading = false;
      });
    }
  }

  /// Business Logic: 点击文件后应进入预览；打开失败（网络断开/文件已删除/权限受限）
  /// 必须显式提示并停留在列表页，不能静默无反应（对齐 web 失败提示语义）；
  /// 快速连点两个文件时，旧 open 响应不能把用户带去旧文件（对齐 web openRequestIdRef
  /// + context 双校验）。
  /// Code Logic: 目录节点入栈并刷新（_reload 代数守卫使过期列表响应失效）；文件节点
  /// 调 client.open 前自增 _openGeneration，await 返回后校验仍是最新代且发起时的
  /// project/worktree 未变才 push FilePreviewPage，过期结果直接丢弃。
  Future<void> _open(Map<String, dynamic> node) async {
    final kind = node['kind'] as String? ?? node['type'] as String? ?? 'file';
    final path = node['path'] as String? ?? node['name'] as String? ?? '';
    if (kind == 'dir' || kind == 'directory') {
      _stack.add(path);
      await _reload();
      return;
    }
    final generation = ++_openGeneration;
    final projectId = widget.project.id;
    final worktreeId = widget.worktreeId;
    final Map<String, dynamic> opened;
    try {
      opened = await _client.open(
        projectId: projectId,
        path: path,
        worktreeId: worktreeId,
      );
    } catch (error) {
      if (mounted && generation == _openGeneration) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('打开文件失败：$error')),
        );
      }
      return;
    }
    if (!mounted || generation != _openGeneration) {
      return;
    }
    if (projectId != widget.project.id || worktreeId != widget.worktreeId) {
      return;
    }
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => FilePreviewPage(
          client: _client,
          projectId: projectId,
          worktreeId: worktreeId ?? '',
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
    // 首载（尚无任何列表数据）才整页 spinner；已有数据时刷新保留 crumb 与旧列表，
    // 仅在列表上方显示行内细进度条（对齐 web loading 为行内状态行）。
    if (_loading && _nodes.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    return Column(
      children: [
        // 当前目录路径 crumb（对齐 web mobilePathCrumb：muted 纯文本、root 为
        // 「/」、段不可点击；长路径横向滚动查看）。
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: Text(
              _crumbLabel,
              key: const Key('files-path-crumb'),
              style: Theme.of(context).textTheme.bodySmall?.copyWith(
                    color: Theme.of(context).colorScheme.onSurfaceVariant,
                  ),
            ),
          ),
        ),
        if (_loading)
          const LinearProgressIndicator(
            key: Key('files-refresh-indicator'),
            minHeight: 2,
          ),
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
  /// open 响应的可变副本：保存成功后用响应的 metadata / baseHash / baseModifiedAt
  /// 回写（对齐 web setOpened 合并），元信息行与乐观锁基线随之刷新。
  late Map<String, dynamic> _opened;
  late String _text;
  late String _hash;

  /// open 响应的 canEdit 门控：文本文件且后端明确允许编辑才能改；
  /// 缺 capabilities/canEdit 字段按 web falsy 语义处理为只读（fail-closed，
  /// 对齐 web canEditOpenedFile = text && capabilities.canEdit）。
  late final bool _canEdit;

  /// 是否存在未保存草稿：门控保存按钮（!dirty 禁用，对齐 web disabled={!dirty || saving}）。
  bool _dirty = false;

  /// 保存请求进行中：按钮显示「保存中」并禁用，防止并发重复保存。
  bool _saving = false;
  String _markdownMode = 'render';
  late SqlitePreviewState _sqlite;
  late final TextEditingController _editor;

  /// 保存失败的 inline 错误条；null 表示无错误。
  String? _saveError;

  @override
  void initState() {
    super.initState();
    _opened = Map<String, dynamic>.from(widget.opened);
    final text = _opened['text'];
    if (text is Map) {
      _text = text['content'] as String? ?? '';
      // 乐观锁基线读后端真实字段 baseHash（WorkbenchTextContent serde camelCase），
      // 兼容回退旧字段 hash；读错字段会让保存恒带空基线而必然 409。
      _hash = fileTextBaseHash(Map<String, dynamic>.from(text));
    } else {
      _text = '';
      _hash = '';
    }
    final capabilities = _opened['capabilities'];
    final backendCanEdit =
        capabilities is Map && capabilities['canEdit'] == true;
    _canEdit = text is Map && backendCanEdit;
    _editor = TextEditingController(text: _text);
    final sqlite = _opened['sqlite'];
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

  /// Business Logic: 用户编辑草稿后要立即反映到保存按钮门控（!dirty 禁用），
  /// 同时通知工作区 dirty guard 阻断跨 project/worktree 切换。
  /// Code Logic: setState 更新草稿文本与 _dirty 标记，并同步 workspace.markDirty。
  void _onDraftChanged(String value) {
    setState(() {
      _text = value;
      _dirty = true;
    });
    _markDirty();
  }

  /// Business Logic: 保存失败（含 baseHash 乐观锁冲突）必须 inline 上屏且文件保持
  /// dirty，否则用户以为已保存而丢失改动；成功才清 dirty 并提示。保存期间按钮
  /// 显示「保存中」并禁用，防止并发重复保存（对齐 web saving 状态）。
  /// Code Logic: 调 client.saveText；成功用返回的 metadata/baseHash/baseModifiedAt
  /// 回写 _opened（对齐 web setOpened 合并：元信息行刷新、同一次预览会话内
  /// 「编辑→保存→再编辑→再保存」不因旧基线误报 409），然后 markClean + SnackBar
  /// + 返回 true；失败按 409 冲突给「文件已在磁盘上变化」语义文案，其余展示原始
  /// 错误，返回 false。
  Future<bool> _save() async {
    if (_saving) {
      return false;
    }
    setState(() {
      _saveError = null;
      _saving = true;
    });
    try {
      final result = await widget.client.saveText(
        projectId: widget.projectId,
        path: widget.path,
        content: _text,
        baseHash: _hash,
        worktreeId: widget.worktreeId,
      );
      final nextHash = result['baseHash'];
      final nextModifiedAt = result['baseModifiedAt'];
      final metadata = result['metadata'];
      if (!mounted) {
        return false;
      }
      setState(() {
        if (metadata is Map) {
          _opened['metadata'] = Map<String, dynamic>.from(metadata);
        }
        final text = _opened['text'];
        if (nextHash is String && nextHash.isNotEmpty) {
          _hash = nextHash;
          if (text is Map) {
            text['baseHash'] = nextHash;
          }
        }
        if (text is Map && nextModifiedAt is String) {
          text['baseModifiedAt'] = nextModifiedAt;
        }
        _dirty = false;
        _saving = false;
      });
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
          _saving = false;
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
      onChanged: _onDraftChanged,
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
    final metadataText = fileMetadataText(_opened);
    final csv = _opened['csv'];
    final imageBytes = imageBytesFromOpenFile(_opened);
    final hasText = _opened['text'] is Map;
    Widget body;
    // 图片/CSV/SQLite 三类只读预览与 canEdit 无关（后端 open 响应里 text 与
    // image/csv/sqlite 是互斥 Option，非文本文件 text=null → _canEdit 恒 false，
    // 若被 !_canEdit 挡死三类预览将永远不可达；对齐 web 分支顺序）。
    if (imageBytes != null) {
      body = Image.memory(imageBytes);
    } else if (csv is Map && csv['rows'] is List) {
      final rows = csv['rows'] as List;
      body = SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: DataTable(
          columns: [
            // 表头字段是后端 DTO 的 columns（兼容回退旧字段 headers）。
            for (final header in csvPreviewColumns(Map<String, dynamic>.from(csv)))
              DataColumn(label: Text(header)),
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
    } else if (_opened['sqlite'] is Map) {
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
    } else if (!hasText) {
      // 完全未知的二进制（无 text/image/csv/sqlite 载荷）：兜底说明
      // （对齐 web readonlyUnsupported：「{type} 文件暂不支持移动端编辑」）。
      body = _UnsupportedFileNote(
        detectedType: (_opened['detectedType'] as String?)?.trim() ?? '',
      );
    } else if (!_canEdit) {
      // 文本文件但 canEdit=false：隐藏编辑器与保存（对齐 web canEditOpenedFile 门控），
      // 只读展示说明。
      body = const _ReadonlyFileNote();
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
        onTextChanged: _onDraftChanged,
      );
    } else {
      body = TextField(
        maxLines: null,
        expands: true,
        style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
        controller: _editor,
        onChanged: _onDraftChanged,
      );
    }
    return PopScope(
      canPop: !_dirty,
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
            if (_canEdit)
              // 保存门控对齐 web：disabled={!dirty || saving}，saving 显示「保存中」；
              // 凡 canEdit 的文本文件都可保存（web 不按扩展名收窄）。
              TextButton(
                key: const Key('files-save'),
                onPressed: (!_dirty || _saving) ? null : _save,
                child: Text(_saving ? '保存中' : '保存'),
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

/// 无 text/image/csv/sqlite 任何载荷的未知二进制兜底说明
/// （对齐 web readonlyUnsupported：「{type} 文件暂不支持移动端编辑」）。
class _UnsupportedFileNote extends StatelessWidget {
  const _UnsupportedFileNote({required this.detectedType});

  /// open 响应的后端检测类型（image/csv/sqlite/text/...；缺省显示「该文件」）。
  final String detectedType;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Column(
        key: const Key('files-unsupported-note'),
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.block_outlined, size: 24, color: theme.colorScheme.onSurface),
          const SizedBox(height: 8),
          Text(
            '${detectedType.isEmpty ? '该文件' : detectedType} 文件暂不支持移动端编辑',
          ),
        ],
      ),
    );
  }
}
