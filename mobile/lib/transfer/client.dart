/// Host-relay transfer: phone is not a P2P node.
class TransferTask {
  TransferTask({
    required this.id,
    required this.direction,
    required this.status,
    this.fileName,
    this.peer,
    this.peerDeviceName,
    this.fileSize,
    this.progress = 0,
    this.transferredBytes,
    this.failureRetryable,
    this.logicalTransferId,
  });

  final String id;
  final String direction;
  final String status;
  final String? fileName;

  /// 对端设备 id（兼容旧字段 `peer` / 新字段 `peerDeviceId`）。
  final String? peer;
  final String? peerDeviceName;
  final int? fileSize;

  /// 0..1 进度比例；后端缺省时按 0 处理（续传判定依据之一）。
  final double progress;

  /// 已确认传输字节数；>0 时 failed 任务优先显示「继续传输」。
  final int? transferredBytes;

  /// 结构化 failure.retryable；缺省视为可重试（宽容解析）。
  final bool? failureRetryable;

  /// 逻辑传输 id（跨 retry 稳定）；缺省回落 task.id。
  final String? logicalTransferId;

  factory TransferTask.fromJson(Map<String, dynamic> json) {
    assertNoHostPaths(json);
    final failure = json['failure'];
    final failureMap = failure is Map ? Map<String, dynamic>.from(failure) : const <String, dynamic>{};
    return TransferTask(
      id: json['id'] as String? ?? json['taskId'] as String? ?? '',
      direction: json['direction'] as String? ?? '',
      status: json['status'] as String? ?? '',
      fileName: json['fileName'] as String? ?? json['name'] as String?,
      peer: json['peerDeviceId'] as String? ?? json['peer'] as String?,
      peerDeviceName: json['peerDeviceName'] as String?,
      fileSize: (json['fileSize'] as num?)?.toInt(),
      progress: (json['progress'] as num?)?.toDouble() ?? 0,
      transferredBytes: (json['transferredBytes'] as num?)?.toInt(),
      failureRetryable: failureMap['retryable'] as bool?,
      logicalTransferId: json['logicalTransferId'] as String?,
    );
  }
}

const _hostPathKeys = {
  'hostPath',
  'sourcePath',
  'absolutePath',
  'filePath',
  'localPath',
};

/// Task JSON from the PC must not include host filesystem paths.
void assertNoHostPaths(Map<String, dynamic> json) {
  for (final key in _hostPathKeys) {
    if (json.containsKey(key) && json[key] != null && '${json[key]}'.isNotEmpty) {
      throw FormatException('transfer task JSON must not include host path key $key');
    }
  }
}

bool canDownload(TransferTask task) {
  if (task.status != 'completed') {
    return false;
  }
  if (task.direction.toLowerCase() == 'receive') {
    return true;
  }
  return task.peer == 'cc-partner-mobile-inbox';
}

/// 任务分组 key，与 /mobile 的 groupTransferTasks 对齐：
/// pending/transferring → active；failed/cancelled → needsAttention；其余 completed。
String classifyTransferGroup(TransferTask task) {
  if (task.status == 'pending' || task.status == 'transferring') {
    return 'active';
  }
  if (task.status == 'failed' || task.status == 'cancelled') {
    return 'needsAttention';
  }
  return 'completed';
}

/// 一次任务列表刷新的三组视图；空组由 UI 决定是否隐藏。
class TransferTaskGroups {
  const TransferTaskGroups({
    required this.active,
    required this.needsAttention,
    required this.completed,
  });

  final List<TransferTask> active;
  final List<TransferTask> needsAttention;
  final List<TransferTask> completed;
}

/// 按进行中/需注意/已完成把任务列表分成三组，保持原有顺序。
TransferTaskGroups groupTransferTasks(List<TransferTask> tasks) {
  final active = <TransferTask>[];
  final needsAttention = <TransferTask>[];
  final completed = <TransferTask>[];
  for (final task in tasks) {
    final group = classifyTransferGroup(task);
    if (group == 'active') {
      active.add(task);
    } else if (group == 'needsAttention') {
      needsAttention.add(task);
    } else {
      completed.add(task);
    }
  }
  return TransferTaskGroups(
    active: active,
    needsAttention: needsAttention,
    completed: completed,
  );
}

/// Upload starts immediately after the system file picker returns a file.
class TransferUploadPlan {
  const TransferUploadPlan({
    required this.fileName,
    required this.size,
    required this.startImmediately,
  });

  final String fileName;
  final int size;
  final bool startImmediately;
}

TransferUploadPlan planUploadAfterPick({
  required String fileName,
  required int size,
}) {
  return TransferUploadPlan(
    fileName: fileName,
    size: size,
    startImmediately: true,
  );
}

/// Host PC (`isSelf`) is listed first, then the current peer, matching `/mobile`.
List<Map<String, dynamic>> rankTransferTargets(List<Map<String, dynamic>> devices) {
  final copy = List<Map<String, dynamic>>.from(devices);
  copy.sort((a, b) {
    final aSelf = a['isSelf'] == true;
    final bSelf = b['isSelf'] == true;
    if (aSelf == bSelf) {
      return 0;
    }
    return aSelf ? -1 : 1;
  });
  return copy;
}

