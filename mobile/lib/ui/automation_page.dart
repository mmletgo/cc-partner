import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../automation/client.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../transfer/api.dart';

class AutomationPage extends StatefulWidget {
  const AutomationPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;

  @override
  State<AutomationPage> createState() => _AutomationPageState();
}

class _AutomationPageState extends State<AutomationPage> {
  late final AutomationClient _client;
  List<Map<String, dynamic>> _tasks = [];
  List<Map<String, dynamic>> _outbox = [];
  List<Map<String, dynamic>> _experiments = [];
  Map<String, dynamic>? _snapshot;
  Map<String, dynamic>? _detail;
  String? _error;
  bool _loading = true;
  final _title = TextEditingController();
  final _goal = TextEditingController();
  final _acceptance = TextEditingController();

  @override
  void initState() {
    super.initState();
    _client = AutomationClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  @override
  void dispose() {
    _title.dispose();
    _goal.dispose();
    _acceptance.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final tasks = await _client.listTasks(widget.project.id);
      final outbox = await _client.listOutbox(widget.project.id);
      final experiments = await _client.listExperiments(widget.project.id);
      final snapshot = await _client.runtimeSnapshot(widget.project.id);
      if (mounted) {
        setState(() {
          _tasks = tasks;
          _outbox = outbox;
          _experiments = experiments;
          _snapshot = snapshot;
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

  Future<void> _create() async {
    await _client.createTask(
      projectId: widget.project.id,
      title: _title.text,
      goal: _goal.text,
      acceptanceCriteria: _acceptance.text,
      clientRequestId: newClientOperationId(),
    );
    _title.clear();
    _goal.clear();
    _acceptance.clear();
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    return ListView(
      padding: const EdgeInsets.all(12),
      children: [
        if (_error != null) Text(_error!),
        if (_snapshot != null)
          Card(
            child: ListTile(
              title: const Text('runtime snapshot'),
              subtitle: Text('${_snapshot!['remoteStatus'] ?? _snapshot}'),
            ),
          ),
        const Text('创建任务'),
        TextField(controller: _title, decoration: const InputDecoration(labelText: '标题')),
        TextField(controller: _goal, decoration: const InputDecoration(labelText: '目标')),
        TextField(
          controller: _acceptance,
          decoration: const InputDecoration(labelText: '验收标准'),
        ),
        FilledButton(onPressed: _create, child: const Text('创建')),
        const Divider(),
        const Text('任务'),
        for (final task in _tasks)
          ListTile(
            title: Text(task['title'] as String? ?? task['id'] as String? ?? ''),
            subtitle: Text(task['id'] as String? ?? ''),
            onTap: () async {
              final detail = await _client.taskDetail(
                widget.project.id,
                task['id'] as String? ?? '',
              );
              if (mounted) {
                setState(() => _detail = detail);
              }
            },
          ),
        if (_detail != null) Text('详情: ${_detail!['title'] ?? _detail!['id']}'),
        const Divider(),
        const Text('outbox'),
        for (final item in _outbox)
          ListTile(
            title: Text(item['title'] as String? ?? item['id'] as String? ?? ''),
            subtitle: Text(item['status'] as String? ?? item['kind'] as String? ?? ''),
          ),
        const Divider(),
        const Text('experiments'),
        for (final item in _experiments)
          ListTile(
            title: Text(item['title'] as String? ?? item['id'] as String? ?? ''),
          ),
      ],
    );
  }
}
