import 'package:cc_partner_mobile/settings/risk_copy.dart';
import 'package:test/test.dart';

void main() {
  test('settings risk copy is the fixed LAN statement', () {
    expect(
      kLanRiskStatement,
      '同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。',
    );
    expect(kLanRiskStatement.contains('已认证'), isFalse);
    expect(kLanRiskStatement.contains('可信'), isFalse);
  });
}
