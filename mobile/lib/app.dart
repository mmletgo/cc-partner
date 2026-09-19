import 'package:flutter/material.dart';

import 'address_book/book.dart';
import 'core/lan_http.dart';
import 'ui/address_book_page.dart';

/// Root Flutter app. Workbench is self-drawn, not a WebView of `/mobile`.
class CcPartnerApp extends StatelessWidget {
  const CcPartnerApp({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'cc-partner',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFC96442)),
        useMaterial3: true,
      ),
      home: AddressBookPage(book: book, http: http),
    );
  }
}
