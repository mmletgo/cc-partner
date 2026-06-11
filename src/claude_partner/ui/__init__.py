# -*- coding: utf-8 -*-
"""UI 包：基于 QWebEngineView 的主窗口 + 系统托盘。

旧版 PyQt6 原生面板（MainWindow/PromptPanel/TransferPanel 等）已废弃移除，
界面统一由 web/ React 前端通过 QWebEngineView 嵌入渲染。
"""

from claude_partner.ui.web_main_window import WebMainWindow
from claude_partner.ui.tray import SystemTray

__all__: list[str] = [
    "WebMainWindow",
    "SystemTray",
]
