import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../transfer/api.dart';
import '../transfer/client.dart';
import '../transfer/polling.dart';

/// 同一「设备 + 文件」上传意图的幂等键（不含任何路径），对齐 web buildMobileTransferSendIntentKey。
String _transferSendIntentKey(String deviceId, String fileName, int size) =>
    '$deviceId\x00$fileName\x00$size';

/// 恢复动作的在途幂等记录：同 kind 重试复用同一 clientOperationId。
class _PendingRecovery {
  const _PendingRecovery({required this.clientOperationId, required this.resume});

  final String clientOperationId;
  final bool resume;
}

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

  /// 设备列表首载失败错误文本：仅首载（无任何设备数据）失败时上屏，
  /// 对齐 web devicesState=error 区（错误 + 重试按钮）。
  String? _devicesError;

  /// 任务列表首载失败错误文本：仅首载（无任何任务数据）失败时上屏，
  /// 对齐 web tasksState=error 区（错误 + 重试按钮）。
  String? _tasksError;
  bool _loading = true;
  String? _busy;
  int _uploadedBytes = 0;
  int _uploadTotalBytes = 0;

  /// 发送区行内错误（role=alert 语义），与任务行级错误分开。
  String? _sendError;

  /// 任务行级动作错误（取消/重试/续传），key 为 taskId。
  final Map<String, String> _taskActionErrors = {};

  /// 对账中的任务（get-operation 有界轮询期间按钮禁用并显示「正在确认结果」）。
  final Set<String> _reconcilingIds = {};

  /// 取消防双击。
  final Set<String> _cancellingIds = {};

  /// 恢复动作防重入。
  final Set<String> _recoveryBusy = {};

  /// 恢复动作的幂等键缓存：uncertain 后复用同一 clientOperationId 对账。
  final Map<String, _PendingRecovery> _pendingRecoveries = {};

  /// 上传意图幂等键与对应 clientOperationId：uncertain 后同设备同文件复用同一 id 对账。
  String? _pendingSendIntentKey;
  String? _pendingSendOperationId;

  /// 可见时轮询：任务 3s、设备 5s（对齐 web useVisibilityPolling）。
  late final VisibilityPoller _tasksPoller;
  late final VisibilityPoller _devicesPoller;

  @override
  void initState() {
    super.initState();
    _api = widget.api ?? TransferApi(widget.http, widget.book.active!.baseUrl);
    _tasksPoller = VisibilityPoller(interval: const Duration(seconds: 3), task: _pollTasks);
    _devicesPoller = VisibilityPoller(interval: const Duration(seconds: 5), task: _pollDevices);
    _reload(showLoading: true);
    // initState 已首拉一次，轮询不再立即重复执行；回前台由生命周期立即补拉。
    _tasksPoller.start(runImmediately: false);
    _devicesPoller.start(runImmediately: false);
  }

  @override
  void dispose() {
    _tasksPoller.dispose();
    _devicesPoller.dispose();
    super.dispose();
  }

  /// Business Logic: 任务列表是进度/恢复动作的权威源，可见时每 3s 静默刷新。
  /// Code Logic: 轮询路径不显示 loading；失败保留旧列表，仅首载失败才上屏错误。
  Future<void> _pollTasks() => _loadTasks(showLoading: false);

  /// Business Logic: 设备列表驱动目标下拉与续传能力判定，可见时每 5s 静默刷新。
  /// Code Logic: 同上；失败不得清空已选目标。
  Future<void> _pollDevices() => _loadDevices(showLoading: false);

  Future<void> _reload({required bool showLoading}) async {
    await Future.wait([
      _loadTasks(showLoading: showLoading),
      _loadDevices(showLoading: showLoading),
    ]);
  }

  Future<void> _loadTasks({required bool showLoading}) async {
    if (showLoading && mounted) {
      setState(() {
        _loading = true;
        _tasksError = null;
      });
    }
    try {
      final tasks = await _api.listTasks();
      if (mounted) {
        setState(() {
          _tasks = tasks;
          _tasksError = null;
        });
      }
    } catch (error) {
      // 刷新失败保留上一份列表（对齐 web retainListOnRefreshFailure）；
      // 只有首载（尚无数据）才上屏错误 + 重试按钮。
      if (mounted && _tasks.isEmpty) {
        setState(() {
          _tasksError = '任务列表加载失败：$error';
          _loading = false;
        });
      }
    } finally {
      if (mounted && showLoading) {
        setState(() => _loading = false);
      }
    }
  }

  Future<void> _loadDevices({required bool showLoading}) async {
    if (showLoading && mounted) {
      setState(() {
        _loading = true;
        _devicesError = null;
      });
    }
    try {
      final targets = rankTransferTargets(await _api.listDevices());
      if (mounted) {
        setState(() {
          _targets = targets;
          _selectedTargetId = pickTransferTargetId(targets, selectedId: _selectedTargetId);
          _devicesError = null;
        });
      }
    } catch (error) {
      if (mounted && _targets.isEmpty) {
        setState(() {
          _devicesError = '设备列表加载失败：$error';
          _loading = false;
        });
      }
    } finally {
      if (mounted && showLoading) {
        setState(() => _loading = false);
      }
    }
  }

  void _clearPendingSendIntent() {
    _pendingSendIntentKey = null;
    _pendingSendOperationId = null;
  }

  /// Business Logic: 选完文件立即 init → chunk → complete；timeout/network 结果未知时
  /// 只能用同一 clientOperationId 对账（约 12 次 × 1.5s），禁止盲重发。
  /// Code Logic: sending 期间 busy 门闩；成功重载任务；uncertain 走 _reconcileSend。
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
      _sendError = null;
      _uploadedBytes = 0;
      _uploadTotalBytes = 0;
    });
    final intentKey = _transferSendIntentKey(target, file.name, bytes.length);
    final reused = _pendingSendIntentKey == intentKey ? _pendingSendOperationId : null;
    final String clientOperationId = reused ?? newClientOperationId();
    _pendingSendIntentKey = intentKey;
    _pendingSendOperationId = clientOperationId;
    try {
      final plan = planUploadAfterPick(fileName: file.name, size: bytes.length);
      final init = await _api.uploadInit(
        filename: plan.fileName,
        size: plan.size,
        deviceId: target,
        clientOperationId: clientOperationId,
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
      _clearPendingSendIntent();
      await _loadTasks(showLoading: false);
    } catch (error) {
      if (!mounted) {
        return;
      }
      if (isTransferOutcomeUncertain(error)) {
        // 结果未知：对账而不是把错误当确定性失败。
        await _reconcileSend(clientOperationId);
        return;
      }
      _clearPendingSendIntent();
      setState(() => _sendError = '发送失败：$error');
    } finally {
      if (mounted) {
        setState(() => _busy = null);
      }
    }
  }

  /// Business Logic: 上传 uncertain 后必须有界轮询 get-operation 确认最终状态，
  /// 确认成功按成功处理，确认失败/超时（仍 pending）才报错。
  /// Code Logic: pending → 保留幂等键（重试复用同一 id）；failed → 行内错误；
  /// succeeded/notFound → 清幂等键并静默重载任务列表。
  Future<void> _reconcileSend(String clientOperationId) async {
    final TransferOperationStatus? status;
    try {
      status = await reconcileTransferOperation(api: _api, clientOperationId: clientOperationId);
    } catch (reconcileError) {
      if (mounted) {
        setState(() => _sendError = '发送失败：$reconcileError');
      }
      return;
    }
    if (!mounted) {
      return;
    }
    if (status == null || status.isPending) {
      setState(() => _sendError = '发送失败：操作仍在处理中，请稍后重试');
      return;
    }
    final result = status;
    _clearPendingSendIntent();
    await _loadTasks(showLoading: false);
    if (!mounted) {
      return;
    }
    setState(() {
      _sendError = result.status == 'failed' ? '发送失败：${result.code ?? result.status}' : null;
    });
  }

  /// Business Logic: 下载保持 saveFile 落盘；失败 SnackBar 提示。
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

  /// Business Logic: 进行中任务只能取消；双击不得二次 cancel；失败上屏到行内。
  Future<void> _cancel(TransferTask task) async {
    if (_cancellingIds.contains(task.id)) {
      return;
    }
    setState(() {
      _cancellingIds.add(task.id);
      _taskActionErrors.remove(task.id);
    });
    try {
      await _api.cancel(task.id);
      await _loadTasks(showLoading: false);
    } catch (error) {
      if (mounted) {
        setState(() => _taskActionErrors[task.id] = '取消失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _cancellingIds.remove(task.id));
      }
    }
  }

  /// Business Logic: failed/cancelled 任务可恢复（重试/续传）；同一 logical transfer
  /// 已有活跃 attempt 或对账中时禁止再发；uncertain 用同一幂等键对账。
  /// Code Logic: busy/逻辑锁门闩；成功清幂等键 + 静默重载 + SnackBar；
  /// uncertain → _reconcileRecovery；确定性失败 → 行内错误。
  Future<void> _recover(TransferTask task, {required bool resume}) async {
    final taskId = task.id;
    if (_recoveryBusy.contains(taskId) || _reconcilingIds.contains(taskId)) {
      return;
    }
    if (isTransferRecoveryLocked(task, _tasks, _reconcilingIds)) {
      return;
    }
    setState(() {
      _recoveryBusy.add(taskId);
      _taskActionErrors.remove(taskId);
    });
    final existing = _pendingRecoveries[taskId];
    final clientOperationId =
        existing != null && existing.resume == resume
            ? existing.clientOperationId
            : newClientOperationId();
    _pendingRecoveries[taskId] = _PendingRecovery(
      clientOperationId: clientOperationId,
      resume: resume,
    );
    try {
      if (resume) {
        await _api.resume(taskId, clientOperationId);
      } else {
        await _api.retry(taskId, clientOperationId);
      }
      if (!mounted) {
        return;
      }
      setState(() {
        _pendingRecoveries.remove(taskId);
        _reconcilingIds.remove(taskId);
      });
      await _loadTasks(showLoading: false);
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(resume ? '已继续传输' : '已重新传输')),
        );
      }
    } catch (error) {
      if (!mounted) {
        return;
      }
      if (isTransferOutcomeUncertain(error)) {
        await _reconcileRecovery(taskId, clientOperationId);
        return;
      }
      setState(() {
        _pendingRecoveries.remove(taskId);
        _reconcilingIds.remove(taskId);
        _taskActionErrors[taskId] = resume ? '继续传输失败：$error' : '重新传输失败：$error';
      });
    } finally {
      if (mounted) {
        setState(() => _recoveryBusy.remove(taskId));
      } else {
        _recoveryBusy.remove(taskId);
      }
    }
  }

  /// Business Logic: 恢复动作 uncertain 后必须对账到终态，期间行内显示「正在确认结果」。
  /// Code Logic: pending → 保留幂等键提示稍后重试；failed → 行内错误；
  /// succeeded/notFound → 清幂等键 + 静默重载 + 成功 SnackBar。
  Future<void> _reconcileRecovery(String taskId, String clientOperationId) async {
    setState(() => _reconcilingIds.add(taskId));
    final TransferOperationStatus? status;
    try {
      status = await reconcileTransferOperation(api: _api, clientOperationId: clientOperationId);
    } catch (reconcileError) {
      if (mounted) {
        setState(() {
          _reconcilingIds.remove(taskId);
          _taskActionErrors[taskId] = '恢复失败：$reconcileError';
        });
      }
      return;
    }
    if (!mounted) {
      return;
    }
    if (status == null || status.isPending) {
      setState(() {
        _reconcilingIds.remove(taskId);
        _taskActionErrors[taskId] = '操作仍在处理中，请稍后重试';
      });
      return;
    }
    final failed = status.status == 'failed';
    final result = status;
    setState(() {
      _pendingRecoveries.remove(taskId);
      _reconcilingIds.remove(taskId);
      if (failed) {
        _taskActionErrors[taskId] = '恢复失败：${result.code ?? result.status}';
      }
    });
    await _loadTasks(showLoading: false);
    if (!failed && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('传输已恢复')));
    }
  }

  Widget _taskTile(TransferTask task) {
    final theme = Theme.of(context);
    final reconciling = _reconcilingIds.contains(task.id);
    final recoveryLocked = isTransferRecoveryLocked(task, _tasks, _reconcilingIds);
    final supportsResume = peerSupportsTransferResume(task, _targets);
    final canResume = !reconciling && !recoveryLocked && isTransferResumable(task, supportsResume);
    final canRetry = !reconciling && !recoveryLocked && isTransferRetryable(task, supportsResume);
    final canCancel = task.status == 'pending' || task.status == 'transferring';
    final actionError = _taskActionErrors[task.id];
    final subtitle = [
      '${task.direction} · ${task.status}',
      if (reconciling) '正在确认结果',
      if (canDownload(task)) '可下载',
    ].join(' · ');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ListTile(
          key: Key('transfer-task-${task.id}'),
          title: Text(task.fileName ?? task.id),
          subtitle: Text(subtitle),
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (canDownload(task))
                IconButton(
                  tooltip: '下载',
                  onPressed: () => _download(task),
                  icon: const Icon(Icons.download),
                ),
              if (canResume)
                IconButton(
                  key: Key('transfer-resume-${task.id}'),
                  tooltip: '继续传输',
                  onPressed: () => _recover(task, resume: true),
                  icon: const Icon(Icons.play_arrow),
                ),
              if (canRetry)
                IconButton(
                  key: Key('transfer-retry-${task.id}'),
                  tooltip: '重新传输',
                  onPressed: () => _recover(task, resume: false),
                  icon: const Icon(Icons.replay),
                ),
              if (canCancel)
                IconButton(
                  tooltip: '取消',
                  onPressed: _cancellingIds.contains(task.id) ? null : () => _cancel(task),
                  icon: const Icon(Icons.cancel_outlined),
                ),
            ],
          ),
        ),
        // 进行中任务行内进度条与字节文本（对齐 web 行内进度条）；completed/failed 不显示。
        if (canCancel) ...[
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
            child: LinearProgressIndicator(
              key: Key('transfer-progress-${task.id}'),
              value: task.progress > 0 ? task.progress.clamp(0.0, 1.0).toDouble() : null,
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Text(
              transferTaskProgressText(task),
              key: Key('transfer-progress-text-${task.id}'),
              style: theme.textTheme.bodySmall,
            ),
          ),
        ],
        if (actionError != null)
          Padding(
            key: Key('transfer-error-${task.id}'),
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Semantics(
              liveRegion: true,
              child: Text(
                actionError,
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.error),
              ),
            ),
          ),
      ],
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

  /// Business Logic: 任务/设备首载失败且无任何数据时，只有下拉刷新一条恢复路径
  /// 太隐蔽；对齐 web 失败区的显式「重试」按钮（common:action.retry）。
  /// Code Logic: 一行 = 错误文本（liveRegion 语义、error 色）+ TextButton 重试，
  /// 回调由调用方注入（设备区 → _loadDevices，任务区 → _loadTasks）。
  Widget _firstLoadErrorRow({
    required String text,
    required Key textKey,
    required Key retryKey,
    required VoidCallback onRetry,
  }) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
      child: Semantics(
        liveRegion: true,
        child: Row(
          children: [
            Expanded(
              child: Text(
                text,
                key: textKey,
                style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.error),
              ),
            ),
            const SizedBox(width: 8),
            TextButton.icon(
              key: retryKey,
              onPressed: onRetry,
              icon: const Icon(Icons.refresh, size: 18),
              label: const Text('重试'),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
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
        if (_sendError != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Semantics(
              liveRegion: true,
              child: Text(
                _sendError!,
                key: const Key('transfer-send-error'),
                style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.error),
              ),
            ),
          ),
        if (_devicesError != null)
          _firstLoadErrorRow(
            text: _devicesError!,
            textKey: const Key('transfer-devices-error'),
            retryKey: const Key('transfer-devices-retry'),
            onRetry: () => _loadDevices(showLoading: false),
          ),
        Expanded(
          child: _loading
              ? const Center(child: CircularProgressIndicator())
              : RefreshIndicator(
                  onRefresh: () => _reload(showLoading: false),
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
                      if (_tasksError != null)
                        _firstLoadErrorRow(
                          text: _tasksError!,
                          textKey: const Key('transfer-tasks-error'),
                          retryKey: const Key('transfer-tasks-retry'),
                          onRetry: () => _loadTasks(showLoading: false),
                        ),
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

/// Business Logic: 进行中任务行要把「已传多少 / 总共多少 / 百分比」直接呈现，
/// 对齐 web 任务行内进度文本；completed/failed 行不展示。
/// Code Logic: 已传字节优先 `TransferTask.transferredBytes`，缺失时按
/// `progress × fileSize` 估算；总量未知（fileSize 缺失）只显示「已传 X」。
String transferTaskProgressText(TransferTask task) {
  final total = task.fileSize ?? 0;
  final transferred = task.transferredBytes ??
      (total > 0 ? (task.progress.clamp(0, 1) * total).round() : 0);
  if (total <= 0) {
    return '已传 ${formatTransferBytes(transferred)}';
  }
  final percent = ((transferred / total) * 100).clamp(0, 100).toStringAsFixed(0);
  return '已传 ${formatTransferBytes(transferred)} / ${formatTransferBytes(total)}（$percent%）';
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
