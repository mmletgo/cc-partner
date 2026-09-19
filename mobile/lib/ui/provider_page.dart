import 'package:flutter/material.dart';

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../provider/client.dart';

class ProviderPage extends StatefulWidget {
  const ProviderPage({
    super.key,
    required this.book,
    required this.http,
    this.client,
  });

  final AddressBook book;
  final LanHttpClient http;

  /// 测试注入点；为空时按当前设备 baseUrl 构造。
  final ProviderClient? client;

  @override
  State<ProviderPage> createState() => _ProviderPageState();
}

class _ProviderPageState extends State<ProviderPage> {
  late final ProviderClient _client;
  ProviderSupport? _support;
  List<ProviderApp> _apps = [];
  bool _cliAvailable = true;
  String? _error;
  bool _loading = true;
  String? _switching;

  @override
  void initState() {
    super.initState();
    _client = widget.client ?? ProviderClient(widget.http, widget.book.active!.baseUrl);
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final support = await _client.probe();
      var apps = <ProviderApp>[];
      var cliAvailable = true;
      if (support == ProviderSupport.ready) {
        final summary = await _client.summary();
        apps = _client.appsFromSummary(summary);
        cliAvailable = _client.cliFromSummary(summary).available;
      }
      if (mounted) {
        setState(() {
          _support = support;
          _apps = apps;
          _cliAvailable = cliAvailable;
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

  Future<void> _switch(String app, String providerId) async {
    setState(() => _switching = '$app:$providerId');
    try {
      await _client.switchProvider(app: app, providerId: providerId);
      await _reload();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('切换失败: $error')));
      }
    } finally {
      if (mounted) {
        setState(() => _switching = null);
      }
    }
  }

  /// Business Logic: 后端 cc-switch CLI 缺失时切换必然失败，需要先告知用户原因
  /// 与影响（文案对齐 web providerManager:status.cliMissing / cliMissingHint），
  /// 手机端不触发远端安装。
  /// Code Logic: 顶部警示卡（errorContainer 语义色）：标题 + hint 两行。
  Widget _cliMissingBanner(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      key: const Key('provider-cli-missing'),
      margin: const EdgeInsets.only(bottom: 12),
      color: theme.colorScheme.errorContainer,
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(Icons.warning_amber_rounded,
                size: 20, color: theme.colorScheme.onErrorContainer),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    '未安装 cc-switch CLI，切换功能已禁用。',
                    style: theme.textTheme.bodyMedium
                        ?.copyWith(color: theme.colorScheme.onErrorContainer),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    '安装 cc-switch CLI 会与你现有的 cc-switch 共享同一份数据，不会影响 GUI。',
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.colorScheme.onErrorContainer),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(8, 4, 8, 0),
          child: Row(
            children: [
              const Spacer(),
              IconButton(
                key: const Key('provider-refresh'),
                tooltip: '刷新',
                onPressed: _loading ? null : _reload,
                icon: const Icon(Icons.refresh),
              ),
            ],
          ),
        ),
        Expanded(
          child: _loading
              ? const Center(child: CircularProgressIndicator())
              : RefreshIndicator(
                  onRefresh: _reload,
                  child: ListView(
                    padding: const EdgeInsets.all(16),
                    children: [
                      if (_error != null) ...[
                        Text(_error!),
                        const SizedBox(height: 8),
                        FilledButton.icon(
                          key: const Key('provider-retry'),
                          onPressed: _reload,
                          icon: const Icon(Icons.refresh),
                          label: const Text('重新检测'),
                        ),
                      ] else if (_support == ProviderSupport.unsupported) ...[
                        const Text('当前电脑不支持 provider-manager.v1'),
                      ] else ...[
                        const Text('手机不会安装 cc-switch CLI。'),
                        const SizedBox(height: 12),
                        if (!_cliAvailable)
                          _cliMissingBanner(context),
                        for (final app in _apps) ...[
                          Text(app.app, style: Theme.of(context).textTheme.titleMedium),
                          for (final provider in app.providers)
                            ListTile(
                              title: Text(provider.name),
                              subtitle: Text(provider.category ?? provider.id),
                              trailing: provider.isCurrent
                                  ? const Chip(label: Text('当前'))
                                  : TextButton(
                                      onPressed: (_switching == null && _cliAvailable)
                                          ? () => _switch(app.app, provider.id)
                                          : null,
                                      child: const Text('切换'),
                                    ),
                            ),
                        ],
                      ],
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}
