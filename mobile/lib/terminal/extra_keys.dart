enum ExtraKeyKind { payload, modifier }

enum StickyModifier { ctrl, alt }

class ExtraKeyDef {
  const ExtraKeyDef({
    required this.id,
    required this.kind,
    required this.label,
    this.payload,
    this.modifier,
    this.popup,
    this.repeatable = false,
  });

  final String id;
  final ExtraKeyKind kind;
  final String label;
  final String? payload;
  final StickyModifier? modifier;
  final List<ExtraKeyDef>? popup;
  final bool repeatable;
}

class StickyResult {
  const StickyResult({required this.data, required this.consume});
  final String data;
  final bool consume;
}

class StickyToggle {
  const StickyToggle(this.armed);
  final StickyModifier? armed;
}

/// 业务逻辑：sticky Ctrl/Alt 武装后若用户忘记再按键会误改写后续普通输入，需要 3 秒自动解除。
///
/// Code Logic：用外部注入的毫秒时钟记录武装时刻；shouldAutoDisarm(nowMs) 判断是否已超过
/// [kStickyTimeoutMs]，纯函数便于测试；实际解除由 UI 层的 Timer 驱动。
class StickyModifierHold {
  StickyModifierHold({this.timeoutMs = kStickyTimeoutMs});

  final int timeoutMs;

  /// 最近一次武装时刻（毫秒时间戳），未武装为 null。
  int? armedAtMs;

  bool get isArmed => armedAtMs != null;

  /// 武装（或重新武装）sticky 修饰键，nowMs 由调用方注入。
  void arm(int nowMs) {
    armedAtMs = nowMs;
  }

  /// 消费、手动切换或超时解除后取消武装计时。
  void cancel() {
    armedAtMs = null;
  }

  /// 判断 nowMs 时刻是否应自动解除；任意输入消耗 sticky 时应先 cancel 再判断。
  bool shouldAutoDisarm(int nowMs) {
    final armedAt = armedAtMs;
    if (armedAt == null) {
      return false;
    }
    return nowMs - armedAt >= timeoutMs;
  }
}

const kExtraKeyLongPressMs = 400;
const kExtraKeyRepeatDelayMs = kExtraKeyLongPressMs;
const kExtraKeyRepeatIntervalMs = 80;
const kStickyTimeoutMs = 3000;

const _csi = '\x1b[';

const kExtraKeyPayloads = <String, String>{
  'esc': '\x1b',
  'tab': '\t',
  'shiftTab': '${_csi}Z',
  'enter': '\r',
  'slash': '/',
  'slashClear': '/clear',
  'slashRewind': '/rewind',
  'slashResume': '/resume',
  'slashCompact': '/compact',
  'up': '${_csi}A',
  'down': '${_csi}B',
  'right': '${_csi}C',
  'left': '${_csi}D',
  'home': '${_csi}H',
  'end': '${_csi}F',
  'pageUp': '${_csi}5~',
  'pageDown': '${_csi}6~',
  'ctrlC': '\x03',
  'ctrlD': '\x04',
  'ctrlZ': '\x1a',
  'ctrlL': '\x0c',
  'cdUp': 'cd ..',
  'lsLa': 'ls -la',
  'clear': 'clear',
};

