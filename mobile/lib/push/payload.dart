/// Notify payload sent through the internet relay. No terminal/path/prompt bytes.
class PushPayload {
  const PushPayload({
    required this.pcDeviceId,
    required this.category,
    required this.title,
    required this.body,
    this.projectId,
    this.sessionId,
    this.worktreeId,
  });

  final String pcDeviceId;
  final String category;
  final String title;
  final String body;
  final String? projectId;
  final String? sessionId;
  final String? worktreeId;

  Map<String, dynamic> toJson() {
    final map = <String, dynamic>{
      'pcDeviceId': pcDeviceId,
      'category': category,
      'title': title,
      'body': body,
    };
    if (projectId != null) map['projectId'] = projectId;
    if (sessionId != null) map['sessionId'] = sessionId;
    if (worktreeId != null) map['worktreeId'] = worktreeId;
    assertNoSensitiveFields(map);
    return map;
  }
}

const _forbiddenKeys = {
  'terminal',
  'output',
  'prompt',
  'path',
  'filePath',
  'hostPath',
  'cwd',
  'transcript',
};

void assertNoSensitiveFields(Map<String, dynamic> payload) {
  for (final key in payload.keys) {
    if (_forbiddenKeys.contains(key)) {
      throw FormatException('push payload must not include $key');
    }
  }
  for (final value in payload.values) {
    if (value is String && (value.contains('/Users/') || value.contains('\\\\'))) {
      throw FormatException('push payload must not include filesystem paths');
    }
  }
}

class PushRegistration {
  const PushRegistration({
    required this.mobileDeviceId,
    required this.platform,
    required this.token,
    required this.appBuild,
  });

  final String mobileDeviceId;
  final String platform;
  final String token;
  final String appBuild;

  Map<String, dynamic> toJson() => {
        'mobileDeviceId': mobileDeviceId,
        'platform': platform,
        'token': token,
        'appBuild': appBuild,
      };
}
