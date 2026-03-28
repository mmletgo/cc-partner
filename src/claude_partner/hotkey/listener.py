# -*- coding: utf-8 -*-
"""全局快捷键管理模块：Linux 使用 Xlib XGrabKey，macOS/Windows 回退到 pynput。"""

from __future__ import annotations

import logging
import sys
import threading
import time
from typing import TYPE_CHECKING

from PyQt6.QtCore import QObject, pyqtSignal, QTimer, QMetaObject, Qt, Q_ARG

# Linux X11 方案
if sys.platform.startswith("linux"):
    from Xlib import X, XK, display as xdisplay

# macOS / Windows 回退方案
if not sys.platform.startswith("linux"):
    from pynput import keyboard

if TYPE_CHECKING:
    from Xlib.display import Display
    from pynput import keyboard as _pynput_keyboard

logger = logging.getLogger(__name__)

# pynput 格式 <-> 人类可读格式 转换预设（macOS 使用 Cmd 键，其他平台使用 Ctrl 键）
if sys.platform == "darwin":
    HOTKEY_PRESETS: list[tuple[str, str]] = [
        ("<cmd>+<shift>+s", "Cmd+Shift+S"),
        ("<cmd>+<alt>+s", "Cmd+Alt+S"),
        ("<cmd>+<shift>+a", "Cmd+Shift+A"),
        ("<cmd>+<shift>+x", "Cmd+Shift+X"),
    ]
    _DEFAULT_SCREENSHOT_HOTKEY: str = "<cmd>+<shift>+s"
else:
    HOTKEY_PRESETS: list[tuple[str, str]] = [
        ("<ctrl>+<shift>+s", "Ctrl+Shift+S"),
        ("<ctrl>+<alt>+s", "Ctrl+Alt+S"),
        ("<ctrl>+<shift>+a", "Ctrl+Shift+A"),
        ("<ctrl>+<shift>+x", "Ctrl+Shift+X"),
    ]
    _DEFAULT_SCREENSHOT_HOTKEY: str = "<ctrl>+<shift>+s"

# 修饰键映射：pynput 格式 -> 人类可读格式
_MODIFIER_TO_DISPLAY: dict[str, str] = {
    "<ctrl>": "Ctrl",
    "<shift>": "Shift",
    "<alt>": "Alt",
    "<cmd>": "Cmd",
}

# 反向映射：人类可读格式 -> pynput 格式
_DISPLAY_TO_MODIFIER: dict[str, str] = {v: k for k, v in _MODIFIER_TO_DISPLAY.items()}


def pynput_to_display(pynput_fmt: str) -> str:
    """
    Business Logic（为什么需要这个函数）:
        用户在设置面板中看到的快捷键应该是人类可读格式（如 Ctrl+Shift+S），
        而程序内部使用 pynput 格式（如 <ctrl>+<shift>+s），需要进行转换。

    Code Logic（这个函数做什么）:
        将 pynput 格式字符串按 '+' 分割，将修饰键替换为可读名称，
        普通键转为大写，最后用 '+' 连接。
        示例: "<ctrl>+<shift>+s" -> "Ctrl+Shift+S"
    """
    parts: list[str] = pynput_fmt.split("+")
    result: list[str] = []
    for part in parts:
        part_stripped = part.strip()
        if part_stripped in _MODIFIER_TO_DISPLAY:
            result.append(_MODIFIER_TO_DISPLAY[part_stripped])
        else:
            # 普通键，转大写
            result.append(part_stripped.upper())
    return "+".join(result)


def display_to_pynput(display_fmt: str) -> str:
    """
    Business Logic（为什么需要这个函数）:
        用户选择的人类可读格式快捷键（如 Ctrl+Shift+S）需要转为
        pynput 可识别的格式（如 <ctrl>+<shift>+s）才能注册热键监听。

    Code Logic（这个函数做什么）:
        将人类可读格式字符串按 '+' 分割，将修饰键名称替换为 pynput 格式，
        普通键转为小写，最后用 '+' 连接。
        示例: "Ctrl+Shift+S" -> "<ctrl>+<shift>+s"
    """
    parts: list[str] = display_fmt.split("+")
    result: list[str] = []
    for part in parts:
        part_stripped = part.strip()
        if part_stripped in _DISPLAY_TO_MODIFIER:
            result.append(_DISPLAY_TO_MODIFIER[part_stripped])
        else:
            # 普通键，转小写
            result.append(part_stripped.lower())
    return "+".join(result)


