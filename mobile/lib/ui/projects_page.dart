import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/client.dart';
import '../transfer/api.dart';

class ProjectsPage extends StatefulWidget {
  const ProjectsPage({
    super.key,
    required this.book,
    required this.http,
    required this.onOpen,
  });

  final AddressBook book;
  final LanHttpClient http;
  final void Function(ProjectSummary project) onOpen;

  @override
  State<ProjectsPage> createState() => _ProjectsPageState();
}

class _ProjectsPageState extends State<ProjectsPage> {
  late final ProjectsClient _client;
  late final TransferApi _transfer;
  List<ProjectSummary> _items = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _client = ProjectsClient(widget.http, widget.book.active!.baseUrl);
    _transfer = TransferApi(widget.http, widget.book.active!.baseUrl);
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

  Future<void> _remove(ProjectSummary project) async {
    await _client.remove(project.id);
    await _reload();
  }

  Future<void> _openPicker({required bool lan}) async {
    final pathController = TextEditingController();
    final nameController = TextEditingController();
    final deviceController = TextEditingController();
    List<Map<String, dynamic>> devices = const [];
    if (lan) {
      try {
        devices = rankTransferTargets(await _transfer.listDevices());
      } catch (_) {}
    }
    if (!mounted) {
      return;
    }
    final opened = await showDialog<ProjectSummary>(
      context: context,
      builder: (context) {
        return AlertDialog(
          title: Text(lan ? '添加局域网目录' : '添加本机目录'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (lan)
                DropdownButtonFormField<String>(
                  items: [
                    for (final device in devices)
                      DropdownMenuItem(
                        value: device['id'] as String? ?? device['deviceId'] as String?,
                        child: Text(
                          '${device['name'] ?? device['id']}${device['isSelf'] == true ? ' · 主机' : ''}',
                        ),
                      ),
                  ],
                  onChanged: (value) => deviceController.text = value ?? '',
                  decoration: const InputDecoration(labelText: '设备'),
                ),
              TextField(
                controller: pathController,
                decoration: const InputDecoration(labelText: '目录路径'),
              ),
              TextField(
                controller: nameController,
                decoration: const InputDecoration(labelText: '新建一层文件夹（可选）'),
              ),
            ],
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
            FilledButton(
              onPressed: () async {
                var path = pathController.text.trim();
                final folder = nameController.text.trim();
                try {
                  if (lan) {
                    final deviceId = deviceController.text.trim();
                    if (folder.isNotEmpty) {
                      final created = await _client.createRemoteDir(
                        deviceId: deviceId,
                        parentPath: path,
                        name: folder,
                      );
                      path = created['path'] as String? ?? '$path/$folder';
                    }
                    final project = await _client.openRemote(deviceId: deviceId, path: path);
                    if (context.mounted) {
                      Navigator.pop(context, project);
                    }
                  } else {
                    if (folder.isNotEmpty) {
                      final created = await _client.createDir(parentPath: path, name: folder);
                      path = created['path'] as String? ?? '$path/$folder';
                    }
                    final project = await _client.open(path: path);
                    if (context.mounted) {
                      Navigator.pop(context, project);
                    }
                  }
                } catch (error) {
                  if (context.mounted) {
                    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$error')));
                  }
                }
              },
              child: const Text('打开'),
            ),
          ],
        );
      },
    );
    if (opened != null) {
      widget.onOpen(opened);
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
                onPressed: () => _openPicker(lan: false),
                child: const Text('添加本机目录'),
              ),
              const SizedBox(width: 8),
              OutlinedButton(
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
                              return ListTile(
                                leading: const Icon(Icons.folder),
                                title: Text(project.name),
                                subtitle: Text(project.path ?? project.kind ?? project.id),
                                onTap: () => widget.onOpen(project),
                                trailing: IconButton(
                                  icon: const Icon(Icons.delete_outline),
                                  onPressed: () => _remove(project),
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
