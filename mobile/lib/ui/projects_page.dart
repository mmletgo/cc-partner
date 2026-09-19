import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import 'project_home.dart';

class ProjectsPage extends StatefulWidget {
  const ProjectsPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<ProjectsPage> createState() => _ProjectsPageState();
}

class _ProjectsPageState extends State<ProjectsPage> {
  late final ProjectsClient _client;
  List<ProjectSummary> _items = [];
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _client = ProjectsClient(widget.http, widget.book.active!.baseUrl);
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

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    if (_items.isEmpty) {
      return const Center(child: Text('这台电脑还没有最近项目。请在桌面工作台打开一个文件夹。'));
    }
    return RefreshIndicator(
      onRefresh: _reload,
      child: ListView.builder(
        itemCount: _items.length,
        itemBuilder: (context, index) {
          final project = _items[index];
          return ListTile(
            leading: const Icon(Icons.folder),
            title: Text(project.name),
            subtitle: Text(project.path ?? project.kind ?? project.id),
            onTap: () {
              Navigator.of(context).push(
                MaterialPageRoute(
                  builder: (_) => ProjectHome(
                    book: widget.book,
                    http: widget.http,
                    project: project,
                  ),
                ),
              );
            },
            trailing: IconButton(
              icon: const Icon(Icons.delete_outline),
              onPressed: () => _remove(project),
            ),
          );
        },
      ),
    );
  }
}
