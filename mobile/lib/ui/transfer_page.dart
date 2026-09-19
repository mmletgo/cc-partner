import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../transfer/api.dart';
import '../transfer/client.dart';

class TransferPage extends StatefulWidget {
  const TransferPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<TransferPage> createState() => _TransferPageState();
}

class _TransferPageState extends State<TransferPage> {
  late final TransferApi _api;
  List<TransferTask> _tasks = [];
  String? _error;
  bool _loading = true;
  String? _busy;

  @override
  void initState() {
    super.initState();
    _api = TransferApi(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final tasks = await _api.listTasks();
      if (mounted) {
        setState(() {
          _tasks = tasks;
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

  Future<void> _pickAndSend() async {
    final picked = await FilePicker.platform.pickFiles(withData: true);
    if (picked == null || picked.files.isEmpty) {
      return;
    }
    final file = picked.files.first;
    final bytes = file.bytes;
    if (bytes == null) {
      return;
    }
    setState(() => _busy = '上传');
    try {
      final devices = await _api.listDevices();
      final target = devices.isNotEmpty
          ? devices.first['id'] as String? ?? devices.first['deviceId'] as String? ?? ''
          : '';
      final plan = planUploadAfterPick(fileName: file.name, size: bytes.length);
      final init = await _api.uploadInit(
        filename: plan.fileName,
        size: plan.size,
        deviceId: target,
        clientOperationId: newClientOperationId(),
      );
      final id = init['id'] as String? ?? init['uploadId'] as String? ?? '';
      const chunk = 256 * 1024;
      for (var offset = 0; offset < bytes.length; offset += chunk) {
        final end = (offset + chunk > bytes.length) ? bytes.length : offset + chunk;
        await _api.uploadChunk(id, offset, bytes.sublist(offset, end));
      }
      await _api.uploadComplete(id);
      await _reload();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('发送失败: $error')));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        if (_busy != null) const LinearProgressIndicator(),
        Padding(
          padding: const EdgeInsets.all(8),
          child: FilledButton.icon(
            onPressed: _busy == null ? _pickAndSend : null,
            icon: const Icon(Icons.file_upload),
            label: const Text('选择文件并立即发送'),
          ),
        ),
        if (_error != null) Text(_error!),
        Expanded(
          child: _loading
              ? const Center(child: CircularProgressIndicator())
              : RefreshIndicator(
                  onRefresh: _reload,
                  child: ListView(
                    children: [
                      for (final task in _tasks)
                        ListTile(
                          title: Text(task.fileName ?? task.id),
                          subtitle: Text('${task.direction} · ${task.status}${canDownload(task) ? ' · 可下载' : ''}'),
                        ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}
