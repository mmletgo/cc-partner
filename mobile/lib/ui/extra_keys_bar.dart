import 'dart:async';

import 'package:flutter/material.dart';

import '../terminal/extra_keys.dart';

class ExtraKeysBar extends StatefulWidget {
  const ExtraKeysBar({
    super.key,
    required this.onSend,
    required this.sticky,
    required this.onSticky,
  });

  final ValueChanged<String> onSend;
  final StickyModifier? sticky;
  final ValueChanged<StickyModifier?> onSticky;

  @override
  State<ExtraKeysBar> createState() => _ExtraKeysBarState();
}

class _ExtraKeysBarState extends State<ExtraKeysBar> {
  Timer? _repeat;

  @override
  void dispose() {
    _repeat?.cancel();
    super.dispose();
  }

  void _press(ExtraKeyDef key) {
    if (key.kind == ExtraKeyKind.modifier && key.modifier != null) {
      widget.onSticky(toggleStickyModifier(widget.sticky, key.modifier!).armed);
      return;
    }
    final payload = key.payload;
    if (payload == null || payload.isEmpty) {
      return;
    }
    widget.onSend(payload);
  }

  void _startRepeat(ExtraKeyDef key) {
    _press(key);
    _repeat?.cancel();
    _repeat = Timer.periodic(
      const Duration(milliseconds: kExtraKeyRepeatIntervalMs),
      (_) => _press(key),
    );
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 44,
      child: ListView(
        scrollDirection: Axis.horizontal,
        children: [
          for (final key in getTerminalExtraKeys())
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 2),
              child: extraKeyHasPopup(key)
                  ? GestureDetector(
                      onLongPress: () async {
                        final chosen = await showMenu<ExtraKeyDef>(
                          context: context,
                          position: const RelativeRect.fromLTRB(40, 400, 40, 0),
                          items: [
                            for (final item in key.popup!)
                              PopupMenuItem(
                                value: item,
                                child: Text(item.label),
                              ),
                          ],
                        );
                        if (chosen != null) {
                          _press(chosen);
                        }
                      },
                      child: OutlinedButton(
                        onPressed: () => _press(key),
                        child: Text(key.label),
                      ),
                    )
                  : extraKeyIsRepeatable(key)
                      ? GestureDetector(
                          onTap: () => _press(key),
                          onLongPressStart: (_) => _startRepeat(key),
                          onLongPressEnd: (_) => _repeat?.cancel(),
                          child: OutlinedButton(
                            onPressed: () => _press(key),
                            child: Text(key.label),
                          ),
                        )
                      : OutlinedButton(
                          onPressed: () => _press(key),
                          style: widget.sticky != null &&
                                  key.modifier == widget.sticky
                              ? OutlinedButton.styleFrom(
                                  backgroundColor: Theme.of(context)
                                      .colorScheme
                                      .primaryContainer,
                                )
                              : null,
                          child: Text(key.label),
                        ),
            ),
        ],
      ),
    );
  }
}
