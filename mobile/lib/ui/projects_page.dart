import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/api.dart';
import 'project_dir_picker.dart';

/// Workbench 项目列表页：最近项目 + 添加本机 / 局域网目录（目录浏览式选择器）。
class ProjectsPage extends StatefulWidget {
  const ProjectsPage({
    super.key,
    required this.book,
    required this.http,
    required this.onOpen,
    this.client,
    this.transferApi,
  });

  final AddressBook book;
  final LanHttpClient http;
  final void Function(ProjectSummary project) onOpen;

  /// 测试注入的项目接口；为空时按当前主机构造。
  final ProjectsClient? client;

  /// 测试注入的设备列表接口；为空时按当前主机构造。
  final TransferApi? transferApi;

  @override
  State<ProjectsPage> createState() => _ProjectsPageState();
}

class _ProjectsPageState extends State<ProjectsPage> {
  late final ProjectsClient _client;
  late final TransferApi _transfer;
  final Set<String> _removingIds = {};
  List<ProjectSummary> _items = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    final baseUrl = widget.book.active?.baseUrl ?? '';
    _client = widget.client ?? ProjectsClient(widget.http, baseUrl);
    _transfer = widget.transferApi ?? TransferApi(widget.http, baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final items = await _client.listRecent();
      if (mounted) {
        setState(() {
          _items = items;
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

  /// 打开目录浏览式选择器；成功打开项目后回传给上层导航。
  Future<void> _openPicker({required bool lan}) async {
    final opened = await showProjectDirPicker(
      context,
      client: _client,
      transferApi: _transfer,
      lan: lan,
    );
    if (opened != null) {
      widget.onOpen(opened);
    }
  }

  /// 先弹确认框，确认后才从最近列表移除；移除中按钮禁用防重复提交。
  Future<void> _remove(ProjectSummary project) async {
    if (_removingIds.contains(project.id)) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('remove-confirm'),
        title: Text('移除项目「${project.name}」？'),
        content: const Text('只会从这台电脑的最近列表移除，不会删除文件。'),
        actions: [
          TextButton(
            key: const Key('remove-confirm-cancel'),
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const Key('remove-confirm-accept'),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('移除'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) {
      return;
    }
    setState(() => _removingIds.add(project.id));
    try {
      await _client.remove(project.id);
      await _reload();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('$error')),
        );
      }
    } finally {
      if (mounted) {
        setState(() => _removingIds.remove(project.id));
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
              FilledButton(
                key: const Key('project-add-local'),
                onPressed: () => _openPicker(lan: false),
                child: const Text('添加本机目录'),
              ),
              const SizedBox(width: 8),
              OutlinedButton(
                key: const Key('project-add-lan'),
                onPressed: () => _openPicker(lan: true),
                child: const Text('添加局域网目录'),
              ),
            ],
          ),
        ),
        Expanded(
          child: _loading
              ? const Center(child: CircularProgressIndicator())
              : _error != null
                  ? Center(child: Text(_error!))
                  : _items.isEmpty
                      ? const Center(child: Text('这台电脑还没有最近项目。请添加本机或局域网目录。'))
                      : RefreshIndicator(
                          onRefresh: _reload,
                          child: ListView.builder(
                            itemCount: _items.length,
                            itemBuilder: (context, index) {
                              final project = _items[index];
                              final removing = _removingIds.contains(project.id);
                              return ListTile(
                                leading: const Icon(Icons.folder),
                                title: Text(project.name),
                                subtitle: Text(project.path ?? project.kind ?? project.id),
                                onTap: () => widget.onOpen(project),
                                trailing: IconButton(
                                  key: Key('project-remove-${project.id}'),
                                  icon: removing
                                      ? const SizedBox(
                                          width: 16,
                                          height: 16,
                                          child: CircularProgressIndicator(strokeWidth: 2),
                                        )
                                      : const Icon(Icons.delete_outline),
                                  onPressed: removing ? null : () => _remove(project),
                                ),
                              );
                            },
                          ),
                        ),
        ),
      ],
    );
  }
}
