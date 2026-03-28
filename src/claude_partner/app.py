# -*- coding: utf-8 -*-
"""应用入口：负责初始化所有子系统并管理应用生命周期。"""

from __future__ import annotations

import asyncio
import logging
import signal
import sys
from typing import NoReturn

from PyQt6.QtWidgets import QApplication
from PyQt6.QtCore import Qt
import qasync

from claude_partner.config import AppConfig
from claude_partner.storage.database import Database
from claude_partner.storage.prompt_repo import PromptRepository
from claude_partner.network.discovery import DeviceDiscovery
from claude_partner.network.server import HTTPServer
from claude_partner.network.protocol import APIProtocol
from claude_partner.network.client import PeerClient
from claude_partner.sync.engine import SyncEngine
from claude_partner.transfer.sender import FileSender
from claude_partner.transfer.receiver import FileReceiver
from claude_partner.screenshot.capture import ScreenshotManager
from claude_partner.ui.main_window import MainWindow
from claude_partner.ui.prompt_panel import PromptPanel
from claude_partner.ui.transfer_panel import TransferPanel
from claude_partner.ui.device_panel import DevicePanel
from claude_partner.ui.tray import SystemTray
from claude_partner.ui.settings_panel import SettingsPanel
from claude_partner.ui import theme
from claude_partner.hotkey.listener import GlobalHotkeyManager

logger: logging.Logger = logging.getLogger(__name__)


