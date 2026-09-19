import 'package:cc_partner_mobile/workbench/shell.dart';
import 'package:test/test.dart';

void main() {
  test('first version does not embed /mobile and omits automation/browser', () {
    expect(isFirstVersionPanel('terminal'), isTrue);
    expect(isFirstVersionPanel('files'), isTrue);
    expect(isFirstVersionPanel('provider'), isTrue);
    expect(isFirstVersionPanel('automation'), isFalse);
    expect(isFirstVersionPanel('browser'), isFalse);
    expect(kDeferredPanels, containsAll(['automation', 'browser']));
  });
}
