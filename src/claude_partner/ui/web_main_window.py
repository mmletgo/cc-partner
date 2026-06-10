# -*- coding: utf-8 -*-
"""
WebMainWindow - 基于 QWebEngineView 的新主窗口，嵌入 web/ 前端。

将 PyQt6 与 React 前端集成：
- 加载 web/dist/index.html（PyInstaller 打包后）或 http://localhost:5173（dev 模式）
- 前端通过 fetch 调用 aiohttp HTTP API（同源/同进程）
- 保留 QSystemTrayIcon 和全局快捷键
"""

from __future__ import annotations

import logging
import os
import sys
from pathlib import Path

from PyQt6.QtCore import QSize, QUrl
from PyQt6.QtGui import QCloseEvent
from PyQt6.QtWidgets import QMainWindow
from PyQt6.QtWebEngineWidgets import QWebEngineView
from PyQt6.QtWebEngineCore import QWebEngineSettings

from claude_partner.ui import theme

logger: logging.Logger = logging.getLogger(__name__)


def _resolve_frontend_url(backend_port: int = 0) -> QUrl:
    """
    Business Logic:
        前端 URL 来源优先级：
        1. CP_FRONTEND_URL 环境变量（强制指定，便于 e2e 测试）
        2. Vite dev server (http://localhost:5173)，开发态自动启用
        3. 后端静态资源服务（http://localhost:{backend_port}/），需要 backend_port > 0
        4. 打包后的 web/dist/index.html（file:// 降级方案）
    """
    env_url = os.environ.get("CP_FRONTEND_URL")
    if env_url:
        logger.info("使用 CP_FRONTEND_URL: %s", env_url)
        return QUrl(env_url)

    # dev 模式：尝试连接 Vite
    is_frozen = getattr(sys, "frozen", False)
    if not is_frozen and os.environ.get("CP_NO_DEV") != "1":
        vite_url = QUrl("http://localhost:5173")
        logger.info("dev 模式加载 Vite: %s", vite_url.toString())
        return vite_url

    # production：优先使用后端静态资源服务（同源，避免跨域问题）
    if backend_port > 0:
        url = QUrl(f"http://localhost:{backend_port}/")
        logger.info("加载后端静态资源: %s", url.toString())
        return url

    # 降级：从本地文件系统加载
    if is_frozen:
        # PyInstaller 临时目录
        base = Path(getattr(sys, "_MEIPASS", ".")) / "web" / "dist"
    else:
        # 源运行：从项目根目录 web/dist 加载
        base = Path(__file__).resolve().parents[3] / "web" / "dist"

    index_path = base / "index.html"
    if not index_path.exists():
        logger.warning("前端 dist 不存在: %s，降级到 dev", index_path)
        return QUrl("http://localhost:5173")

    logger.info("加载本地前端文件: %s", index_path)
    return QUrl.fromLocalFile(str(index_path))


class WebMainWindow(QMainWindow):
    """
    Business Logic:
        新版主窗口，用 QWebEngineView 嵌入 React 前端。
        保留 PyQt6 系统托盘、全局快捷键、qasync 异步桥。
        前端通过 fetch 调用现有 aiohttp HTTP API 完成数据交互。

    Code Logic:
        - 中心 QWebEngineView 加载 web/dist/index.html
        - 窗口外观（标题/图标/大小）保持与旧 MainWindow 一致
        - 启用 LocalStorage、DeveloperExtras（dev 模式可打开 DevTools）
        - 关闭事件只隐藏窗口（由 tray 真正退出应用）
    """

    def __init__(self, backend_port: int = 0) -> None:
        """
        Business Logic（为什么需要这个函数）:
            创建新版主窗口，用 QWebEngineView 嵌入 React 前端。
            需要传入后端实际监听端口，以便通过 HTTP 同源方式加载前端静态资源。

        Code Logic（这个函数做什么）:
            初始化窗口外观、QWebEngineView，通过 backend_port 解析前端 URL。
        """
        super().__init__()

        self.setWindowTitle("Claude Partner")
        self.setWindowIcon(theme.create_app_icon(128))
        self.resize(1200, 760)
        self.setMinimumSize(QSize(900, 600))

        # QWebEngineView 容器
        self._web: QWebEngineView = QWebEngineView()
        self._configure_web_engine()
        self._web.setUrl(_resolve_frontend_url(backend_port))

        self.setCentralWidget(self._web)
        logger.info("WebMainWindow 初始化完成, backend_port=%d", backend_port)

    def _configure_web_engine(self) -> None:
        """
        Business Logic:
            前端需要 LocalStorage 持久化主题/设置；dev 模式需要 DevTools。
        """
        settings: QWebEngineSettings = self._web.settings()  # type: ignore[assignment]
        settings.setAttribute(QWebEngineSettings.WebAttribute.LocalStorageEnabled, True)
        settings.setAttribute(QWebEngineSettings.WebAttribute.JavascriptEnabled, True)
        settings.setAttribute(QWebEngineSettings.WebAttribute.LocalContentCanAccessRemoteUrls, True)

    def open_devtools(self) -> None:
        """开发用：打开 Chrome DevTools"""
        try:
            from PyQt6.QtWebEngineCore import QWebEnginePage
            page = self._web.page()
            if page is not None:
                page.triggerAction(QWebEnginePage.WebAction.InspectElement)
        except Exception as e:
            logger.debug("DevTools 触发失败: %s", e)

    def closeEvent(self, a0: QCloseEvent | None) -> None:  # noqa: N802
        """
        Business Logic:
            关闭窗口时只隐藏，最小化到托盘，不退出应用进程。
            由托盘菜单"退出"或 app.py 的 quit() 真正终止。
        """
        if a0 is None:
            return
        if hasattr(self, "_allow_close") and self._allow_close:
            a0.accept()
            return
        a0.ignore()
        self.hide()
        logger.debug("主窗口已隐藏到托盘")

    def force_close(self) -> None:
        """应用退出时强制关闭窗口"""
        self._allow_close = True
        self.close()

    @property
    def web_view(self) -> QWebEngineView:
        """暴露给外部（系统托盘、快捷键）使用"""
        return self._web