class Application:
    """
    应用核心类，负责初始化和管理所有子系统的生命周期。

    Business Logic（为什么需要这个类）:
        Claude Partner 包含多个子系统（数据库、网络、同步、UI 等），
        需要一个统一的入口来按正确顺序初始化它们，并在退出时反向清理资源。

    Code Logic（这个类做什么）:
        启动顺序：配置加载 → 数据库初始化 → 网络客户端/接收器 → HTTP 服务端 →
        mDNS 注册 → 同步引擎 → UI 组件 → 系统托盘。
        关闭时反向停止：同步引擎 → mDNS → HTTP 服务 → 数据库。
        使用 qasync 将 asyncio 事件循环集成到 Qt 事件循环中。
    """

    def __init__(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化应用对象，占位所有子系统引用。

        Code Logic（这个函数做什么）:
            声明所有子系统的类型占位符为 None，实际初始化在 start() 中执行。
        """
        self._config: AppConfig | None = None
        self._database: Database | None = None
        self._prompt_repo: PromptRepository | None = None
        self._peer_client: PeerClient | None = None
        self._file_sender: FileSender | None = None
        self._file_receiver: FileReceiver | None = None
        self._protocol: APIProtocol | None = None
        self._http_server: HTTPServer | None = None
        self._discovery: DeviceDiscovery | None = None
        self._sync_engine: SyncEngine | None = None
        self._screenshot_mgr: ScreenshotManager | None = None
        self._main_window: MainWindow | None = None
        self._system_tray: SystemTray | None = None
        self._hotkey_mgr: GlobalHotkeyManager | None = None
        self._settings_panel: SettingsPanel | None = None

    async def start(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用启动时需要按依赖顺序初始化所有子系统，
            确保数据库在存储层之前就绪，网络在同步之前就绪。

        Code Logic（这个函数做什么）:
            按顺序初始化：config → database → repos → network →
            transfer → protocol → http server → discovery → sync →
            screenshot → UI → tray，并连接各组件间的信号。
        """
        # 1. 配置
        self._config = AppConfig.load()
        logger.info(
            "配置加载完成: device_id=%s, device_name=%s",
            self._config.device_id,
            self._config.device_name,
        )

        # 2. 数据库
        self._database = Database(self._config.db_path)
        await self._database.initialize()
        logger.info("数据库初始化完成")

        # 3. 存储层
        self._prompt_repo = PromptRepository(self._database)

        # 4. 网络客户端
        self._peer_client = PeerClient()

        # 5. 文件传输
        self._file_sender = FileSender(self._peer_client)
        self._file_receiver = FileReceiver(self._config)

        # 6. API 协议（注册传输回调）
        self._protocol = APIProtocol(
            config=self._config,
            prompt_repo=self._prompt_repo,
            on_transfer_init=self._file_receiver.init_transfer,
            on_transfer_chunk=self._file_receiver.receive_chunk,
            get_transfer_status=self._file_receiver.get_transfer_status,
        )

        # 7. HTTP 服务端
        self._http_server = HTTPServer(self._protocol)
        actual_port: int = await self._http_server.start(self._config.http_port)
        logger.info("HTTP 服务端启动在端口 %d", actual_port)

        # 8. mDNS 设备发现（在独立线程+独立事件循环中运行，不阻塞主线程）
        self._discovery = DeviceDiscovery(self._config)
        self._discovery.start(actual_port)

        # 9. 同步引擎
        self._sync_engine = SyncEngine(
            self._config, self._prompt_repo, self._peer_client
        )
        await self._sync_engine.start_periodic_sync(self._discovery.get_devices)

        # 10. 截图管理器
        self._screenshot_mgr = ScreenshotManager()
        logger.info("截图管理器创建完成")

        # 11. 全局快捷键
        self._hotkey_mgr = GlobalHotkeyManager(
            {"screenshot": self._config.screenshot_hotkey}
        )
        self._hotkey_mgr.start()
        logger.info("全局快捷键启动完成")

        # 12. UI 组件
        try:
            prompt_panel = PromptPanel(self._prompt_repo, self._config)
            logger.info("PromptPanel 创建完成")
            transfer_panel = TransferPanel(self._file_sender, self._file_receiver)
            logger.info("TransferPanel 创建完成")
            device_panel = DevicePanel()
            logger.info("DevicePanel 创建完成")
            self._settings_panel = SettingsPanel(self._config)
            logger.info("SettingsPanel 创建完成")

            self._main_window = MainWindow(
                prompt_panel=prompt_panel,
                transfer_panel=transfer_panel,
                device_panel=device_panel,
                settings_panel=self._settings_panel,
            )
            logger.info("MainWindow 创建完成")
        except Exception as e:
            logger.error("UI 创建失败: %s", e, exc_info=True)
            raise

        # 12. 系统托盘
        try:
            self._system_tray = SystemTray()
            self._system_tray.show()
            logger.info("系统托盘创建完成")
        except Exception as e:
            logger.error("系统托盘创建失败: %s", e, exc_info=True)
            # 托盘失败不阻止应用启动
            self._system_tray = None

        # 连接信号
        self._connect_signals(prompt_panel, transfer_panel, device_panel)

        # 显示主窗口并加载数据
        self._main_window.show()
        asyncio.ensure_future(prompt_panel.refresh())

        logger.info("应用启动完成")

    def _connect_signals(
        self,
        prompt_panel: PromptPanel,
        transfer_panel: TransferPanel,
        device_panel: DevicePanel,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            各子系统间需要通过信号连接协作，如设备发现通知 UI 更新，
            Prompt 变更触发同步等。

        Code Logic（这个函数做什么）:
            连接 DeviceDiscovery → DevicePanel/TransferPanel/SyncEngine，
            PromptPanel → SyncEngine，SystemTray → MainWindow/ScreenshotManager 等信号。
        """
        assert self._discovery is not None
        assert self._sync_engine is not None
        assert self._screenshot_mgr is not None
        assert self._system_tray is not None
        assert self._main_window is not None

        # 设备发现 → UI + 同步
        self._discovery.device_found.connect(device_panel.add_device)
        self._discovery.device_found.connect(
            lambda dev: transfer_panel.update_devices(self._discovery.get_devices())
        )
        self._discovery.device_found.connect(
            lambda dev: self._system_tray.update_device_count(
                len(self._discovery.get_devices())
            )
        )
        self._discovery.device_found.connect(
            lambda dev: asyncio.ensure_future(
                self._sync_engine.sync_with_peer(dev)
            )
        )
        self._discovery.device_lost.connect(device_panel.remove_device)
        self._discovery.device_lost.connect(
            lambda _: transfer_panel.update_devices(self._discovery.get_devices())
        )
        self._discovery.device_lost.connect(
            lambda _: self._system_tray.update_device_count(
                len(self._discovery.get_devices())
            )
        )

        # Prompt 变更 → 同步
        prompt_panel.prompt_changed.connect(
            lambda prompt: asyncio.ensure_future(
                self._sync_engine.on_local_change(
                    prompt, self._discovery.get_devices()
                )
            )
        )

        # 同步完成 → 刷新 UI
        self._sync_engine.sync_completed.connect(
            lambda: asyncio.ensure_future(prompt_panel.refresh())
        )

        # 系统托盘
        self._system_tray.show_window_requested.connect(self._show_main_window)
        self._system_tray.screenshot_requested.connect(
            self._screenshot_mgr.take_screenshot
        )
        self._system_tray.quit_requested.connect(self._quit)

        # 全局快捷键 → 截图
        if self._hotkey_mgr is not None:
            self._hotkey_mgr.hotkey_triggered.connect(self._on_hotkey)

        # 设置变更 → 更新快捷键和配置
        if self._settings_panel is not None:
            self._settings_panel.settings_changed.connect(self._on_settings_changed)

    def _check_macos_permissions(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            macOS 要求应用分别获得「屏幕录制」和「输入监控」权限，
            打包后的 .app 与 Terminal 是不同的应用身份，需要单独授权。
            缺失权限时截图只能捕获桌面壁纸、全局快捷键完全无效。

        Code Logic（这个函数做什么）:
            仅在 PyInstaller 打包的 frozen app 中执行（从 Terminal
            运行时权限检测 API 会误报）。
            1. 屏幕录制: CGRequestScreenCaptureAccess 直接打开系统设置页面
            2. 输入监控: 无请求 API，通过 URL scheme 直接打开系统设置的
               输入监控页面，让用户手动启用开关
            3. 任一权限缺失时提示用户授权后重启
        """
        # Terminal 运行时权限检测 API 会误报，只在打包后的 .app 中检查
        if not getattr(sys, "frozen", False):
            return

        import subprocess

        requested: list[str] = []

        # 检测屏幕录制权限
        try:
            import Quartz  # type: ignore[import-untyped]
            if hasattr(Quartz, "CGPreflightScreenCaptureAccess"):
                if not Quartz.CGPreflightScreenCaptureAccess():
                    Quartz.CGRequestScreenCaptureAccess()
                    requested.append("屏幕录制")
                    logger.info("已请求 macOS 屏幕录制权限")
        except ImportError:
            pass

        # 检测输入监控权限（通过 CGEventTap 检测，无专用请求 API）
        try:
            import Quartz  # type: ignore[import-untyped]

            def _dummy(proxy: object, etype: int, event: object, ref: object) -> object:
                return event

            tap = Quartz.CGEventTapCreate(
                Quartz.kCGHIDEventTap,
                Quartz.kCGHeadInsertEventTap,
                Quartz.kCGEventTapOptionListenOnly,
                Quartz.CGEventMaskBit(Quartz.kCGEventKeyDown),
                _dummy,
                None,
            )
            if tap is None:
                # 输入监控无请求 API，直接打开系统设置对应页面
                subprocess.Popen([
                    "open",
                    "x-apple.systempreferences:"
                    "com.apple.preference.security?Privacy_ListenEvent",
                ])
                requested.append("输入监控")
                logger.info("已打开 macOS 输入监控设置页面")
            else:
                Quartz.CFMachPortInvalidate(tap)
        except ImportError:
            pass

        if requested:
            from PyQt6.QtWidgets import QMessageBox

            QMessageBox.information(
                self._main_window,
                "请授予权限",
                "已打开系统设置对应页面，请在列表中找到\n"
                "Claude Partner 并启用权限开关。\n\n"
                "授权完成后请重启应用。",
            )

    def _on_hotkey(self, action: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            全局快捷键触发时需要执行对应的动作。

        Code Logic（这个函数做什么）:
            根据动作名称分发到对应的处理函数。
        """
        logger.info("_on_hotkey 收到动作: %s", action)
        if action == "screenshot" and self._screenshot_mgr is not None:
            self._screenshot_mgr.take_screenshot()

    def _on_settings_changed(self, config: object) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户修改设置后需要实时更新运行中的子系统配置。

        Code Logic（这个函数做什么）:
            更新内部 config 引用，重新注册全局快捷键。
        """
        if isinstance(config, AppConfig):
            self._config = config
            if self._hotkey_mgr is not None:
                self._hotkey_mgr.update_hotkey(
                    "screenshot", config.screenshot_hotkey
                )

    def _show_main_window(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户通过托盘菜单或双击托盘图标时需要显示主窗口。

        Code Logic（这个函数做什么）:
            如果窗口已最小化则恢复，然后激活到前台。
        """
        if self._main_window is not None:
            self._main_window.showNormal()
            self._main_window.activateWindow()

    def _quit(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户通过托盘菜单选择退出时，需要触发应用关闭流程。

        Code Logic（这个函数做什么）:
            调用 QApplication.quit() 触发应用退出，
            实际清理在 shutdown() 中执行。
        """
        QApplication.quit()

    async def shutdown(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用退出时需要按反向顺序释放所有资源，
            避免数据丢失、端口占用和 mDNS 服务残留。

        Code Logic（这个函数做什么）:
            反向停止：同步引擎 → mDNS → HTTP 服务 → 网络客户端 → 数据库。
        """
        logger.info("应用关闭中...")

        if self._hotkey_mgr is not None:
            self._hotkey_mgr.stop()

        if self._sync_engine is not None:
            await self._sync_engine.stop()

        if self._discovery is not None:
            self._discovery.stop()

        if self._http_server is not None:
            await self._http_server.stop()

        if self._peer_client is not None:
            await self._peer_client.close()

        if self._database is not None:
            await self._database.close()

        logger.info("应用已关闭")


def main() -> None:
    """
    Business Logic（为什么需要这个函数）:
        作为 pyproject.toml 中定义的入口点，启动整个应用。

    Code Logic（这个函数做什么）:
        1. 配置日志系统
        2. 创建 QApplication
        3. 使用 qasync 将 asyncio 事件循环集成到 Qt
        4. 启动 Application，运行事件循环
        5. 退出时执行 shutdown 清理
    """
    # 配置日志
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # 创建 Qt 应用
    qt_app = QApplication(sys.argv)
    qt_app.setQuitOnLastWindowClosed(False)  # 关闭窗口不退出，由托盘管理
    qt_app.setStyleSheet(theme.get_global_stylesheet())  # Apple 风格全局样式

    # 使用 qasync 事件循环
    loop = qasync.QEventLoop(qt_app)
    asyncio.set_event_loop(loop)

    app = Application()

    async def _run() -> None:
        """启动应用并等待退出。"""
        await app.start()

    async def _cleanup() -> None:
        """退出时清理资源。"""
        await app.shutdown()

    # SIGTERM 处理：macOS Dock 退出和 kill 命令发送 SIGTERM
    # 使用 socketpair + QSocketNotifier 确保信号能在任何状态下被 Qt 事件循环处理
    # （纯 signal.signal 在 C++ 模态对话框或 PyInstaller 中可能失效）
    import socket
    from PyQt6.QtCore import QSocketNotifier

    _sig_r, _sig_w = socket.socketpair(type=socket.SOCK_STREAM)
    _sig_r.setblocking(False)
    _sig_w.setblocking(False)

    def _sigterm_handler(*_: object) -> None:
        """信号处理器：只写一个字节，由 Qt 事件循环安全处理退出。"""
        try:
            _sig_w.send(b"\x00")
        except OSError:
            pass

    signal.signal(signal.SIGTERM, _sigterm_handler)
    signal.signal(signal.SIGINT, _sigterm_handler)

    _sig_notifier = QSocketNotifier(_sig_r.fileno(), QSocketNotifier.Type.Read)
    _sig_notifier.activated.connect(lambda: (logger.info("收到退出信号"), QApplication.quit()))

    # macOS 权限检查：必须在 run_forever 中执行，不能在 run_until_complete 中，
    # 因为 QMessageBox 模态对话框会触发嵌套事件循环导致 qasync 状态损坏
    if sys.platform == "darwin":
        from PyQt6.QtCore import QTimer
        QTimer.singleShot(500, app._check_macos_permissions)

    try:
        loop.run_until_complete(_run())
        loop.run_forever()
    except KeyboardInterrupt:
        logger.info("收到 Ctrl+C，退出...")
    finally:
        loop.run_until_complete(_cleanup())
        loop.close()


if __name__ == "__main__":
    main()