String transferDeviceId(Map<String, dynamic> device) =>
    device['id'] as String? ?? device['deviceId'] as String? ?? '';

String transferDeviceLabel(Map<String, dynamic> device) {
  final name = device['name'] as String? ?? device['deviceName'] as String?;
  final id = transferDeviceId(device);
  final label = (name != null && name.isNotEmpty) ? name : id;
  return device['isSelf'] == true ? '$label · 主机' : label;
}

/// Host is the default; a still-listed user choice wins.
String? pickTransferTargetId(
  List<Map<String, dynamic>> ranked, {
  String? selectedId,
}) {
  if (selectedId != null && selectedId.isNotEmpty) {
    for (final device in ranked) {
      if (transferDeviceId(device) == selectedId) {
        return selectedId;
      }
    }
  }
  if (ranked.isEmpty) {
    return null;
  }
  return transferDeviceId(ranked.first);
}

/// 对端声明断点续传的能力 token，与后端 `transfer.resume.v1` 对齐。
const transferResumeCapabilityV1 = 'transfer.resume.v1';

/// Business Logic: 设备下拉与续传判定需要统一的能力口径，避免各处自行猜字段。
/// Code Logic: capabilities 是字符串数组且包含 transfer.resume.v1 才算支持。
bool deviceSupportsTransferResume(Map<String, dynamic> device) {
  final caps = device['capabilities'];
  return caps is List && caps.contains(transferResumeCapabilityV1);
}

/// Business Logic: 旧 peer 无 resume.v1 时必须回退「重新传输」，本机目标视为支持，
/// 防止显示点了必然失败的假续传。
/// Code Logic: 找到 task.peer 对应设备；isSelf 直接 true；否则查 capabilities；
/// peer 缺失或不在列表里 fail-closed 返回 false。
bool peerSupportsTransferResume(
  TransferTask task,
  List<Map<String, dynamic>> devices,
) {
  final peerId = task.peer?.trim() ?? '';
  if (peerId.isEmpty) {
    return false;
  }
  for (final device in devices) {
    if (transferDeviceId(device) == peerId) {
      if (device['isSelf'] == true) {
        return true;
      }
      return deviceSupportsTransferResume(device);
    }
  }
  return false;
}

/// Business Logic: 失败且仍有已确认字节/进度时优先续传，而不是全量重传。
/// Code Logic: 仅 Send + failed + 未标不可重试，且 transferredBytes>0 或 0<progress<1，
/// 且 peerSupportsResume（缺省 false）。
bool isTransferResumable(TransferTask task, bool peerSupportsResume) {
  if (!peerSupportsResume) {
    return false;
  }
  if (task.direction.toLowerCase() != 'send') {
    return false;
  }
  if (task.status != 'failed') {
    return false;
  }
  if (task.failureRetryable == false) {
    return false;
  }
  if ((task.transferredBytes ?? 0) > 0) {
    return true;
  }
  return task.progress > 0 && task.progress < 1;
}

/// Business Logic: 无续传元数据、peer 无 resume 能力或已取消的任务允许显式重新传输。
/// Code Logic: Send 方向 cancelled → true；failed 且可重试且非 resumable → true。
bool isTransferRetryable(TransferTask task, bool peerSupportsResume) {
  if (task.direction.toLowerCase() != 'send') {
    return false;
  }
  if (task.status == 'cancelled') {
    return true;
  }
  if (task.status != 'failed') {
    return false;
  }
  if (task.failureRetryable == false) {
    return false;
  }
  return !isTransferResumable(task, peerSupportsResume);
}

/// Business Logic: 恢复动作互斥与列表扫描共用同一逻辑身份解析。
/// Code Logic: 非空 logicalTransferId 优先，否则回落 task.id。
String resolveLogicalTransferId(TransferTask task) {
  final logical = task.logicalTransferId?.trim() ?? '';
  return logical.isNotEmpty ? logical : task.id;
}

/// Business Logic: 判定某 attempt 是否仍占用 logical transfer 的发送槽。
/// Code Logic: status 为 pending 或 transferring 即活跃。
bool isTransferAttemptActive(TransferTask task) {
  return task.status == 'pending' || task.status == 'transferring';
}

/// Business Logic: 同一 logical transfer 下已有 attempt 在传输或对账中时，
/// 旧 failed 行不得再点 resume/retry 另 mint clientOperationId 并发发送。
/// Code Logic: 扫描 tasks，同 logical 且 reconciling 或活跃 → true。
bool isTransferRecoveryLocked(
  TransferTask task,
  List<TransferTask> tasks,
  Set<String> reconcilingIds,
) {
  final logicalId = resolveLogicalTransferId(task);
  for (final candidate in tasks) {
    if (resolveLogicalTransferId(candidate) != logicalId) {
      continue;
    }
    if (reconcilingIds.contains(candidate.id)) {
      return true;
    }
    if (isTransferAttemptActive(candidate)) {
      return true;
    }
  }
  return false;
}
