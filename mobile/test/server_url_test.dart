import 'package:cc_partner_mobile/core/server_url.dart';
import 'package:test/test.dart';

void main() {
  test('strips /mobile path from desktop QR URL', () {
    final parsed = parseServerInput('http://192.168.1.8:62116/mobile');
    expect(parsed.host, '192.168.1.8');
    expect(parsed.port, 62116);
    expect(parsed.baseUrl, 'http://192.168.1.8:62116');
  });

  test('strips nested path after /mobile', () {
    final parsed = parseServerInput(
      'http://Office-PC.local:62117/mobile/workbench?x=1',
    );
    expect(parsed.host, 'office-pc.local');
    expect(parsed.port, 62117);
    expect(parsed.baseUrl, 'http://office-pc.local:62117');
  });

  test('defaults port to 62116 when omitted', () {
    final parsed = parseServerInput('10.0.0.4');
    expect(parsed.host, '10.0.0.4');
    expect(parsed.port, kDefaultLanPort);
    expect(parsed.baseUrl, 'http://10.0.0.4:62116');
  });

  test('accepts host:port without scheme', () {
    final parsed = parseServerInput('10.0.0.4:62118');
    expect(parsed.port, 62118);
    expect(parsed.baseUrl, 'http://10.0.0.4:62118');
  });

  test('URL without port uses 62116', () {
    final parsed = parseServerInput('http://10.0.0.9/mobile');
    expect(parsed.port, 62116);
    expect(parsed.baseUrl, 'http://10.0.0.9:62116');
  });

  test('rejects empty input', () {
    expect(() => parseServerInput('  '), throwsFormatException);
  });
}
