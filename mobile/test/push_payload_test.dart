import 'package:cc_partner_mobile/push/payload.dart';
import 'package:test/test.dart';

void main() {
  test('notify payload has no terminal, path, or prompt fields', () {
    final json = const PushPayload(
      pcDeviceId: 'pc-1',
      category: 'agentNeedsInput',
      title: '待处理',
      body: '项目 demo 需要输入',
      projectId: 'p1',
      sessionId: 's1',
    ).toJson();
    expect(json.keys, containsAll(['pcDeviceId', 'category', 'title', 'body']));
    expect(json.containsKey('terminal'), isFalse);
    expect(json.containsKey('path'), isFalse);
    expect(json.containsKey('prompt'), isFalse);
    expect(json.containsKey('output'), isFalse);
  });

  test('rejects forbidden keys', () {
    expect(
      () => assertNoSensitiveFields({'prompt': 'do stuff'}),
      throwsFormatException,
    );
  });
}