const _keys = <ExtraKeyDef>[
  ExtraKeyDef(
    id: 'esc',
    kind: ExtraKeyKind.payload,
    label: 'Esc',
    payload: '\x1b',
  ),
  ExtraKeyDef(
    id: 'shift-tab',
    kind: ExtraKeyKind.payload,
    label: '⇧Tab',
    payload: '$_csi' 'Z',
  ),
  ExtraKeyDef(
    id: 'slash',
    kind: ExtraKeyKind.payload,
    label: '/',
    payload: '/',
    popup: [
      ExtraKeyDef(
        id: 'slash-clear',
        kind: ExtraKeyKind.payload,
        label: '/clear',
        payload: '/clear',
      ),
      ExtraKeyDef(
        id: 'slash-rewind',
        kind: ExtraKeyKind.payload,
        label: '/rewind',
        payload: '/rewind',
      ),
      ExtraKeyDef(
        id: 'slash-resume',
        kind: ExtraKeyKind.payload,
        label: '/resume',
        payload: '/resume',
      ),
      ExtraKeyDef(
        id: 'slash-compact',
        kind: ExtraKeyKind.payload,
        label: '/compact',
        payload: '/compact',
      ),
    ],
  ),
  ExtraKeyDef(
    id: 'up',
    kind: ExtraKeyKind.payload,
    label: '↑',
    payload: '$_csi' 'A',
    repeatable: true,
  ),
  ExtraKeyDef(
    id: 'down',
    kind: ExtraKeyKind.payload,
    label: '↓',
    payload: '$_csi' 'B',
    repeatable: true,
  ),
  ExtraKeyDef(
    id: 'left',
    kind: ExtraKeyKind.payload,
    label: '←',
    payload: '$_csi' 'D',
    repeatable: true,
  ),
  ExtraKeyDef(
    id: 'right',
    kind: ExtraKeyKind.payload,
    label: '→',
    payload: '$_csi' 'C',
    repeatable: true,
  ),
  ExtraKeyDef(
    id: 'enter',
    kind: ExtraKeyKind.payload,
    label: '⏎',
    payload: '\r',
  ),
  ExtraKeyDef(
    id: 'tab',
    kind: ExtraKeyKind.payload,
    label: 'Tab',
    payload: '\t',
  ),
  ExtraKeyDef(
    id: 'ctrl',
    kind: ExtraKeyKind.modifier,
    label: 'Ctrl',
    modifier: StickyModifier.ctrl,
  ),
  ExtraKeyDef(
    id: 'alt',
    kind: ExtraKeyKind.modifier,
    label: 'Alt',
    modifier: StickyModifier.alt,
  ),
  ExtraKeyDef(
    id: 'ctrl-c',
    kind: ExtraKeyKind.payload,
    label: '^C',
    payload: '\x03',
  ),
  ExtraKeyDef(
    id: 'ctrl-d',
    kind: ExtraKeyKind.payload,
    label: '^D',
    payload: '\x04',
  ),
  ExtraKeyDef(
    id: 'ctrl-z',
    kind: ExtraKeyKind.payload,
    label: '^Z',
    payload: '\x1a',
  ),
  ExtraKeyDef(
    id: 'ctrl-l',
    kind: ExtraKeyKind.payload,
    label: '^L',
    payload: '\x0c',
  ),
  ExtraKeyDef(
    id: 'home',
    kind: ExtraKeyKind.payload,
    label: 'Home',
    payload: '$_csi' 'H',
  ),
  ExtraKeyDef(
    id: 'end',
    kind: ExtraKeyKind.payload,
    label: 'End',
    payload: '$_csi' 'F',
  ),
  ExtraKeyDef(
    id: 'pgup',
    kind: ExtraKeyKind.payload,
    label: 'PgUp',
    payload: '$_csi' '5~',
  ),
  ExtraKeyDef(
    id: 'pgdn',
    kind: ExtraKeyKind.payload,
    label: 'PgDn',
    payload: '$_csi' '6~',
  ),
  ExtraKeyDef(
    id: 'cd-up',
    kind: ExtraKeyKind.payload,
    label: 'cd..',
    payload: 'cd ..',
  ),
  ExtraKeyDef(
    id: 'ls-la',
    kind: ExtraKeyKind.payload,
    label: 'ls',
    payload: 'ls -la',
  ),
  ExtraKeyDef(
    id: 'clear-snippet',
    kind: ExtraKeyKind.payload,
    label: 'clr',
    payload: 'clear',
  ),
];

List<ExtraKeyDef> getTerminalExtraKeys() => List<ExtraKeyDef>.from(_keys);

bool extraKeyHasPopup(ExtraKeyDef key) =>
    key.popup != null && key.popup!.isNotEmpty;

bool extraKeyIsRepeatable(ExtraKeyDef key) => key.repeatable;

String? encodeCtrlKeyInput(String data) {
  if (data.length != 1) {
    return null;
  }
  final code = data.codeUnitAt(0);
  if (code == 0x20 || code == 0x40) {
    return '\x00';
  }
  if ((code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a)) {
    return String.fromCharCode(code & 0x1f);
  }
  if (code >= 0x5b && code <= 0x5f) {
    return String.fromCharCode(code & 0x1f);
  }
  if (data == '?') {
    return '\x7f';
  }
  return null;
}

String? encodeAltKeyInput(String data) {
  if (data.length != 1) {
    return null;
  }
  return '\x1b$data';
}

StickyResult applyStickyModifier(StickyModifier? modifier, String data) {
  if (modifier == null) {
    return StickyResult(data: data, consume: false);
  }
  if (modifier == StickyModifier.ctrl) {
    return StickyResult(data: encodeCtrlKeyInput(data) ?? data, consume: true);
  }
  return StickyResult(data: encodeAltKeyInput(data) ?? data, consume: true);
}

StickyToggle toggleStickyModifier(
  StickyModifier? current,
  StickyModifier next,
) {
  if (current == next) {
    return const StickyToggle(null);
  }
  return StickyToggle(next);
}

ExtraKeyDef? selectPopupItem(ExtraKeyDef key, String hitId) {
  if (hitId == 'trigger' || hitId == key.id) {
    return key;
  }
  return key.popup?.where((item) => item.id == hitId).firstOrNull;
}
