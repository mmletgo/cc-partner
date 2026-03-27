# -*- coding: utf-8 -*-
"""全局快捷键模块：提供跨平台的全局键盘快捷键监听能力。"""

from claude_partner.hotkey.listener import (
    GlobalHotkeyManager,
    pynput_to_display,
    display_to_pynput,
    HOTKEY_PRESETS,
)

__all__ = ["GlobalHotkeyManager", "pynput_to_display", "display_to_pynput", "HOTKEY_PRESETS"]
