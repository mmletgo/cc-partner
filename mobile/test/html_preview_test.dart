import 'package:cc_partner_mobile/files/html_preview.dart';
import 'package:test/test.dart';

void main() {
  test('HTML preview disables scripts', () {
    expect(HtmlPreviewPolicy.scriptsEnabled, isFalse);
    expect(HtmlPreviewPolicy.referrerPolicy, 'no-referrer');
    expect(HtmlPreviewPolicy.modes, containsAll(['source', 'wysiwyg', 'split']));
  });

  test('rewrites relative assets to data URLs', () {
    const html = '<link href="style.css"><img src="pic.png">';
    final rewritten = rewriteRelativeAssetsToDataUrls(
      html,
      loadAssetDataUrl: (path) => 'data:text/plain,$path',
    );
    expect(rewritten, contains('data:text/plain,style.css'));
    expect(rewritten, contains('data:text/plain,pic.png'));
    expect(rewritten.contains('http://'), isFalse);
  });
}
