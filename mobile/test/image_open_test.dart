import 'dart:convert';
import 'dart:typed_data';

import 'package:cc_partner_mobile/files/workspace.dart';
import 'package:test/test.dart';

void main() {
  test('open DTO dataUrl becomes displayable bytes, not a network URL', () {
    const png1x1 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';
    final bytes = imageBytesFromOpenFile({
      'image': {'dataUrl': 'data:image/png;base64,$png1x1'},
    });
    expect(bytes, isNotNull);
    expect(bytes, Uint8List.fromList(base64Decode(png1x1)));
    expect(imagePreviewUsesNetworkUrl({'image': {'dataUrl': 'data:image/png;base64,$png1x1'}}), isFalse);
  });

  test('open DTO raw base64 also becomes displayable bytes', () {
    const raw = 'aGVsbG8=';
    expect(
      imageBytesFromOpenFile({
        'image': {'base64': raw},
      }),
      Uint8List.fromList(base64Decode(raw)),
    );
  });
}
