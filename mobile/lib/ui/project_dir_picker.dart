import 'package:flutter/material.dart';

import '../projects/client.dart';
import '../transfer/api.dart';
import '../transfer/client.dart';

/// 目录浏览列表里的一行条目（本机 fs/list 与远端 remote/list 返回结构一致）。
class ProjectDirEntry {
  const ProjectDirEntry({
    required this.name,
    required this.path,
    required this.kind,
    this.isGitRepo = false,
  });

  final String name;
  final String path;
  final String kind;
  final bool isGitRepo;

  factory ProjectDirEntry.fromJson(Map<String, dynamic> json) => ProjectDirEntry(
        name: json['name'] as String? ?? '',
        path: json['path'] as String? ?? '',
        kind: json['kind'] as String? ?? 'file',
        isGitRepo: json['isGitRepo'] == true,
      );

  /// 只有目录可以继续进入；文件仅作展示。
  bool get isDir => kind == 'dir';
}

/// 排序条目：目录在前，同类型按名称不区分大小写升序，对齐网页版浏览体验。
List<ProjectDirEntry> sortProjectDirEntries(List<ProjectDirEntry> entries) {
  final copy = List<ProjectDirEntry>.from(entries);
  copy.sort((a, b) {
    final aRank = a.isDir ? 0 : 1;
    final bRank = b.isDir ? 0 : 1;
    if (aRank != bRank) {
      return aRank - bRank;
    }
    return a.name.toLowerCase().compareTo(b.name.toLowerCase());
  });
  return copy;
}

