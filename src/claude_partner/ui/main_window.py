# -*- coding: utf-8 -*-
"""主窗口：Tab 布局的应用主界面，集成 Prompt 管理、文件传输和设备列表面板。"""

from PyQt6.QtWidgets import QMainWindow, QTabWidget, QWidget
from PyQt6.QtCore import QSize, Qt
from PyQt6.QtGui import QCloseEvent

from claude_partner.ui.prompt_panel import PromptPanel
from claude_partner.ui import theme


class MainWindow(QMainWindow):
    """
    应用主窗口：使用 Tab 布局组织多个功能面板。

    Business Logic（为什么需要这个类）:
        应用有多个核心功能模块（Prompt 管理、文件传输、设备列表），
        需要一个主窗口以 Tab 切换的方式统一管理和展示。

    Code Logic（这个类做什么）:
        QMainWindow 内以 QTabWidget 为中心组件，包含三个 Tab 页：
        Tab 1 - Prompt 管理面板，Tab 2 - 文件传输面板，Tab 3 - 设备列表面板。
        后两个面板可传入占位 QWidget，待后续开发完成后替换。
    """

    def __init__(
        self,
        prompt_panel: PromptPanel,
        transfer_panel: QWidget | None = None,
        device_panel: QWidget | None = None,
        scratchpad_panel: QWidget | None = None,
        settings_panel: QWidget | None = None,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化主窗口，将各功能面板注册为 Tab 页。

        Code Logic（这个函数做什么）:
            创建 QTabWidget，添加三个 Tab 页（Prompt / 文件传输 / 设备列表），
            设置窗口标题、默认大小和最小尺寸。
        """
        super().__init__()

        self.setWindowTitle("Claude Partner")
        self.setWindowIcon(theme.create_app_icon(128))
        self.resize(900, 700)
        self.setMinimumSize(QSize(600, 500))

        # Tab 布局
        self._tab_widget: QTabWidget = QTabWidget()
        self._tab_widget.setStyleSheet(theme.tab_bar_style())
        self._tab_widget.tabBar().setElideMode(Qt.TextElideMode.ElideNone)
        self.setCentralWidget(self._tab_widget)

        # Tab 1: Prompt 管理面板
        self._prompt_panel: PromptPanel = prompt_panel
        self._tab_widget.addTab(self._prompt_panel, "Prompt 管理")

        # Tab 2: 文件传输面板（占位）
        self._transfer_panel: QWidget = transfer_panel or QWidget()
        self._tab_widget.addTab(self._transfer_panel, "文件传输")

        # Tab 3: 设备列表面板（占位）
        self._device_panel: QWidget = device_panel or QWidget()
        self._tab_widget.addTab(self._device_panel, "设备列表")

        # Tab 4: 速记本面板
        self._scratchpad_panel: QWidget = scratchpad_panel or QWidget()
        self._tab_widget.addTab(self._scratchpad_panel, "速记本")

        # Tab 5: 设置面板
        self._settings_panel: QWidget = settings_panel or QWidget()
        self._tab_widget.addTab(self._settings_panel, "设置")

    def _refresh_theme(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统主题切换时，主窗口需要刷新所有子面板的样式，
            使文字、背景、边框等颜色与新主题一致。

        Code Logic（这个函数做什么）:
            重新应用 Tab 栏样式，级联调用各面板的 _reapply_styles() 方法。
            对于 settings_panel，通过 hasattr 检查是否支持刷新。
        """
        self._tab_widget.setStyleSheet(theme.tab_bar_style())

        # 刷新各面板样式
        self._prompt_panel._reapply_styles()

        if hasattr(self._transfer_panel, "_reapply_styles"):
            self._transfer_panel._reapply_styles()

        if hasattr(self._device_panel, "_reapply_styles"):
            self._device_panel._reapply_styles()

        if hasattr(self._settings_panel, "_reapply_styles"):
            self._settings_panel._reapply_styles()

        if hasattr(self._scratchpad_panel, "_reapply_styles"):
            self._scratchpad_panel._reapply_styles()

    def closeEvent(self, event: QCloseEvent) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS 上用户点击红色关闭按钮时，应将窗口隐藏到托盘而不是退出应用，
            这样后台同步和快捷键等功能可以继续工作。直接关闭窗口会让用户误以为
            应用卡死（进程仍在运行但无可见界面）。

        Code Logic（这个函数做什么）:
            拦截关闭事件，改为隐藏窗口。用户可通过托盘菜单重新打开或退出。
        """
        event.ignore()
        self.hide()

    @property
    def prompt_panel(self) -> PromptPanel:
        """
        Business Logic（为什么需要这个函数）:
            外部（如 app.py）需要访问 Prompt 面板以触发刷新或连接信号。

        Code Logic（这个函数做什么）:
            返回 Prompt 管理面板实例。
        """
        return self._prompt_panel
