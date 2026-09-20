import 'dart:async';

import 'package:flutter/material.dart';

import '../terminal/extra_keys.dart';

/// 终端额外按键条。
///
/// Business Logic（为什么需要）:
///   手机软键盘缺少 Esc/Tab/Ctrl/方向键等；需要在终端底部提供固定键位条，
///   方向键支持按住连发（与网页版一致的 400ms 前置延迟 + 80ms 间隔），
///   `/` 长按在实际按键位置上方弹出命令菜单。
///
/// Code Logic（做什么）:
///   纯展示：横向滚动渲染全部键位；modifier 经 onSticky 交给页面管理 sticky 状态；
///   repeatable 键长按后立即发一次、延迟 [kExtraKeyRepeatDelayMs] 后按
///   [kExtraKeyRepeatIntervalMs] 连发，松手/取消停止；popup 键长按用
///   GlobalKey 的 RenderBox 计算屏幕位置弹出 showMenu。
///   [disabled] 为 true 时整条置灰：不发送、不切换 sticky、不连发、不弹菜单
///   （对齐 web MobileTerminalPanel：disabled = !sessionId || status!=='running'
///   || busy || 流未就绪，透传每键置灰）。
class ExtraKeysBar extends StatefulWidget {
  const ExtraKeysBar({
    super.key,
    required this.onSend,
    required this.sticky,
    required this.onSticky,
    this.disabled = false,
  });

  final ValueChanged<String> onSend;
  final StickyModifier? sticky;
  final ValueChanged<StickyModifier?> onSticky;

  /// 全键禁用：会话缺失/非 running/输入流未就绪等；默认 false（可用）。
  final bool disabled;

  @override
  State<ExtraKeysBar> createState() => _ExtraKeysBarState();
}

class _ExtraKeysBarState extends State<ExtraKeysBar> {
  final GlobalKey _popupAnchorKey = GlobalKey();
  Timer? _repeatDelay;
  Timer? _repeat;

  @override
  void dispose() {
    _stopRepeat();
    super.dispose();
  }

  /// 停止连发：清掉前置延迟与周期两个 Timer。
  void _stopRepeat() {
    _repeatDelay?.cancel();
    _repeatDelay = null;
    _repeat?.cancel();
    _repeat = null;
  }

  /// 发送一个键位：modifier 键交给页面切换 sticky，payload 键直接发送；
  /// disabled 时一律忽略。
  void _press(ExtraKeyDef key) {
    if (widget.disabled) {
      return;
    }
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

  /// 业务逻辑：方向键连发先立即发送一次，再经前置延迟后周期连发，避免一次误触翻过多行。
  ///
  /// Code Logic：立即 _press 一次；启动 [kExtraKeyRepeatDelayMs] 前置延迟 Timer，
  /// 到期后用 [kExtraKeyRepeatIntervalMs] 的周期 Timer 连发；disabled 时不武装。
  void _startRepeat(ExtraKeyDef key) {
    if (widget.disabled) {
      return;
    }
    _press(key);
    _stopRepeat();
    _repeatDelay = Timer(
      const Duration(milliseconds: kExtraKeyRepeatDelayMs),
      () {
        _repeatDelay = null;
        _repeat = Timer.periodic(
          const Duration(milliseconds: kExtraKeyRepeatIntervalMs),
          (_) => _press(key),
        );
      },
    );
  }

  /// 业务逻辑：`/` 命令菜单必须弹出在按键实际位置附近，而不是硬编码屏幕坐标。
  ///
  /// Code Logic：用 GlobalKey 找到按键 RenderBox，换算成 overlay 坐标后交给
  /// showMenu；锚点尚未布局时退回旧的固定位置兜底。
  RelativeRect _popupPosition() {
    final overlayRender = Overlay.of(context).context.findRenderObject();
    final anchorRender = _popupAnchorKey.currentContext?.findRenderObject();
    if (overlayRender is! RenderBox ||
        !overlayRender.hasSize ||
        anchorRender is! RenderBox ||
        !anchorRender.hasSize) {
      return const RelativeRect.fromLTRB(40, 400, 40, 0);
    }
    final overlay = overlayRender;
    final anchor = anchorRender;
    final topLeft = anchor.localToGlobal(Offset.zero, ancestor: overlay);
    final bottomRight =
        anchor.localToGlobal(anchor.size.bottomRight(Offset.zero), ancestor: overlay);
    return RelativeRect.fromRect(
      Rect.fromPoints(topLeft, bottomRight),
      Offset.zero & overlay.size,
    );
  }

  /// 弹出 `/` 命令菜单并转发所选命令；未选择时不发送；disabled 时不弹出。
  Future<void> _openPopup(ExtraKeyDef key) async {
    if (widget.disabled) {
      return;
    }
    final chosen = await showMenu<ExtraKeyDef>(
      context: context,
      position: _popupPosition(),
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
  }

  @override
  Widget build(BuildContext context) {
    final disabled = widget.disabled;
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
                      key: _popupAnchorKey,
                      onLongPress: disabled ? null : () => _openPopup(key),
                      child: OutlinedButton(
                        onPressed: disabled ? null : () => _press(key),
                        child: Text(key.label),
                      ),
                    )
                  : extraKeyIsRepeatable(key)
                      ? GestureDetector(
                          onTap: disabled ? null : () => _press(key),
                          onLongPressStart:
                              disabled ? null : (_) => _startRepeat(key),
                          onLongPressEnd:
                              disabled ? null : (_) => _stopRepeat(),
                          onLongPressCancel:
                              disabled ? null : () => _stopRepeat(),
                          child: OutlinedButton(
                            onPressed: disabled ? null : () => _press(key),
                            child: Text(key.label),
                          ),
                        )
                      : OutlinedButton(
                          onPressed: disabled ? null : () => _press(key),
                          style: !disabled &&
                                  widget.sticky != null &&
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
