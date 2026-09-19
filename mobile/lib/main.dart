import 'dart:io';

import 'package:flutter/material.dart';

import 'address_book/book.dart';
import 'address_book/file_store.dart';
import 'app.dart';
import 'core/lan_http.dart';

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
  runApp(CcPartnerApp(book: book, http: LanHttpClient()));
}