/// 计算上级路径；已是根目录（含 Windows 盘符根）时返回 null 表示不能再往上级。
String? parentProjectPath(String path) {
  final trimmed = path.trim();
  if (trimmed.isEmpty) {
    return null;
  }
  final lastSlash = trimmed.lastIndexOf('/');
  final lastBackslash = trimmed.lastIndexOf(r'\');
  if (lastBackslash > lastSlash) {
    final withoutTrailing = trimmed.replaceAll(RegExp(r'\\+$'), '');
    if (withoutTrailing.isEmpty || RegExp(r'^[A-Za-z]:$').hasMatch(withoutTrailing)) {
      return null;
    }
    final parentIndex = withoutTrailing.lastIndexOf(r'\');
    if (parentIndex < 0) {
      return null;
    }
    final parent = withoutTrailing.substring(0, parentIndex);
    return RegExp(r'^[A-Za-z]:$').hasMatch(parent) ? '$parent\\' : parent;
  }
  if (trimmed == '/') {
    return null;
  }
  final withoutTrailing = trimmed.replaceAll(RegExp(r'/+$'), '');
  if (withoutTrailing.isEmpty) {
    return null;
  }
  final parentIndex = withoutTrailing.lastIndexOf('/');
  if (parentIndex < 0) {
    return null;
  }
  if (parentIndex == 0) {
    return '/';
  }
  return withoutTrailing.substring(0, parentIndex);
}

/// 拼接子路径；父路径已带分隔符（如 `C:\`）时直接追加名字。
String joinProjectPath(String parent, String name) {
  if (parent.endsWith('/') || parent.endsWith(r'\')) {
    return '$parent$name';
  }
  return '$parent/$name';
}

/// 新建文件夹名字校验：非空、不是 `.`/`..`、不含斜杠；最终以后端校验为准。
bool isValidProjectChildName(String name) {
  final trimmed = name.trim();
  return trimmed.isNotEmpty &&
      trimmed != '.' &&
      trimmed != '..' &&
      !trimmed.contains('/') &&
      !trimmed.contains(r'\');
}

/// 打开目录浏览选择器；用户点「打开此目录」成功后返回项目摘要，取消返回 null。
Future<ProjectSummary?> showProjectDirPicker(
  BuildContext context, {
  required ProjectsClient client,
  required TransferApi transferApi,
  required bool lan,
}) {
  return Navigator.of(context).push<ProjectSummary>(
    MaterialPageRoute<ProjectSummary>(
      builder: (_) => ProjectDirPicker(
        client: client,
        transferApi: transferApi,
        lan: lan,
      ),
    ),
  );
}

enum _PickerMode { lanDevices, browse }

/// 项目目录浏览选择器（添加本机 / 局域网项目）。
///
/// Business Logic: 手机没有系统目录选择框，用户需要逐级浏览主机或局域网对端
/// 目录并挑一个目录打开为 Workbench 项目。
/// Code Logic: lan=true 先列设备（主机置顶）再走远端接口逐级浏览；
/// lan=false 直接从本机根目录起步浏览；busy 期间禁止关闭与重复提交。
class ProjectDirPicker extends StatefulWidget {
  const ProjectDirPicker({
    super.key,
    required this.client,
    required this.transferApi,
    required this.lan,
  });

  final ProjectsClient client;
  final TransferApi transferApi;
  final bool lan;

  @override
  State<ProjectDirPicker> createState() => _ProjectDirPickerState();
}

class _ProjectDirPickerState extends State<ProjectDirPicker> {
  _PickerMode _mode = _PickerMode.browse;
  List<Map<String, dynamic>> _devices = const [];
  bool _devicesLoading = false;
  String? _devicesError;
  String? _deviceId;
  String? _currentPath;
  List<ProjectDirEntry> _entries = const [];
  bool _entriesLoading = false;
  String? _browseError;
  bool _openBusy = false;

  @override
  void initState() {
    super.initState();
    if (widget.lan) {
      _mode = _PickerMode.lanDevices;
      _loadDevices();
    } else {
      _loadRoots();
    }
  }

  bool get _busy => _openBusy;

  /// 拉取局域网设备列表；主机（isSelf）已被置顶并带「· 主机」标签。
  Future<void> _loadDevices() async {
    setState(() {
      _devicesLoading = true;
      _devicesError = null;
    });
    try {
      final devices = rankTransferTargets(await widget.transferApi.listDevices());
      if (!mounted) {
        return;
      }
      setState(() {
        _devices = devices;
        _devicesLoading = false;
      });
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _devicesError = '$error';
        _devicesLoading = false;
      });
    }
  }

  /// 选定设备后切到浏览态并从该设备根目录起步；未选设备不能进入浏览。
  Future<void> _selectDevice(Map<String, dynamic> device) async {
    if (_busy) {
      return;
    }
    setState(() {
      _deviceId = transferDeviceId(device);
      _mode = _PickerMode.browse;
    });
    await _loadRoots();
  }

  /// 加载根目录并把浏览位置落到第一个根。
  Future<void> _loadRoots() async {
    setState(() {
      _entriesLoading = true;
      _browseError = null;
      _entries = const [];
    });
    try {
      final roots = widget.lan
          ? await widget.client.listRemoteRoots(_deviceId ?? '')
          : await widget.client.listLocalRoots();
      if (!mounted) {
        return;
      }
      final first = roots.isEmpty ? null : roots.first['path'] as String?;
      setState(() {
        _currentPath = (first == null || first.isEmpty) ? null : first;
      });
      if (_currentPath != null) {
        await _loadEntries();
      } else {
        setState(() => _entriesLoading = false);
      }
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _entriesLoading = false;
        _browseError = '$error';
      });
    }
  }

  /// 进入某个目录（含返回上级 / 新建文件夹后落入新目录）。
  Future<void> _browse(String path) async {
    if (_busy) {
      return;
    }
    setState(() => _currentPath = path);
    await _loadEntries();
  }

  /// 列出当前路径的条目并排序展示。
  Future<void> _loadEntries() async {
    final path = _currentPath;
    if (path == null) {
      return;
    }
    setState(() {
      _entriesLoading = true;
      _browseError = null;
      _entries = const [];
    });
    try {
      final raw = widget.lan
          ? await widget.client.listRemoteDir(deviceId: _deviceId ?? '', path: path)
          : await widget.client.listLocalDir(path);
      if (!mounted) {
        return;
      }
      setState(() {
        _entries = sortProjectDirEntries(
          raw.map(ProjectDirEntry.fromJson).toList(growable: false),
        );
        _entriesLoading = false;
      });
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _entriesLoading = false;
        _browseError = '$error';
      });
    }
  }

  /// 失败重试：还没拿到根就重拉根，否则重拉当前目录。
  void _retry() {
    if (_currentPath == null) {
      _loadRoots();
    } else {
      _loadEntries();
    }
  }

  void _goUp() {
    final path = _currentPath;
    if (path == null) {
      return;
    }
    final parent = parentProjectPath(path);
    if (parent != null) {
      _browse(parent);
    }
  }

  /// 把当前目录打开为项目；成功后 pop 回项目页并带回 ProjectSummary。
  Future<void> _open() async {
    final path = _currentPath;
    if (path == null || _busy) {
      return;
    }
    setState(() {
      _openBusy = true;
      _browseError = null;
    });
    try {
      final project = widget.lan
          ? await widget.client.openRemote(deviceId: _deviceId ?? '', path: path)
          : await widget.client.open(path: path);
      if (!mounted) {
        return;
      }
      Navigator.of(context).pop(project);
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _openBusy = false;
        _browseError = '$error';
      });
    }
  }

  /// 弹出新建文件夹输入框；成功后浏览位置落到新文件夹。
  Future<void> _showCreateDialog() async {
    final parent = _currentPath;
    if (parent == null || _busy) {
      return;
    }
    final newPath = await showDialog<String>(
      context: context,
      barrierDismissible: false,
      builder: (_) => _CreateFolderDialog(
        onCreate: (name) async {
          final created = widget.lan
              ? await widget.client.createRemoteDir(
                  deviceId: _deviceId ?? '',
                  parentPath: parent,
                  name: name,
                )
              : await widget.client.createDir(parentPath: parent, name: name);
          return created['path'] as String? ?? joinProjectPath(parent, name);
        },
      ),
    );
    if (newPath != null) {
      await _browse(newPath);
    }
  }

  @override
  Widget build(BuildContext context) {
    final pickingDevices = widget.lan && _mode == _PickerMode.lanDevices;
    return PopScope(
      canPop: !_busy,
      child: Scaffold(
        appBar: AppBar(
          title: Text(
            widget.lan
                ? (pickingDevices ? '选择设备' : '添加局域网目录')
                : '添加本机目录',
          ),
        ),
        body: pickingDevices ? _buildDeviceList() : _buildBrowser(),
        bottomNavigationBar: pickingDevices ? null : _buildFooter(),
      ),
    );
  }

  Widget _buildDeviceList() {
    if (_devicesLoading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_devicesError != null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.all(16),
              child: Text(_devicesError!, textAlign: TextAlign.center),
            ),
            TextButton(
              key: const Key('picker-devices-retry'),
              onPressed: _loadDevices,
              child: const Text('重试'),
            ),
          ],
        ),
      );
    }
    if (_devices.isEmpty) {
      return const Center(child: Text('没有可用的局域网设备'));
    }
    return ListView.builder(
      itemCount: _devices.length,
      itemBuilder: (context, index) {
        final device = _devices[index];
        return ListTile(
          key: Key('picker-device-${transferDeviceId(device)}'),
          leading: const Icon(Icons.computer),
          title: Text(transferDeviceLabel(device)),
          onTap: () => _selectDevice(device),
        );
      },
    );
  }

  Widget _buildBrowser() {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  _currentPath ?? '（尚未选择目录）',
                  key: const Key('picker-path'),
                  style: Theme.of(context).textTheme.bodySmall,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              const SizedBox(width: 8),
              TextButton(
                key: const Key('picker-parent'),
                onPressed:
                    (_busy || _currentPath == null) ? null : _goUp,
                child: const Text('返回上级'),
              ),
            ],
          ),
        ),
        const Divider(height: 1),
        Expanded(child: _buildEntryList()),
      ],
    );
  }

  Widget _buildEntryList() {
    if (_entriesLoading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_browseError != null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.all(16),
              child: Text(_browseError!, textAlign: TextAlign.center),
            ),
            TextButton(
              key: const Key('picker-retry'),
              onPressed: _retry,
              child: const Text('重试'),
            ),
          ],
        ),
      );
    }
    if (_entries.isEmpty) {
      return const Center(child: Text('这个目录是空的'));
    }
    return ListView.builder(
      itemCount: _entries.length,
      itemBuilder: (context, index) {
        final entry = _entries[index];
        return ListTile(
          key: Key('picker-entry-${entry.name}'),
          leading: Icon(entry.isDir ? Icons.folder : Icons.insert_drive_file),
          title: Text(entry.name),
          subtitle: Text(entry.path, overflow: TextOverflow.ellipsis),
          trailing: entry.isGitRepo
              ? const Text('Git', style: TextStyle(fontSize: 12))
              : null,
          onTap: entry.isDir && !_busy ? () => _browse(entry.path) : null,
        );
      },
    );
  }

  Widget _buildFooter() {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
        child: Row(
          children: [
            OutlinedButton(
              key: const Key('picker-create'),
              onPressed: (_busy || _currentPath == null) ? null : _showCreateDialog,
              child: const Text('新建文件夹'),
            ),
            const Spacer(),
            FilledButton(
              key: const Key('picker-open'),
              onPressed: (_busy || _currentPath == null) ? null : _open,
              child: const Text('打开此目录'),
            ),
          ],
        ),
      ),
    );
  }
}

