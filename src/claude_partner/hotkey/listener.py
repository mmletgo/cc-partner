# -*- coding: utf-8 -*-
"""全局快捷键管理模块：使用 pynput 在后台线程监听键盘快捷键。"""

from PyQt6.QtCore import QObject, pyqtSignal, QTimer
from pynput import keyboard
import logging
import threading

logger = logging.getLogger(__name__)

# pynput 格式 <-> 人类可读格式 转换预设
HOTKEY_PRESETS: list[tuple[str, str]] = [
    ("<ctrl>+<shift>+s", "Ctrl+Shift+S"),
    ("<ctrl>+<alt>+s", "Ctrl+Alt+S"),
    ("<ctrl>+<shift>+a", "Ctrl+Shift+A"),
    ("<ctrl>+<shift>+x", "Ctrl+Shift+X"),
]

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


class GlobalHotkeyManager(QObject):
    """
    全局快捷键管理器。

    Business Logic（为什么需要这个类）:
        截图等功能需要在任何时候（即使窗口最小化）都能通过键盘快捷键触发，
        因此需要一个全局级别的键盘监听器。

    Code Logic（这个类做什么）:
        使用 pynput 在后台线程监听键盘，匹配到快捷键时通过 Qt 信号安全通知主线程。
        pynput 的回调运行在后台线程中，通过 QTimer.singleShot(0, ...) 安全地切换到
        Qt 主线程后再发射信号。
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
        """
        super().__init__()
        self._hotkeys: dict[str, str] = hotkeys or {}
        self._listener: keyboard.GlobalHotKeys | None = None
        self._running: bool = False

    def start(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用启动或用户修改快捷键后，需要开始监听全局键盘事件。

        Code Logic（这个函数做什么）:
            根据 _hotkeys 字典构建 pynput GlobalHotKeys 的 bindings，
            为每个动作创建闭包回调，回调中使用 QTimer.singleShot(0, ...)
            将信号发射安全地调度到 Qt 主线程。
        """
        if self._running or not self._hotkeys:
            return
        self._running = True
        bindings: dict[str, callable] = {}
        for action, combo in self._hotkeys.items():
            # 用闭包捕获 action
            def make_callback(act: str) -> callable:
                """
                Business Logic（为什么需要这个函数）:
                    每个快捷键需要独立的回调函数，闭包确保 action 名称正确绑定。

                Code Logic（这个函数做什么）:
                    创建并返回一个闭包回调函数，该回调在被 pynput 后台线程调用时，
                    通过 QTimer.singleShot(0, ...) 安全地在 Qt 主线程发射信号。
                """
                def cb() -> None:
                    logger.info("快捷键触发: %s", act)
                    # 从 pynput 后台线程安全通知 Qt 主线程
                    QTimer.singleShot(0, lambda: self.hotkey_triggered.emit(act))
                return cb
            bindings[combo] = make_callback(action)

        try:
            self._listener = keyboard.GlobalHotKeys(bindings)
            self._listener.start()
            logger.info("全局快捷键监听已启动: %s", self._hotkeys)
        except Exception as e:
            logger.error("全局快捷键启动失败: %s", e)
            self._running = False

    def stop(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用退出或用户修改快捷键时，需要停止当前的监听。

        Code Logic（这个函数做什么）:
            停止 pynput 监听线程并清理资源，将运行状态标记为 False。
        """
        self._running = False
        if self._listener is not None:
            self._listener.stop()
            self._listener = None
            logger.info("全局快捷键监听已停止")

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
