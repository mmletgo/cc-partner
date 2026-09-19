import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:image_picker/image_picker.dart';
import 'package:xterm/xterm.dart' hide TerminalController;
import 'package:xterm/xterm.dart' as xterm show TerminalController;

import '../address_book/book.dart';
import '../core/lan_http.dart';
import '../projects/client.dart';
import '../prompts/client.dart';
import '../sessions/client.dart';
import '../terminal/controller.dart';
import '../terminal/extra_keys.dart';
import 'extra_keys_bar.dart';

class TerminalPage extends StatefulWidget {
  const TerminalPage({
    super.key,
    required this.book,
    required this.http,
    required this.project,
    this.preferredSessionId,
    this.worktreeId,
  });

  final AddressBook book;
  final LanHttpClient http;
  final ProjectSummary project;
  final String? preferredSessionId;
  final String? worktreeId;

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage> {
  late final SessionsClient _sessions;
  late final TerminalController _policy;
  final Terminal _terminal = Terminal(maxLines: 5000);
  final _view = xterm.TerminalController();
  final _input = TextEditingController();
  WebSocket? _socket;
  HttpClient? _eventsClient;
  String? _sessionId;
  String? _error;
  String _status = '连接中';
  String? _laneId;
  int _seq = 1;
  String? _owner;
  int _sequence = 0;
  StickyModifier? _sticky;
  List<SessionSummary> _sessionList = [];

  @override
  void initState() {
    super.initState();
    _sessions = SessionsClient(widget.http, widget.book.active!.baseUrl);
    _policy = TerminalController(sessionId: widget.preferredSessionId ?? '');
    _boot();
  }

  @override
  void dispose() {
    _socket?.close();
    _eventsClient?.close(force: true);
    _input.dispose();
    _view.dispose();
    super.dispose();
  }

  Future<void> _boot() async {
    try {
      var sessions = await _sessions.list(widget.project.id);
      _sessionList = sessions;
      var session = sessions.where((s) => s.id == widget.preferredSessionId).firstOrNull ??
          sessions.where((s) => s.status != 'exited').firstOrNull;
      session ??= await _sessions.create(widget.project.id, worktreeId: widget.worktreeId);
      _sessionId = session.id;
      await _sessions.focus(session.id);
      final replay = await _sessions.replay(session.id);
      final snapshot = replay['snapshot'] as String? ??
          replay['data'] as String? ??
          replay['output'] as String? ??
          '';
      if (snapshot.isNotEmpty) {
        _terminal.write(snapshot);
      }
      await _openInput();
      _listenEvents();
      if (mounted) {
        setState(() => _status = '就绪');
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
  }

  Future<void> _openInput() async {
    _socket?.close();
    final socket = await widget.http.openWebSocket(
      widget.book.active!.baseUrl,
      TerminalController.inputPath,
      protocols: [TerminalController.inputSubprotocol],
    );
    _socket = socket;
    _laneId = 'lane-${DateTime.now().microsecondsSinceEpoch}';
    socket.add(jsonEncode({
      'type': 'hello',
      'clientId': 'mobile-${DateTime.now().microsecondsSinceEpoch}',
    }));
    socket.listen((event) {
      if (event is! String) {
        return;
      }
      try {
        final frame = jsonDecode(event);
        if (frame is Map && frame['type'] == 'ack') {
          _policy.onAck('${frame['seq']}');
        }
        if (frame is Map && (frame['type'] == 'gap' || frame['kind'] == 'gap')) {
          _policy.onNdjsonLine({'type': 'gap'});
        }
      } catch (_) {}
    }, onDone: () {
      if (!mounted) {
        return;
      }
      _policy.takeUnackedOnDisconnect();
      setState(() => _status = '输入已断开；未确认输入不会自动重放');
    });
  }

  Future<void> _listenEvents() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    _eventsClient?.close(force: true);
    final client = HttpClient();
    _eventsClient = client;
    final http = LanHttpClient(client: client);
    var query = 'terminalSessionId=${Uri.encodeQueryComponent(sessionId)}';
    if (_owner != null) {
      query +=
          '&afterOwnerInstanceId=${Uri.encodeQueryComponent(_owner!)}&afterSequence=$_sequence';
    }
    try {
      await for (final line in http.streamLines(
        widget.book.active!.baseUrl,
        '${TerminalController.eventsPath}?$query',
      )) {
        if (!mounted || line.trim().isEmpty) {
          continue;
        }
        Map<String, dynamic> frame;
        try {
          frame = jsonDecode(line) as Map<String, dynamic>;
        } catch (_) {
          continue;
        }
        final type = frame['type'] as String? ?? '';
        if (type == 'heartbeat') {
          continue;
        }
        final owner = frame['ownerInstanceId'] as String?;
        final seq = frame['sequence'];
        if (owner != null) {
          _owner = owner;
        }
        if (seq is int) {
          _sequence = seq;
        }
        if (type == 'gap') {
          _policy.onNdjsonLine({'type': 'gap'});
          _policy.beginReplay();
          final replay = await _sessions.replay(sessionId);
          final snapshot = replay['snapshot'] as String? ??
              replay['data'] as String? ??
              replay['output'] as String? ??
              '';
          _terminal.write('\x1b[2J\x1b[H');
          if (snapshot.isNotEmpty) {
            _terminal.write(snapshot);
          }
          _policy.finishReplay();
          continue;
        }
        if (type == 'terminalOutput') {
          final payload = frame['payload'];
          if (payload is Map && payload['sessionId'] == sessionId) {
            final chunk = payload['chunk'] as String? ?? '';
            if (chunk.isNotEmpty) {
              _terminal.write(chunk);
            }
          }
        }
        if (type == 'terminalResync') {
          final payload = frame['payload'];
          if (payload is Map) {
            final snapshot = payload['snapshot'] as String? ?? payload['data'] as String? ?? '';
            if (snapshot.isNotEmpty) {
              _terminal.write('\x1b[2J\x1b[H');
              _terminal.write(snapshot);
            }
          }
        }
      }
    } catch (error) {
      if (mounted) {
        setState(() => _status = '实时输出断开，将重连: $error');
      }
    }
  }

  void _send(String data) {
    final sessionId = _sessionId;
    final socket = _socket;
    if (sessionId == null || socket == null || socket.readyState != WebSocket.open) {
      return;
    }
    if (_policy.sync == TerminalSync.gapReplayRequired) {
      return;
    }
    final applied = applyStickyModifier(_sticky, data);
    if (applied.consume) {
      setState(() => _sticky = null);
    }
    final seq = _seq++;
    _policy.sendInput(applied.data);
    socket.add(jsonEncode({
      'type': 'input',
      'laneId': _laneId,
      'sessionId': sessionId,
      'seq': seq,
      'data': applied.data,
    }));
  }

  Future<void> _copySelection() async {
    final selection = _view.selection;
    if (selection == null) {
      return;
    }
    final text = _terminal.buffer.getText(selection);
    if (text.isEmpty) {
      return;
    }
    await Clipboard.setData(ClipboardData(text: text));
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已复制')));
    }
  }

  Future<void> _pickFavorite() async {
    final prompts = await PromptsClient(widget.http, widget.book.active!.baseUrl).listFavorites();
    if (!mounted) {
      return;
    }
    final chosen = await showModalBottomSheet<FavoritePrompt>(
      context: context,
      builder: (context) => ListView(
        children: [
          for (final prompt in prompts)
            ListTile(
              title: Text(prompt.title),
              subtitle: Text(prompt.content, maxLines: 2, overflow: TextOverflow.ellipsis),
              onTap: () => Navigator.pop(context, prompt),
            ),
        ],
      ),
    );
    if (chosen != null) {
      _send(chosen.content);
    }
  }

  Future<void> _optimize() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    final controller = TextEditingController(text: _input.text);
    final prompt = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Prompt 优化'),
        content: TextField(controller: controller, maxLines: 4),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('写入会话'),
          ),
        ],
      ),
    );
    if (prompt == null || prompt.isEmpty) {
      return;
    }
    await PromptsClient(widget.http, widget.book.active!.baseUrl).streamOptimizerToSession(
      prompt: prompt,
      sessionId: sessionId,
      workingDirectory: widget.project.path,
    );
  }

  Future<void> _pickSession() async {
    final sessions = await _sessions.list(widget.project.id);
    if (!mounted) {
      return;
    }
    final chosen = await showModalBottomSheet<SessionSummary>(
      context: context,
      builder: (context) => ListView(
        children: [
          ListTile(
            title: const Text('新建 session'),
            onTap: () => Navigator.pop(
              context,
              SessionSummary(id: '', projectId: widget.project.id, name: 'new', status: 'new'),
            ),
          ),
          for (final session in sessions)
            ListTile(
              title: Text(session.name.isEmpty ? session.id : session.name),
              subtitle: Text(session.status),
              onTap: () => Navigator.pop(context, session),
            ),
        ],
      ),
    );
    if (chosen == null) {
      return;
    }
    SessionSummary session = chosen;
    if (session.id.isEmpty) {
      session = await _sessions.create(widget.project.id, worktreeId: widget.worktreeId);
    }
    await _sessions.focus(session.id);
    setState(() {
      _sessionId = session.id;
      _sessionList = sessions;
    });
    await _openInput();
    _listenEvents();
  }

  Future<void> _pasteImage() async {
    final sessionId = _sessionId;
    if (sessionId == null) {
      return;
    }
    final picked = await ImagePicker().pickImage(source: ImageSource.gallery);
    if (picked == null) {
      return;
    }
    final bytes = await picked.readAsBytes();
    final b64 = base64Encode(bytes);
    final mime = picked.mimeType ?? 'image/jpeg';
    await _sessions.pasteImage(sessionId, 'data:$mime;base64,$b64');
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      return Center(child: Text(_error!));
    }
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
          child: Row(
            children: [
              Expanded(child: Text(_status, style: Theme.of(context).textTheme.bodySmall)),
              IconButton(
                tooltip: '会话',
                onPressed: _pickSession,
                icon: const Icon(Icons.tab),
              ),
              IconButton(
                tooltip: '收藏 Prompt',
                onPressed: _pickFavorite,
                icon: const Icon(Icons.star_outline),
              ),
              IconButton(
                tooltip: 'Prompt 优化',
                onPressed: _optimize,
                icon: const Icon(Icons.auto_fix_high),
              ),
              IconButton(
                tooltip: '相册贴图',
                onPressed: _pasteImage,
                icon: const Icon(Icons.photo_outlined),
              ),
            ],
          ),
        ),
        Expanded(
          child: GestureDetector(
            onLongPress: _copySelection,
            child: TerminalView(_terminal, controller: _view),
          ),
        ),
        ExtraKeysBar(
          onSend: _send,
          sticky: _sticky,
          onSticky: (value) => setState(() => _sticky = value),
        ),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _input,
                  decoration: const InputDecoration(hintText: '输入后回车发送'),
                  onSubmitted: (value) {
                    _send('$value\r');
                    _input.clear();
                  },
                ),
              ),
              IconButton(
                onPressed: () {
                  _send('${_input.text}\r');
                  _input.clear();
                },
                icon: const Icon(Icons.send),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
