import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../transfer/api.dart';
import '../transfer/client.dart';

class TransferPage extends StatefulWidget {
  const TransferPage({
    super.key,
    required this.book,
    required this.http,
    this.api,
    this.saveSink,
  });

  final AddressBook book;
  final LanHttpClient http;
  final TransferApi? api;
  final TransferSaveSink? saveSink;

  @override
  State<TransferPage> createState() => _TransferPageState();
}

class _TransferPageState extends State<TransferPage> {
  late final TransferApi _api;
  List<TransferTask> _tasks = [];
  List<Map<String, dynamic>> _targets = [];
  String? _selectedTargetId;
  String? _error;
  bool _loading = true;
  String? _busy;

  @override
  void initState() {
    super.initState();
    _api = widget.api ?? TransferApi(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final tasks = await _api.listTasks();
      final targets = rankTransferTargets(await _api.listDevices());
      if (mounted) {
        setState(() {
          _tasks = tasks;
          _targets = targets;
          _selectedTargetId = pickTransferTargetId(targets, selectedId: _selectedTargetId);
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
    final target = pickTransferTargetId(_targets, selectedId: _selectedTargetId) ?? '';
    setState(() => _busy = '上传');
    try {
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

  Future<void> _download(TransferTask task) async {
    setState(() => _busy = '下载');
    try {
      await downloadAndSaveTask(
        api: _api,
        taskId: task.id,
        fileName: task.fileName ?? task.id,
        save: widget.saveSink ?? saveTransferBytesToPhone,
      );
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已保存到本机')));
      }
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('下载失败: $error')));
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
        if (_targets.isNotEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: DropdownButtonFormField<String>(
              key: const Key('transfer-target'),
              value: _selectedTargetId,
              decoration: const InputDecoration(labelText: '发送到'),
              items: [
                for (final device in _targets)
                  DropdownMenuItem(
                    value: transferDeviceId(device),
                    child: Text(transferDeviceLabel(device)),
                  ),
              ],
              onChanged: (value) => setState(() => _selectedTargetId = value),
            ),
          ),
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
                          trailing: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              if (canDownload(task))
                                IconButton(
                                  tooltip: '下载',
                                  onPressed: () => _download(task),
                                  icon: const Icon(Icons.download),
                                ),
                              if (task.status != 'completed')
                                IconButton(
                                  tooltip: '取消',
                                  onPressed: () async {
                                    await _api.cancel(task.id);
                                    await _reload();
                                  },
                                  icon: const Icon(Icons.cancel_outlined),
                                ),
                            ],
                          ),
                        ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}

Future<void> saveTransferBytesToPhone({
  required String fileName,
  required List<int> bytes,
}) async {
  await FilePicker.platform.saveFile(
    fileName: fileName,
    bytes: Uint8List.fromList(bytes),
  );
}
