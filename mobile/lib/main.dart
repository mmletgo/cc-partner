import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'address_book/book.dart';
import 'address_book/file_store.dart';
import 'app.dart';
import 'core/lan_http.dart';
import 'core/health_probe.dart';
import 'push/apns.dart';
import 'push/fanout.dart';
import 'settings/theme.dart';

/// App sandbox documents directory (iOS HOME/Documents, Android app files).
Directory documentsDirectory() {
  final home = Platform.environment['HOME'];
  if (home != null && home.isNotEmpty) {
    return Directory('$home/Documents');
  }
  return Directory.systemTemp;
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final dir = documentsDirectory();
  await dir.create(recursive: true);
  final book = AddressBook(
    store: FileAddressBookStore(File('${dir.path}/address_book.json')),
  );
  await book.load();
  final http = LanHttpClient();
  unawaited(_registerPush(book, http));
  const MethodChannel('cc_partner/push').setMethodCallHandler((call) async {
    if (call.method == 'onToken' && call.arguments is String) {
      await _registerPush(book, http, tokenOverride: call.arguments as String);
    }
  });
  runApp(
    CcPartnerApp(
      book: book,
      http: http,
      themeStore: FileThemeStore(File('${dir.path}/theme.txt')),
    ),
  );
}

Future<void> _registerPush(
  AddressBook book,
  LanHttpClient http, {
  String? tokenOverride,
}) async {
  final token = tokenOverride ?? await nativePushToken();
  if (token == null) {
    return;
  }
  await book.refreshHealth((baseUrl) => probeLanHealth(http, baseUrl));
  await PushFanout(http).registerAll(
    book: book,
    token: token,
    platform: pushPlatformName(),
    appBuild: '0.2.1+4',
  );
  await book.persist();
}
