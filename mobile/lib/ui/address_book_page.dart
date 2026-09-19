import 'dart:async';

import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../address_book/models.dart';
import '../core/health_probe.dart';
import '../core/lan_http.dart';
import '../core/server_url.dart';
import '../settings/risk_copy.dart';
import 'scan_qr_page.dart';
import 'workbench_home.dart';

/// Address book screen: save and switch LAN PC servers.
class AddressBookPage extends StatefulWidget {
  const AddressBookPage({
    super.key,
    required this.book,
    required this.http,
    this.probe,
  });

  final AddressBook book;
  final LanHttpClient http;
  final HealthProbe? probe;

  @override
  State<AddressBookPage> createState() => _AddressBookPageState();
}

class _AddressBookPageState extends State<AddressBookPage>
    with WidgetsBindingObserver {
  String? _busyId;
  String? _error;
  bool _refreshing = false;
  bool _refreshAgain = false;

  AddressBook get _book => widget.book;

  HealthProbe get _probe =>
      widget.probe ?? ((baseUrl) => probeLanHealth(widget.http, baseUrl));

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_refreshHealth());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(_refreshHealth());
    }
  }

  /// Re-probe saved PCs on open, resume, and pull-to-refresh.
  ///
  /// Business Logic: 本地网络授权或电脑上线发生在首次探测之后，必须再探一次才能从离线恢复。
  /// Code Logic: 进行中的刷新结束后再跟一次，避免授权框期间的请求被丢掉。
  Future<void> _refreshHealth() async {
    if (_book.servers.isEmpty) {
      return;
    }
    if (_refreshing) {
      _refreshAgain = true;
      return;
    }
    _refreshing = true;
    try {
      do {
        _refreshAgain = false;
        await _book.refreshHealth(_probe);
      } while (_refreshAgain);
    } finally {
      _refreshing = false;
      if (mounted) {
        setState(() {});
      }
    }
  }

  Future<void> _scanQr() async {
    final payload = await Navigator.of(
      context,
    ).push<String>(MaterialPageRoute(builder: (_) => const ScanQrPage()));
    if (payload == null || !mounted) {
      return;
    }
    await _saveFromInput(payload, name: '', forceIfUnreachable: true);
  }

  Future<void> _addServer() async {
    final result = await showDialog<_AddResult>(
      context: context,
      builder: (context) => const _AddServerDialog(),
    );
    if (result == null || !mounted) {
      return;
    }
    await _saveFromInput(
      result.input,
      name: result.name,
      forceIfUnreachable: result.force,
    );
  }

  Future<void> _saveFromInput(
    String input, {
    required String name,
    required bool forceIfUnreachable,
  }) async {
    setState(() {
      _error = null;
      _busyId = 'add';
    });
    try {
      final record = await _book.addFromInput(
        input,
        name: name,
        forceIfUnreachable: forceIfUnreachable,
        probe: _probe,
      );
      if (mounted) {
        setState(() => _busyId = null);
        final reason = record.lastProbeError;
        final status = record.isOnline
            ? '已连接'
            : '已保存，当前离线${reason == null ? '' : '（$reason）'}';
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('$status ${record.baseUrl}')));
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
      appBar: AppBar(
        title: const Text('cc-partner 地址簿'),
        actions: [
          IconButton(
            key: const Key('scan-qr'),
            tooltip: '扫描电脑二维码',
            onPressed: _busyId == null ? _scanQr : null,
            icon: const Icon(Icons.qr_code_scanner),
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        key: const Key('add-server'),
        onPressed: _busyId == null ? _addServer : null,
        tooltip: '手动添加 PC',
        icon: const Icon(Icons.add),
        label: const Text('手动添加'),
      ),
      body: RefreshIndicator(
        key: const Key('refresh-health'),
        onRefresh: _refreshHealth,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
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
              Text(
                _error!,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ],
            if (_busyId != null) ...[
              const SizedBox(height: 12),
              const LinearProgressIndicator(),
            ],
            const SizedBox(height: 16),
            if (_book.servers.isEmpty)
              const Text('还没有 PC。点右上角扫一扫桌面上的二维码，或点右下角手动填写。')
            else
              ..._book.servers.map(_serverTile),
          ],
        ),
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
          '${server.lastProbeError == null ? '' : ' · ${server.lastProbeError}'}'
          '${active ? ' · 当前' : ''}',
        ),
        isThreeLine: true,
        onTap: () async {
          await _switchTo(server);
          if (!server.isOnline) {
            if (mounted) {
              ScaffoldMessenger.of(
                context,
              ).showSnackBar(const SnackBar(content: Text('电脑离线，不能进入工作台')));
            }
            return;
          }
          if (!mounted) {
            return;
          }
          await Navigator.of(context).push(
            MaterialPageRoute(
              builder: (_) => WorkbenchHome(book: _book, http: widget.http),
            ),
          );
        },
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
    Navigator.of(
      context,
    ).pop(_AddResult(input: input, name: _name.text.trim(), force: _force));
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
