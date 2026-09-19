import 'dart:io';

import 'package:flutter/services.dart';

const _channel = MethodChannel('cc_partner/push');

/// Read the native APNs/FCM device token if the OS provided one.
Future<String?> nativePushToken() async {
  try {
    final token = await _channel.invokeMethod<String>('getToken');
    if (token == null || token.isEmpty) {
      return null;
    }
    return token;
  } on MissingPluginException {
    return null;
  } on PlatformException {
    return null;
  }
}

String pushPlatformName() {
  if (Platform.isIOS) {
    return 'ios';
  }
  return 'android';
}
