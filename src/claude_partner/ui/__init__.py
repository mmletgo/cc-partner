# -*- coding: utf-8 -*-
"""UI 包：导出主窗口和面板组件。"""

from claude_partner.ui.main_window import MainWindow
from claude_partner.ui.prompt_panel import PromptPanel
from claude_partner.ui.transfer_panel import TransferPanel
from claude_partner.ui.device_panel import DevicePanel
from claude_partner.ui.scratchpad_panel import ScratchpadPanel
from claude_partner.ui.tray import SystemTray

__all__: list[str] = [
    "MainWindow",
    "PromptPanel",
    "TransferPanel",
    "DevicePanel",
    "ScratchpadPanel",
    "SystemTray",
]
