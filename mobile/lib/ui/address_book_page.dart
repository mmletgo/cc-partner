import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../address_book/models.dart';
import '../core/health_probe.dart';
import '../core/lan_http.dart';
import '../core/server_url.dart';
import '../settings/risk_copy.dart';

/// Address book screen: save and switch LAN PC servers.
class AddressBookPage extends StatefulWidget {
  const AddressBookPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<AddressBookPage> createState() => _AddressBookPageState();
}

class _AddressBookPageState extends State<AddressBookPage> {
  String? _busyId;
  String? _error;

  AddressBook get _book => widget.book;

  Future<void> _addServer() async {
    final result = await showDialog<_AddResult>(
      context: context,
      builder: (context) => const _AddServerDialog(),
    );
    if (result == null || !mounted) {
      return;
    }
    setState(() {
      _error = null;
      _busyId = 'add';
    });
    try {
      await _book.addFromInput(
        result.input,
        name: result.name,
        forceIfUnreachable: result.force,
        probe: (baseUrl) => probeLanHealth(widget.http, baseUrl),
      );
      if (mounted) {
        setState(() => _busyId = null);
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _busyId = null;
          _error = error.toString();
        });
      }
    }
  }

  Future<void> _switchTo(ServerRecord server) async {
    setState(() {
      _book.switchActive(server.id);
      _error = null;
    });
    await _book.persist();
  }

  Future<void> _remove(ServerRecord server) async {
    await _book.remove(server.id);
    if (mounted) {
      setState(() {});
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('cc-partner 地址簿')),
      floatingActionButton: FloatingActionButton(
        key: const Key('add-server'),
        onPressed: _busyId == null ? _addServer : null,
        tooltip: '添加 PC',
        child: const Icon(Icons.add),
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Card(
            color: Theme.of(context).colorScheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Text(
                kLanRiskStatement,
                key: const Key('risk-copy'),
                style: Theme.of(context).textTheme.bodyMedium,
              ),
            ),
          ),
          if (_error != null) ...[
            const SizedBox(height: 12),
            Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
          ],
          if (_busyId != null) ...[
            const SizedBox(height: 12),
            const LinearProgressIndicator(),
          ],
          const SizedBox(height: 16),
          if (_book.servers.isEmpty)
            const Text('还没有 PC。点右下角添加局域网地址，或粘贴桌面二维码里的 URL。')
          else
            ..._book.servers.map(_serverTile),
        ],
      ),
    );
  }

  Widget _serverTile(ServerRecord server) {
    final active = server.id == _book.activeServerId;
    final label = server.name.isNotEmpty
        ? server.name
        : (server.deviceName ?? server.baseUrl);
    return Card(
      key: Key('server-${server.id}'),
      child: ListTile(
        leading: Icon(
          active ? Icons.check_circle : Icons.lan_outlined,
          color: active ? Theme.of(context).colorScheme.primary : null,
        ),
        title: Text(label),
        subtitle: Text(
          '${server.baseUrl}\n${_healthLabel(server.lastHealth)}'
          '${active ? ' · 当前' : ''}',
        ),
        isThreeLine: true,
        onTap: () => _switchTo(server),
        trailing: IconButton(
          icon: const Icon(Icons.delete_outline),
          onPressed: () => _remove(server),
        ),
      ),
    );
  }

  String _healthLabel(ServerHealth health) {
    switch (health) {
      case ServerHealth.online:
        return '在线';
      case ServerHealth.unsupported:
        return '协议过旧';
      case ServerHealth.unreachable:
        return '离线';
    }
  }
}

class _AddResult {
  const _AddResult({
    required this.input,
    required this.name,
    required this.force,
  });
  final String input;
  final String name;
  final bool force;
}

class _AddServerDialog extends StatefulWidget {
  const _AddServerDialog();

  @override
  State<_AddServerDialog> createState() => _AddServerDialogState();
}

class _AddServerDialogState extends State<_AddServerDialog> {
  final _host = TextEditingController();
  final _port = TextEditingController(text: '$kDefaultLanPort');
  final _name = TextEditingController();
  bool _force = false;

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _name.dispose();
    super.dispose();
  }

  void _submit() {
    final host = _host.text.trim();
    if (host.isEmpty) {
      return;
    }
    final port = _port.text.trim();
    final input = host.contains('://') ? host : '$host:$port';
    Navigator.of(context).pop(
      _AddResult(input: input, name: _name.text.trim(), force: _force),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('添加 PC'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              key: const Key('server-host'),
              controller: _host,
              decoration: const InputDecoration(
                labelText: '主机或 URL',
                hintText: '192.168.1.8 或 http://ip:端口/mobile',
              ),
              autofocus: true,
            ),
            TextField(
              key: const Key('server-port'),
              controller: _port,
              decoration: const InputDecoration(labelText: '端口'),
              keyboardType: TextInputType.number,
            ),
            TextField(
              controller: _name,
              decoration: const InputDecoration(labelText: '名称（可选）'),
            ),
            CheckboxListTile(
              key: const Key('force-save'),
              contentPadding: EdgeInsets.zero,
              title: const Text('探测失败仍保存（标为离线）'),
              value: _force,
              onChanged: (value) => setState(() => _force = value ?? false),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          key: const Key('confirm-add'),
          onPressed: _submit,
          child: const Text('保存'),
        ),
      ],
    );
  }
}
