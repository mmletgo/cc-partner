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
