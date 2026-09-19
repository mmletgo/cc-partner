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
  int _uploadedBytes = 0;
  int _uploadTotalBytes = 0;

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
    setState(() {
      _busy = '上传';
      _uploadedBytes = 0;
      _uploadTotalBytes = 0;
    });
    try {
      final plan = planUploadAfterPick(fileName: file.name, size: bytes.length);
      final init = await _api.uploadInit(
        filename: plan.fileName,
        size: plan.size,
        deviceId: target,
        clientOperationId: newClientOperationId(),
      );
      final id = init['id'] as String? ?? init['uploadId'] as String? ?? '';
      await _api.uploadFileInChunks(
        id: id,
        bytes: bytes,
        onProgress: (uploaded, total) {
          if (mounted) {
            setState(() {
              _uploadedBytes = uploaded;
              _uploadTotalBytes = total;
            });
          }
        },
      );
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

  Widget _taskTile(TransferTask task) {
    return ListTile(
      key: Key('transfer-task-${task.id}'),
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
    );
  }

  Widget _groupSection(String title, List<TransferTask> tasks) {
    if (tasks.isEmpty) {
      return const SizedBox.shrink();
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
          child: Text(title, style: Theme.of(context).textTheme.titleSmall),
        ),
        for (final task in tasks) _taskTile(task),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final groups = groupTransferTasks(_tasks);
    final uploadInProgress = _busy == '上传' && _uploadTotalBytes > 0;
    return Column(
      children: [
        if (_busy != null && !uploadInProgress) const LinearProgressIndicator(),
        if (uploadInProgress)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                LinearProgressIndicator(
                  key: const Key('transfer-upload-progress'),
                  value: _uploadedBytes / _uploadTotalBytes,
                ),
                const SizedBox(height: 4),
                Text(
                  '${formatTransferBytes(_uploadedBytes)} / ${formatTransferBytes(_uploadTotalBytes)}',
                  key: const Key('transfer-upload-progress-text'),
                ),
              ],
            ),
          ),
        if (_targets.isNotEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: DropdownButtonFormField<String>(
              key: const Key('transfer-target'),
              initialValue: _selectedTargetId,
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
                      if (_tasks.isEmpty)
                        const Padding(
                          padding: EdgeInsets.all(24),
                          child: Center(child: Text('暂无传输任务，选择文件后即可发送。')),
                        )
                      else ...[
                        _groupSection('进行中', groups.active),
                        _groupSection('需注意', groups.needsAttention),
                        _groupSection('已完成', groups.completed),
                      ],
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}

/// 把字节数格式化成人类可读的传输进度文本：1 MB 以下用 KB，其余用 MB（各保留一位小数）。
String formatTransferBytes(int bytes) {
  if (bytes < 1024 * 1024) {
    return '${(bytes / 1024).toStringAsFixed(1)} KB';
  }
  return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
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
