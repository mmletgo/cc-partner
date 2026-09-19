/// Host-relay transfer: phone is not a P2P node.
class TransferTask {
  TransferTask({
    required this.id,
    required this.direction,
    required this.status,
    this.fileName,
    this.peer,
  });

  final String id;
  final String direction;
  final String status;
  final String? fileName;
  final String? peer;

  factory TransferTask.fromJson(Map<String, dynamic> json) {
    assertNoHostPaths(json);
    return TransferTask(
      id: json['id'] as String? ?? json['taskId'] as String? ?? '',
      direction: json['direction'] as String? ?? '',
      status: json['status'] as String? ?? '',
      fileName: json['fileName'] as String? ?? json['name'] as String?,
      peer: json['peer'] as String?,
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
