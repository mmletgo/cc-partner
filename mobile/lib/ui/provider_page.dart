import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../provider/client.dart';

class ProviderPage extends StatefulWidget {
  const ProviderPage({super.key, required this.book, required this.http});

  final AddressBook book;
  final LanHttpClient http;

  @override
  State<ProviderPage> createState() => _ProviderPageState();
}

class _ProviderPageState extends State<ProviderPage> {
  late final ProviderClient _client;
  ProviderSupport? _support;
  Map<String, dynamic>? _summary;
  String? _error;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _client = ProviderClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final support = await _client.probe();
      Map<String, dynamic>? summary;
      if (support == ProviderSupport.ready) {
        summary = await _client.summary();
      }
      if (mounted) {
        setState(() {
          _support = support;
          _summary = summary;
          _loading = false;
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = error.toString();
          _loading = false;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Provider')),
      body: _loading
          ? const Center(child: CircularProgressIndicator())
          : _error != null
              ? Center(child: Text(_error!))
              : _support == ProviderSupport.unsupported
                  ? const Center(child: Text('当前电脑不支持 provider-manager.v1'))
                  : ListView(
                      padding: const EdgeInsets.all(16),
                      children: [
                        const Text('手机不会安装 cc-switch CLI。'),
                        const SizedBox(height: 12),
                        Text(_summary?.toString() ?? ''),
                      ],
                    ),
    );
  }
}