# ── X11 辅助：pynput 格式修饰键 → Xlib modifier mask ──────────────────────
_PYNPUT_TO_X11_MODIFIER: dict[str, int] = {}
if sys.platform.startswith("linux"):
    _PYNPUT_TO_X11_MODIFIER = {
        "<ctrl>": X.ControlMask,
        "<shift>": X.ShiftMask,
        "<alt>": X.Mod1Mask,
    }

# NumLock / CapsLock 干扰掩码组合（XGrabKey 必须逐一注册）
_LOCK_MASKS: list[int] = []
if sys.platform.startswith("linux"):
    _LOCK_MASKS = [
        0,
        X.Mod2Mask,                     # NumLock
        X.LockMask,                     # CapsLock
        X.Mod2Mask | X.LockMask,        # NumLock + CapsLock
    ]


def _parse_hotkey(hotkey_str: str, disp: Display) -> tuple[int, int]:
    """
    Business Logic（为什么需要这个函数）:
        将用户配置的 pynput 格式快捷键字符串解析为 Xlib 可用的
        (modifier_mask, keycode) 二元组，供 XGrabKey 注册使用。

    Code Logic（这个函数做什么）:
        按 '+' 分割字符串，逐段识别修饰键（<ctrl>/<shift>/<alt>）和普通键，
        修饰键累加为 X11 modifier mask，普通键通过 XK.string_to_keysym →
        display.keysym_to_keycode 转为 keycode。
        示例: "<ctrl>+<shift>+s" -> (ControlMask | ShiftMask, keycode_for_s)
    """
    parts: list[str] = hotkey_str.split("+")
    modifier_mask: int = 0
    key_str: str = ""

    for part in parts:
        part = part.strip()
        if part in _PYNPUT_TO_X11_MODIFIER:
            modifier_mask |= _PYNPUT_TO_X11_MODIFIER[part]
        else:
            # 普通键（去掉 pynput 的尖括号，如有）
            key_str = part.strip("<>")

    if not key_str:
        raise ValueError(f"快捷键字符串中未找到普通键: {hotkey_str}")

    keysym: int = XK.string_to_keysym(key_str)
    if keysym == 0:
        raise ValueError(f"无法解析按键 '{key_str}' 的 keysym")

    keycode: int = disp.keysym_to_keycode(keysym)
    if keycode == 0:
        raise ValueError(f"无法获取按键 '{key_str}' 的 keycode")

    return modifier_mask, keycode


