/// Health of a saved PC after the last probe.
enum ServerHealth { online, unreachable, unsupported }

/// Last workbench location on one PC (restored after switch-back).
class LastLocation {
  const LastLocation({
    this.projectId,
    this.panel,
    this.worktreeId,
    this.sessionId,
  });

  final String? projectId;
  final String? panel;
  final String? worktreeId;
  final String? sessionId;

  Map<String, dynamic> toJson() => {
        'projectId': projectId,
        'panel': panel,
        'worktreeId': worktreeId,
        'sessionId': sessionId,
      };

  factory LastLocation.fromJson(Map<String, dynamic> json) => LastLocation(
        projectId: json['projectId'] as String?,
        panel: json['panel'] as String?,
        worktreeId: json['worktreeId'] as String?,
        sessionId: json['sessionId'] as String?,
      );

  @override
  bool operator ==(Object other) =>
      other is LastLocation &&
      other.projectId == projectId &&
      other.panel == panel &&
      other.worktreeId == worktreeId &&
      other.sessionId == sessionId;

  @override
  int get hashCode => Object.hash(projectId, panel, worktreeId, sessionId);
}

/// Push-registration intent for one saved PC. Switching the workbench must not clear this.
class PushIntent {
  const PushIntent({this.registeredToken, this.lastError});

  final String? registeredToken;
  final String? lastError;

  Map<String, dynamic> toJson() => {
        'registeredToken': registeredToken,
        'lastError': lastError,
      };

  factory PushIntent.fromJson(Map<String, dynamic> json) => PushIntent(
        registeredToken: json['registeredToken'] as String?,
        lastError: json['lastError'] as String?,
      );
}

/// One PC in the phone address book.
class ServerRecord {
  ServerRecord({
    required this.id,
    required this.host,
    required this.port,
    required this.baseUrl,
    this.name = '',
    this.pcDeviceId,
    this.deviceName,
    this.protocolVersion,
    this.capabilities = const [],
    this.lastHealth = ServerHealth.unreachable,
    this.lastUsedAt,
    this.lastLocation,
    this.pushIntent,
  });

  final String id;
  String name;
  final String host;
  final int port;
  final String baseUrl;
  String? pcDeviceId;
  String? deviceName;
  int? protocolVersion;
  List<String> capabilities;
  ServerHealth lastHealth;
  DateTime? lastUsedAt;
  LastLocation? lastLocation;
  PushIntent? pushIntent;

  bool get isOnline => lastHealth == ServerHealth.online;

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'host': host,
        'port': port,
        'baseUrl': baseUrl,
        'pcDeviceId': pcDeviceId,
        'deviceName': deviceName,
        'protocolVersion': protocolVersion,
        'capabilities': capabilities,
        'lastHealth': lastHealth.name,
        'lastUsedAt': lastUsedAt?.toIso8601String(),
        'lastLocation': lastLocation?.toJson(),
        'pushIntent': pushIntent?.toJson(),
      };

  factory ServerRecord.fromJson(Map<String, dynamic> json) {
    final healthName = json['lastHealth'] as String? ?? 'unreachable';
    return ServerRecord(
      id: json['id'] as String,
      name: json['name'] as String? ?? '',
      host: json['host'] as String,
      port: json['port'] as int,
      baseUrl: json['baseUrl'] as String,
      pcDeviceId: json['pcDeviceId'] as String?,
      deviceName: json['deviceName'] as String?,
      protocolVersion: json['protocolVersion'] as int?,
      capabilities: (json['capabilities'] as List<dynamic>? ?? const [])
          .map((e) => e as String)
          .toList(),
      lastHealth: ServerHealth.values.firstWhere(
        (value) => value.name == healthName,
        orElse: () => ServerHealth.unreachable,
      ),
      lastUsedAt: json['lastUsedAt'] == null
          ? null
          : DateTime.parse(json['lastUsedAt'] as String),
      lastLocation: json['lastLocation'] == null
          ? null
          : LastLocation.fromJson(
              json['lastLocation'] as Map<String, dynamic>,
            ),
      pushIntent: json['pushIntent'] == null
          ? null
          : PushIntent.fromJson(json['pushIntent'] as Map<String, dynamic>),
    );
  }
}

/// Result of probing `GET /api/health` on a PC.
class HealthSnapshot {
  const HealthSnapshot({
    required this.ok,
    this.deviceId,
    this.deviceName,
    this.protocolVersion,
    this.capabilities = const [],
  });

  final bool ok;
  final String? deviceId;
  final String? deviceName;
  final int? protocolVersion;
  final List<String> capabilities;
}
