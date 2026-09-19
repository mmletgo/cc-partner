import 'dart:io';

import 'book.dart';

/// JSON file persistence for the address book.
class FileAddressBookStore implements AddressBookStore {
  FileAddressBookStore(this.file);

  final File file;

  @override
  Future<String?> read() async {
    if (!await file.exists()) {
      return null;
    }
    return file.readAsString();
  }

  @override
  Future<void> write(String contents) async {
    await file.parent.create(recursive: true);
    await file.writeAsString(contents);
  }
}