/// 新建文件夹输入弹窗：busy 期间不可关闭，成功后把新路径带回选择器。
class _CreateFolderDialog extends StatefulWidget {
  const _CreateFolderDialog({required this.onCreate});

  final Future<String> Function(String name) onCreate;

  @override
  State<_CreateFolderDialog> createState() => _CreateFolderDialogState();
}

class _CreateFolderDialogState extends State<_CreateFolderDialog> {
  final TextEditingController _nameController = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _nameController.dispose();
    super.dispose();
  }

  bool get _nameValid => isValidProjectChildName(_nameController.text);

  /// 提交创建；失败时在弹窗内展示错误并可重试。
  Future<void> _submit() async {
    final name = _nameController.text.trim();
    if (!isValidProjectChildName(name) || _busy) {
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final newPath = await widget.onCreate(name);
      if (!mounted) {
        return;
      }
      Navigator.of(context).pop(newPath);
    } catch (error) {
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _error = '$error';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      canPop: !_busy,
      child: AlertDialog(
        title: const Text('新建文件夹'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              key: const Key('picker-create-name'),
              controller: _nameController,
              autofocus: true,
              enabled: !_busy,
              decoration: const InputDecoration(labelText: '文件夹名称'),
              onChanged: (_) => setState(() {}),
              onSubmitted: (_) => _submit(),
            ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(
                  _error!,
                  style: TextStyle(
                    color: Theme.of(context).colorScheme.error,
                    fontSize: 12,
                  ),
                ),
              ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: _busy ? null : () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const Key('picker-create-confirm'),
            onPressed: _nameValid && !_busy ? _submit : null,
            child: const Text('创建'),
          ),
        ],
      ),
    );
  }
}