class GlobalHotkeyManager(QObject):
    """
    全局快捷键管理器。

    Business Logic（为什么需要这个类）:
        截图等功能需要在任何时候（即使窗口最小化）都能通过键盘快捷键触发，
        因此需要一个全局级别的键盘监听器。

    Code Logic（这个类做什么）:
        Linux 上使用 Xlib XGrabKey 实现真正的全局快捷键监听（pynput 在部分
        Linux 桌面环境下无法捕获键盘事件）；macOS/Windows 回退到 pynput 方案。
        监听线程通过 QTimer.singleShot(0, ...) 安全地将事件分发回 Qt 主线程。
    """

    hotkey_triggered = pyqtSignal(str)  # 动作名（如 "screenshot"）

    def __init__(self, hotkeys: dict[str, str] | None = None) -> None:
        """
        Business Logic（为什么需要这个函数）:
            管理器需要知道要监听哪些快捷键以及对应的动作名称。

        Code Logic（这个函数做什么）:
            初始化快捷键映射字典和内部状态。
            hotkeys 参数格式: {action_name: pynput_format_string}
            例如: {"screenshot": "<ctrl>+<shift>+s"}

            Linux 额外维护 X11 Display、监听线程、已注册的 grab 信息。
            macOS/Windows 维护 pynput listener 引用。
        """
        super().__init__()
        self._hotkeys: dict[str, str] = hotkeys or {}
        self._running: bool = False
        self._stop_event: threading.Event = threading.Event()

        # Linux X11 专用
        self._display: Display | None = None
        self._thread: threading.Thread | None = None
        # 已注册的 grab：{(modifier_mask, keycode): action_name}
        self._grabs: dict[tuple[int, int], str] = {}

        # macOS / Windows pynput 回退
        self._listener: _pynput_keyboard.GlobalHotKeys | None = None  # type: ignore[name-defined]

    # ── 公共 API ─────────────────────────────────────────────────────────

    def start(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用启动或用户修改快捷键后，需要开始监听全局键盘事件。

        Code Logic（这个函数做什么）:
            根据平台选择监听方案：
            - Linux: 创建 X11 Display，为每个快捷键调用 XGrabKey（含
              NumLock/CapsLock 组合），启动后台线程轮询 X 事件。
            - macOS/Windows: 使用 pynput GlobalHotKeys 启动监听。
        """
        if self._running or not self._hotkeys:
            return

        if sys.platform.startswith("linux"):
            self._start_x11()
        else:
            self._start_pynput()

    def stop(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用退出或用户修改快捷键时，需要停止当前的监听。

        Code Logic（这个函数做什么）:
            根据平台选择停止方案：
            - Linux: 设置停止事件、UngrabKey、关闭 Display、等待线程结束。
            - macOS/Windows: 停止 pynput 监听线程。
        """
        if not self._running:
            return

        if sys.platform.startswith("linux"):
            self._stop_x11()
        else:
            self._stop_pynput()

    from PyQt6.QtCore import pyqtSlot

    @pyqtSlot(str)
    def _emit_hotkey(self, action: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            后台线程检测到快捷键后需要安全地在 Qt 主线程发射信号。

        Code Logic（这个函数做什么）:
            QMetaObject.invokeMethod 从后台线程调用此槽（QueuedConnection），
            确保 hotkey_triggered 信号在 Qt 主线程中发射。
        """
        self.hotkey_triggered.emit(action)

    def update_hotkey(self, action: str, new_combo: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户在设置面板中修改快捷键后，需要即时生效而不必重启应用。

        Code Logic（这个函数做什么）:
            更新指定动作的快捷键组合，如果监听器正在运行则重启以应用新配置。
        """
        self._hotkeys[action] = new_combo
        if self._running:
            self.stop()
            self.start()

    # ── Linux X11 实现 ───────────────────────────────────────────────────

    def _start_x11(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            Linux 环境下 pynput 无法可靠捕获键盘事件，需要用 Xlib XGrabKey
            直接向 X Server 注册全局快捷键。

        Code Logic（这个函数做什么）:
            1. 打开 X11 Display 连接
            2. 解析每个快捷键为 (modifier_mask, keycode)
            3. 对每个快捷键注册 4 种 Lock 掩码组合的 XGrabKey
            4. 启动后台守护线程轮询 X 事件
        """
        try:
            self._display = xdisplay.Display()
            root = self._display.screen().root
            self._grabs.clear()
            self._stop_event.clear()

            for action, combo in self._hotkeys.items():
                try:
                    mask, keycode = _parse_hotkey(combo, self._display)
                    self._grabs[(mask, keycode)] = action

                    # 同时注册 NumLock / CapsLock 的组合，否则开启时快捷键失效
                    for lock_mask in _LOCK_MASKS:
                        root.grab_key(
                            keycode,
                            mask | lock_mask,
                            True,               # owner_events
                            X.GrabModeAsync,
                            X.GrabModeAsync,
                        )
                    logger.info("X11 GrabKey 注册成功: %s -> %s", combo, action)
                except Exception as e:
                    logger.error("X11 GrabKey 注册失败 (%s): %s", combo, e)

            self._display.flush()

            if not self._grabs:
                logger.error("没有任何快捷键注册成功，放弃启动 X11 监听")
                self._display.close()
                self._display = None
                return

            self._running = True
            self._thread = threading.Thread(
                target=self._run_x11_listener,
                name="x11-hotkey-listener",
                daemon=True,
            )
            self._thread.start()
            logger.info("X11 全局快捷键监听已启动: %s", self._hotkeys)

        except Exception as e:
            logger.error("X11 全局快捷键启动失败: %s", e)
            self._running = False
            if self._display is not None:
                self._display.close()
                self._display = None

    def _run_x11_listener(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            需要在后台线程持续监听 X11 事件，将匹配的快捷键事件转发到 Qt 主线程。

        Code Logic（这个函数做什么）:
            循环检查 display.pending_events()，有事件时取出并判断是否为 KeyPress；
            匹配到已注册的 (modifier_mask, keycode) 后通过 QTimer.singleShot(0, ...)
            安全回到 Qt 主线程发射信号。匹配时需要去除 NumLock/CapsLock 位。
            无事件时 sleep(0.05) 避免空转。
        """
        # 需要清除的 lock 位掩码
        clean_mask: int = ~(X.Mod2Mask | X.LockMask) & 0xFF

        while not self._stop_event.is_set():
            try:
                if self._display is None:
                    break

                pending: int = self._display.pending_events()
                if pending > 0:
                    event = self._display.next_event()
                    if event.type == X.KeyPress:
                        # 去除 NumLock / CapsLock 干扰
                        state: int = event.state & clean_mask
                        keycode: int = event.detail
                        key_tuple: tuple[int, int] = (state, keycode)

                        action: str | None = self._grabs.get(key_tuple)
                        if action is not None:
                            logger.info("快捷键触发: %s", action)
                            # 从后台线程安全调用 Qt 主线程的槽
                            QMetaObject.invokeMethod(
                                self,
                                "_emit_hotkey",
                                Qt.ConnectionType.QueuedConnection,
                                Q_ARG(str, action),
                            )
                else:
                    time.sleep(0.05)

            except Exception as e:
                if not self._stop_event.is_set():
                    logger.error("X11 事件监听异常: %s", e)
                break

    def _stop_x11(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            停止 X11 监听时需要释放所有 GrabKey 并关闭 Display，防止资源泄漏。

        Code Logic（这个函数做什么）:
            1. 设置 _stop_event 通知监听线程退出
            2. 对每个已注册的 keycode/mask 组合调用 ungrab_key
            3. 关闭 Display 连接
            4. 等待监听线程结束（最多 2 秒）
        """
        self._stop_event.set()
        self._running = False

        if self._display is not None:
            try:
                root = self._display.screen().root
                for (mask, keycode) in self._grabs:
                    for lock_mask in _LOCK_MASKS:
                        root.ungrab_key(keycode, mask | lock_mask)
                self._display.close()
            except Exception as e:
                logger.error("X11 资源清理异常: %s", e)
            finally:
                self._display = None
                self._grabs.clear()

        if self._thread is not None:
            self._thread.join(timeout=2.0)
            self._thread = None

        logger.info("X11 全局快捷键监听已停止")

    # ── macOS / Windows pynput 回退 ──────────────────────────────────────

    def _start_pynput(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS / Windows 不支持 Xlib，需要使用 pynput 作为回退方案
            实现全局快捷键监听。

        Code Logic（这个函数做什么）:
            macOS 上先检查输入监控权限，缺失时记录警告。
            使用 keyboard.Listener + HotKey 手动实现全局热键监听（比
            GlobalHotKeys 更可靠，可以通过 canonical() 规范化按键后精确匹配）。
        """
        if sys.platform == "darwin":
            self._check_macos_permissions()

        self._running = True

        # 为每个动作创建 HotKey 对象
        hotkey_objs: list[tuple[str, keyboard.HotKey]] = []
        for action, combo in self._hotkeys.items():
            def make_callback(act: str) -> callable:
                """
                Business Logic（为什么需要这个函数）:
                    每个快捷键需要独立的回调函数，闭包确保 action 名称正确绑定。

                Code Logic（这个函数做什么）:
                    创建并返回一个闭包回调函数，该回调在被 pynput 后台线程调用时，
                    通过 QMetaObject.invokeMethod 安全地在 Qt 主线程发射信号。
                    不能使用 QTimer.singleShot —— 它从非 Qt 线程调用时在
                    macOS 上无法正确投递事件到主线程。
                """
                def cb() -> None:
                    logger.info("快捷键触发: %s", act)
                    QMetaObject.invokeMethod(
                        self,
                        "_emit_hotkey",
                        Qt.ConnectionType.QueuedConnection,
                        Q_ARG(str, act),
                    )
                return cb

            parsed_keys = keyboard.HotKey.parse(combo)
            hk = keyboard.HotKey(parsed_keys, make_callback(action))
            hotkey_objs.append((action, hk))
            logger.debug("注册热键: %s -> %s (parsed: %s)", action, combo, parsed_keys)

        def on_press(key: keyboard.Key | keyboard.KeyCode) -> None:
            """
            Business Logic（为什么需要这个函数）:
                接收每个按键事件，通过 canonical() 规范化后传给 HotKey 对象判断
                是否满足组合键条件。

            Code Logic（这个函数做什么）:
                用 listener 的 canonical() 将按键事件规范化（如 cmd_l → cmd），
                然后调用每个 HotKey.press() 进行匹配。
            """
            canonical = self._listener.canonical(key)  # type: ignore[union-attr]
            for _, hk in hotkey_objs:
                hk.press(canonical)

        def on_release(key: keyboard.Key | keyboard.KeyCode) -> None:
            """
            Business Logic（为什么需要这个函数）:
                按键释放时需要更新 HotKey 状态，避免重复触发。

            Code Logic（这个函数做什么）:
                用 canonical() 规范化后传给每个 HotKey.release()。
            """
            canonical = self._listener.canonical(key)  # type: ignore[union-attr]
            for _, hk in hotkey_objs:
                hk.release(canonical)

        try:
            self._listener = keyboard.Listener(
                on_press=on_press, on_release=on_release
            )
            self._listener.start()
            logger.info("pynput 全局快捷键监听已启动: %s", self._hotkeys)
        except Exception as e:
            logger.error("pynput 全局快捷键启动失败: %s", e)
            self._running = False

    def _check_macos_permissions(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS 10.15+ 要求应用获得"输入监控"权限才能通过 CGEventTap
            捕获全局键盘事件，缺失权限时监听器可以启动但不会收到任何事件。

        Code Logic（这个函数做什么）:
            1. 尝试用 Quartz CGEventTapCreate 创建一个测试用的被动事件监听，
               如果返回 None 说明缺少输入监控权限（最准确的检测方式）。
            2. 若 Quartz 不可用，回退到 AXIsProcessTrusted 检查辅助功能权限。
        """
        # 优先用 Quartz 直接检测输入监控权限
        try:
            import Quartz  # type: ignore[import-untyped]

            def _dummy_callback(
                proxy: object, event_type: int, event: object, refcon: object
            ) -> object:
                return event

            tap = Quartz.CGEventTapCreate(
                Quartz.kCGHIDEventTap,
                Quartz.kCGHeadInsertEventTap,
                Quartz.kCGEventTapOptionListenOnly,
                Quartz.CGEventMaskBit(Quartz.kCGEventKeyDown),
                _dummy_callback,
                None,
            )
            if tap is None:
                logger.warning(
                    "macOS 输入监控权限未授予，全局快捷键无法工作。"
                    "请前往：系统设置 → 隐私与安全性 → 输入监控，"
                    "将本应用（或终端 Terminal.app）添加并启用，然后重启应用。"
                )
                return
            # 清理测试用的事件监听
            Quartz.CFMachPortInvalidate(tap)
            logger.info("macOS 输入监控权限已确认")
            return
        except ImportError:
            logger.debug("Quartz 不可用，回退到 AXIsProcessTrusted 检查")
        except Exception as e:
            logger.debug("Quartz 权限检查异常: %s", e)

        # 回退方案：检查辅助功能权限（不能完全代替输入监控权限检查）
        try:
            import ctypes
            import ctypes.util

            lib_path: str | None = ctypes.util.find_library("ApplicationServices")
            if lib_path is None:
                return
            lib = ctypes.cdll.LoadLibrary(lib_path)
            lib.AXIsProcessTrusted.restype = ctypes.c_bool
            trusted: bool = lib.AXIsProcessTrusted()
            if not trusted:
                logger.warning(
                    "macOS 辅助功能权限未授予，全局快捷键可能无法工作。"
                    "请前往：系统设置 → 隐私与安全性 → 辅助功能 / 输入监控，"
                    "将本应用（或终端 Terminal.app）添加并启用。"
                )
            else:
                logger.info("macOS 辅助功能权限已授予（输入监控权限未检测）")
        except Exception as e:
            logger.debug("macOS 权限检查失败（不影响功能）: %s", e)

    def _stop_pynput(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用退出或用户修改快捷键时，需要停止 pynput 监听并释放资源。

        Code Logic（这个函数做什么）:
            停止 pynput 监听线程并清理引用，将运行状态标记为 False。
        """
        self._running = False
        if self._listener is not None:
            self._listener.stop()
            self._listener = None
            logger.info("pynput 全局快捷键监听已停止")
