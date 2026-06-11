# -*- coding: utf-8 -*-
"""应用入口：负责初始化所有子系统并管理应用生命周期。"""

from __future__ import annotations

import asyncio
import logging
import os
import signal
import sys
from pathlib import Path
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
from claude_partner.ui.web_main_window import WebMainWindow
from claude_partner.ui.tray import SystemTray
from claude_partner.ui import theme
from claude_partner.hotkey.listener import GlobalHotkeyManager
from claude_partner import __version__

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
        self._main_window: WebMainWindow | None = None
        self._system_tray: SystemTray | None = None
        self._hotkey_mgr: GlobalHotkeyManager | None = None

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

        # 6. API 协议（注册传输回调 + device getter + 配置/更新/权限回调）
        self._protocol = APIProtocol(
            config=self._config,
            prompt_repo=self._prompt_repo,
            on_transfer_init=self._file_receiver.init_transfer,
            on_transfer_chunk=self._file_receiver.receive_chunk,
            get_transfer_status=self._file_receiver.get_transfer_status,
            get_devices=self._get_devices_for_api,
            on_transfer_send=self._file_sender.send_file,
            on_transfer_cancel=self._cancel_transfer,
            get_transfers=self._get_transfers_for_api,
            on_choose_dir=self._choose_dir,
            on_check_update=self._check_update,
            check_permissions=self._check_permissions_status,
        )

        # 7. HTTP 服务端
        self._http_server = HTTPServer(self._protocol)

        # 7.1 挂载前端静态资源（必须在 start() 之前调用，因为 runner.setup() 后路由冻结）
        if getattr(sys, "frozen", False):
            web_dir: Path = Path(getattr(sys, "_MEIPASS", ".")) / "web" / "dist"
        else:
            web_dir = Path(__file__).resolve().parents[2] / "web" / "dist"
        self._http_server.serve_static(web_dir)

        actual_port: int = await self._http_server.start(self._config.http_port)
        logger.info("HTTP 服务端启动在端口 %d", actual_port)

        # 7.2 将实际端口同步到 API 协议层（health/config 端点返回真实端口）
        assert self._protocol is not None
        self._protocol.set_actual_port(actual_port)

        # 8. mDNS 设备发现（在独立线程+独立事件循环中运行，不阻塞主线程）
        self._discovery = DeviceDiscovery(self._config)
        self._discovery.start(actual_port)

        # 9. 同步引擎
        self._sync_engine = SyncEngine(
            self._config, self._prompt_repo, self._peer_client
        )

        # 10. 截图管理器
        self._screenshot_mgr = ScreenshotManager()
        logger.info("截图管理器创建完成")

        # 11. 全局快捷键
        self._hotkey_mgr = GlobalHotkeyManager(
            {"screenshot": self._config.screenshot_hotkey}
        )
        self._hotkey_mgr.start()
        logger.info("全局快捷键启动完成")

        # 12. 主窗口（QWebEngineView 嵌入 React 前端）
        try:
            self._main_window = WebMainWindow(backend_port=actual_port)
            self._main_window.show()
            logger.info("WebMainWindow 创建完成（QWebEngineView + React）")
        except Exception as e:
            logger.error("主窗口创建失败: %s", e, exc_info=True)
            raise

        # 13. 系统托盘
        try:
            self._system_tray = SystemTray()
            self._system_tray.show()
            logger.info("系统托盘创建完成")
        except Exception as e:
            logger.error("系统托盘创建失败: %s", e, exc_info=True)
            # 托盘失败不阻止应用启动
            self._system_tray = None

        # 14. 连接托盘与全局信号（始终生效）
        self._connect_global_signals()

        logger.info("应用启动完成")

    def _get_devices_for_api(self) -> list[dict]:
        """
        Business Logic（为什么需要这个函数）:
            前端设备面板通过 /api/devices 拉取设备列表。
            需要把 DeviceDiscovery 内部的 Device 对象转换为前端
            web/src/lib/types.ts Device 约定的字段（address / status / lastSeen）。

        Code Logic（这个函数做什么）:
            从 discovery.get_devices() 拿全部设备，转为 dict 列表。
            status 由 online 字段映射（True → 'online'，False → 'offline'）。
        """
        if self._discovery is None:
            return []
        devices: list[dict] = []
        for d in self._discovery.get_devices().values():
            devices.append({
                "id": d.id,
                "name": d.name,
                "address": d.host,
                "port": d.port,
                "status": "online" if d.online else "offline",
                "lastSeen": d.last_seen.isoformat(),
            })
        return devices

    def _cancel_transfer(self, transfer_id: str) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            前端传输面板的"取消"按钮调 /api/transfer/tasks/{id} DELETE。
            需要同时检查发送端和接收端，因为同一个 transfer_id 可能存在
            在其中一侧（取决于 direction）。

        Code Logic（这个函数做什么）:
            优先查 sender，再查 receiver。任一命中即调用其 cancel()。
            返回是否成功找到并取消。
        """
        if self._file_sender is not None and self._file_sender.get_task(transfer_id) is not None:
            self._file_sender.cancel(transfer_id)
            return True
        if self._file_receiver is not None and self._file_receiver.get_task(transfer_id) is not None:
            self._file_receiver.cancel(transfer_id)
            return True
        return False

    def _choose_dir(self) -> str:
        """
        Business Logic:
            前端设置页面修改"文件接收目录"时，需要打开系统原生
            目录选择对话框，让用户通过可视化界面选择路径。

        Code Logic:
            使用 QFileDialog.getExistingDirectory 弹出目录选择对话框。
            返回用户选择的路径字符串，取消时返回空字符串。
        """
        from PyQt6.QtWidgets import QFileDialog
        path: str = QFileDialog.getExistingDirectory(
            None,
            "选择文件接收目录",
            self._config.receive_dir if self._config else "",
        )
        return path or ""

    async def _check_update(self) -> dict:
        """
        Business Logic:
            前端设置页面用户点击"检查更新"时，需要执行版本检查，
            并将检查结果返回给 API 层响应给前端。

        Code Logic:
            手动调用 GitHub Releases API 获取最新版本，
            与本地 __version__ 语义化比较后构造返回字典：{hasUpdate, version, body}。
        """
        import aiohttp
        from claude_partner.updater.checker import (
            RELEASES_LATEST_URL,
            RELEASE_API_URL,
            SemanticVersion,
            _GITHUB_HEADERS,
        )
        try:
            timeout: aiohttp.ClientTimeout = aiohttp.ClientTimeout(total=15)
            async with aiohttp.ClientSession(timeout=timeout) as session:
                # 通过重定向获取最新 tag
                tag_name: str = ""
                async with session.get(
                    RELEASES_LATEST_URL, allow_redirects=False
                ) as resp:
                    if resp.status == 302:
                        location: str = resp.headers.get("Location", "")
                        tag_name = location.rstrip("/").split("/")[-1]
                    elif resp.status == 200:
                        tag_name = str(resp.url).rstrip("/").split("/")[-1]

                if not tag_name:
                    return {"hasUpdate": False, "version": "", "body": ""}

                remote_version: SemanticVersion = SemanticVersion.parse(tag_name)
                local_version: SemanticVersion = SemanticVersion.parse(__version__)

                if not (remote_version > local_version):
                    return {"hasUpdate": False, "version": "", "body": ""}

                # 有新版本，获取详情
                async with session.get(
                    RELEASE_API_URL, headers=_GITHUB_HEADERS
                ) as resp:
                    if resp.status != 200:
                        return {
                            "hasUpdate": True,
                            "version": tag_name.lstrip("v"),
                            "body": "",
                        }
                    data: dict = await resp.json()
                    return {
                        "hasUpdate": True,
                        "version": tag_name.lstrip("v"),
                        "body": data.get("body", ""),
                    }
        except Exception as e:
            logger.error("更新检查失败: %s", e, exc_info=True)
            return {"hasUpdate": False, "version": "", "body": "", "error": str(e)}

    @staticmethod
    def _check_permissions_status() -> dict:
        """
        Business Logic:
            前端设置页面需要展示 macOS 权限状态（屏幕录制、输入监控），
            让用户了解当前哪些功能可能因权限缺失而不可用。

        Code Logic:
            调用 permissions.py 中的 check_screen_capture_access 和
            check_input_monitoring_access 函数，返回 camelCase 字典。
            非 macOS 打包环境直接返回已授权。
        """
        from claude_partner.ui.permissions import (
            check_screen_capture_access,
            check_input_monitoring_access,
        )
        return {
            "screenCapture": {"granted": check_screen_capture_access()},
            "inputMonitoring": {"granted": check_input_monitoring_access()},
        }

    def _get_transfers_for_api(self) -> list[dict]:
        """
        Business Logic:
            前端传输面板通过 /api/transfer/tasks 拉取全部传输任务列表。
            需要合并发送端和接收端的任务，并转换为前端 camelCase 格式。

        Code Logic:
            合并 sender.list_tasks() + receiver.list_tasks()，
            用 APIProtocol._transfer_to_frontend_dict 做字段名转换。
        """
        from claude_partner.network.protocol import APIProtocol as Proto
        tasks: list = []
        if self._file_sender is not None:
            tasks.extend(
                Proto._transfer_to_frontend_dict(t)
                for t in self._file_sender.list_tasks()
            )
        if self._file_receiver is not None:
            tasks.extend(
                Proto._transfer_to_frontend_dict(t)
                for t in self._file_receiver.list_tasks()
            )
        # 按 startedAt 倒序
        tasks.sort(key=lambda t: t.get("startedAt", ""), reverse=True)
        return tasks

    def _connect_global_signals(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            系统托盘、设备发现、全局快捷键需要通过信号连接协作：
            托盘菜单控制窗口显隐与截图，设备发现更新托盘在线计数，
            快捷键触发截图。界面交互（列表/筛选/同步）由 React 前端
            通过 HTTP API 自行处理，无需此处连接。

        Code Logic（这个函数做什么）:
            连接 SystemTray → 显示窗口/截图/退出，DeviceDiscovery →
            托盘在线计数，GlobalHotkeyManager → 截图动作。
        """
        assert self._screenshot_mgr is not None
        screenshot_mgr = self._screenshot_mgr

        # 系统托盘 → 窗口/截图/退出
        if self._system_tray is not None:
            self._system_tray.show_window_requested.connect(self._show_main_window)
            self._system_tray.screenshot_requested.connect(
                screenshot_mgr.take_screenshot
            )
            self._system_tray.quit_requested.connect(self._quit)

        # 设备发现 → 托盘在线计数
        if self._discovery is not None and self._system_tray is not None:
            discovery = self._discovery
            system_tray = self._system_tray
            discovery.device_found.connect(
                lambda _dev: system_tray.update_device_count(
                    len(discovery.get_devices())
                )
            )
            discovery.device_lost.connect(
                lambda _dev: system_tray.update_device_count(
                    len(discovery.get_devices())
                )
            )

        # 全局快捷键 → 截图
        if self._hotkey_mgr is not None:
            self._hotkey_mgr.hotkey_triggered.connect(self._on_hotkey)

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
            停止 asyncio 事件循环使 run_forever() 返回，进入 finally 块
            执行异步清理。不能直接调用 QApplication.quit()，否则 Qt 事件
            循环被销毁后 qasync 的 run_until_complete(_cleanup()) 会崩溃。
        """
        loop = asyncio.get_event_loop()
        loop.stop()

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


def _fix_linux_input_method() -> None:
    """
    Business Logic（为什么需要这个函数）:
        Linux 上用户使用 fcitx 等输入法框架时，PyQt6 自带的 Qt6 可能不包含
        对应的输入法插件（如 fcitx），导致无法输入中文。
        需要在 QApplication 创建前自动检测并切换到可用的输入法方案。

    Code Logic（这个函数做什么）:
        检查当前 QT_IM_MODULE 指定的输入法插件是否存在于 PyQt6 的 Qt6
        plugins 目录中。如果不存在，尝试启动 ibus-daemon 并切换到 ibus 输入法，
        因为 PyQt6 自带 ibus 插件且多数发行版预装 ibus。
        必须在 QApplication 创建前调用。
    """
    if sys.platform != "linux":
        return

    im_module: str = os.environ.get("QT_IM_MODULE", "")
    if not im_module or im_module in ("ibus", "xim", "compose"):
        return

    # 检查 PyQt6 的 Qt6 插件目录中是否有匹配的输入法插件
    pyqt6_plugins_dir: str = os.path.join(
        os.path.dirname(
            __import__("PyQt6").__file__
        ),
        "Qt6", "plugins", "platforminputcontexts",
    )
    if not os.path.isdir(pyqt6_plugins_dir):
        return

    # 查找与 QT_IM_MODULE 匹配的 .so 插件（文件名含对应关键字）
    import glob
    pattern: str = os.path.join(pyqt6_plugins_dir, f"*{im_module}*.so")
    if glob.glob(pattern):
        return  # 插件存在，无需处理

    # 尝试 ibus 回退：PyQt6 自带 ibus 插件，多数发行版预装 ibus
    ibus_plugin: list[str] = glob.glob(
        os.path.join(pyqt6_plugins_dir, "*ibus*.so")
    )
    if not ibus_plugin:
        logger.warning(
            "PyQt6 的 Qt6 缺少 %s 和 ibus 输入法插件，无法修复中文输入",
            im_module,
        )
        return

    # 确保 ibus-daemon 正在运行
    import subprocess
    try:
        result: subprocess.CompletedProcess[bytes] = subprocess.run(
            ["ibus", "list-engine"],
            capture_output=True,
            timeout=3,
        )
        if result.returncode != 0:
            # ibus-daemon 未运行，尝试启动
            subprocess.Popen(
                ["ibus-daemon", "--daemonize"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            import time
            time.sleep(0.5)
            logger.info("已启动 ibus-daemon")
    except FileNotFoundError:
        logger.warning("ibus 未安装，无法修复中文输入")
        return
    except Exception as e:
        logger.warning("启动 ibus-daemon 失败: %s", e)
        return

    os.environ["QT_IM_MODULE"] = "ibus"
    logger.info(
        "PyQt6 的 Qt6 缺少 %s 插件，已切换到 ibus 输入法 (QT_IM_MODULE=ibus)",
        im_module,
    )


def _fix_macos_qt_issues() -> None:
    """
    Business Logic（为什么需要这个函数）:
        macOS 上存在多个与输入法切换相关的崩溃问题：
        1. pynput 的 CGEventTap 回调在后台线程处理 Caps Lock 事件时，
           调用 NSEvent.eventWithCGEvent: 触发 macOS 输入法切换
           (TSMCreateInputSourceForRomanSwitchAction)，该 API 要求主线程，
           导致 dispatch_assert_queue_fail 崩溃
        2. conda/pip 混合环境下 cocoa 平台插件可能无法被发现

    Code Logic（这个函数做什么）:
        1. Monkey-patch pynput 的 _handle_message，在 NSEvent 创建前
           过滤掉 Caps Lock（keycode 57）事件
        2. 如果 cocoa 插件路径未配置，自动设置 QT_QPA_PLATFORM_PLUGIN_PATH
        必须在 QApplication 创建前、pynput Listener 启动前调用。
    """
    if sys.platform != "darwin":
        return

    # 核心修复：monkey-patch pynput，防止 Caps Lock 在后台线程触发输入法崩溃
    _patch_pynput_caps_lock_filter()

    # 修复 conda 环境下 cocoa 插件无法发现的问题
    if not os.environ.get("QT_QPA_PLATFORM_PLUGIN_PATH"):
        try:
            import PyQt6
            plugins_dir: str = os.path.join(
                os.path.dirname(PyQt6.__file__),
                "Qt6", "plugins", "platforms",
            )
            if os.path.isdir(plugins_dir):
                os.environ["QT_QPA_PLATFORM_PLUGIN_PATH"] = plugins_dir
        except Exception:
            pass


def _patch_pynput_caps_lock_filter() -> None:
    """
    Business Logic（为什么需要这个函数）:
        macOS 上按 Caps Lock 切换「中/英」输入法时，pynput 的 CGEventTap
        回调会在后台线程调用 NSEvent.eventWithCGEvent:，触发
        TSMCreateInputSourceForRomanSwitchAction → dispatch_assert_queue_fail 崩溃。
        这是 pynput 的 bug，无法通过 on_press 回调过滤（崩溃在回调之前）。

    Code Logic（这个函数做什么）:
        monkey-patch pynput.keyboard._darwin.Listener._handle_message，
        在事件处理的最早期检查 keycode 是否为 57（Caps Lock），
        是则直接返回，不调用原始 _handle_message，从而避免
        NSEvent.eventWithCGEvent: 在非主线程执行。
    """
    try:
        from pynput.keyboard import _darwin
        from Quartz import (
            CGEventGetIntegerValueField,
            kCGKeyboardEventKeycode,
            kCGEventFlagsChanged,
            NSSystemDefined,
        )

        _original = _darwin.Listener._handle_message
        _CAPS_LOCK_KEYCODE: int = 0x39  # macOS Caps Lock virtual keycode

        def _safe_handle_message(
            self: object,
            _proxy: object,
            event_type: int,
            event: object,
            _refcon: object,
            injected: bool,
        ) -> None:
            """
            Business Logic:
                macOS 按 Caps Lock 切换「中/英」时，会同时产生两种 CGEvent：
                1. kCGEventFlagsChanged (keycode=57) — 已被 Caps Lock 分支处理
                2. kCGEventSystemDefined — 进入 NSSystemDefined 分支，
                   调用 NSEvent.eventWithCGEvent: 触发输入法切换，
                   在 CGEventTap 后台线程上导致 dispatch_assert_queue_fail。
                两种都必须在 _handle_message 之前过滤。

            Code Logic:
                - kCGEventFlagsChanged 且 keycode=57 → 跳过
                - kCGEventSystemDefined → 跳过（我们不使用媒体键热键）
                - 其他事件正常传递
            """
            try:
                if event_type == kCGEventFlagsChanged:
                    vk: int = CGEventGetIntegerValueField(
                        event, kCGKeyboardEventKeycode
                    )
                    if vk == _CAPS_LOCK_KEYCODE:
                        return
                elif event_type == NSSystemDefined:
                    return
            except Exception:
                pass
            _original(
                self, _proxy, event_type, event, _refcon, injected
            )

        _darwin.Listener._handle_message = _safe_handle_message
        logger.info("已 patch pynput Caps Lock 过滤器，防止输入法切换崩溃")

    except ImportError:
        logger.debug("pynput keyboard darwin 模块不可用，跳过 Caps Lock patch")
    except Exception as e:
        logger.warning("pynput Caps Lock patch 失败: %s", e)


def main() -> None:
    """
    Business Logic（为什么需要这个函数）:
        作为 pyproject.toml 中定义的入口点，启动整个应用。

    Code Logic（这个函数做什么）:
        1. 配置日志系统和崩溃诊断
        2. 修复 macOS Qt 6.10 输入法崩溃问题
        3. 修复 Linux 输入法兼容性
        4. 创建 QApplication
        5. 使用 qasync 将 asyncio 事件循环集成到 Qt
        6. 启动 Application，运行事件循环
        7. 退出时执行 shutdown 清理
    """
    # 崩溃诊断：segfault 时输出 C 调用栈
    import faulthandler
    faulthandler.enable()

    # 配置日志
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # macOS Qt 6.10 输入法崩溃修复（必须在 QApplication 创建前执行）
    _fix_macos_qt_issues()

    # Linux 输入法兼容性修复（必须在 QApplication 创建前执行）
    _fix_linux_input_method()

    # 创建 Qt 应用
    qt_app = QApplication(sys.argv)
    qt_app.setQuitOnLastWindowClosed(False)  # 关闭窗口不退出，由托盘管理
    qt_app.setStyle(theme.create_no_focus_style())  # 全局禁用焦点虚线框

    # 检测系统深色模式并应用主题
    style_hints = qt_app.styleHints()
    initial_dark: bool = (
        style_hints is not None
        and style_hints.colorScheme() == Qt.ColorScheme.Dark
    )
    qt_app.setStyleSheet(theme.apply_theme(initial_dark))

    # 使用 qasync 事件循环
    loop = qasync.QEventLoop(qt_app)
    asyncio.set_event_loop(loop)

    app = Application()

    async def _run() -> None:
        """启动应用并等待退出。"""
        await app.start()

        # 系统主题切换时重新应用样式
        def _on_color_scheme_changed(scheme: Qt.ColorScheme) -> None:
            """
            Business Logic（为什么需要这个函数）:
                用户切换系统深浅色主题时，应用需要实时跟随变化。

            Code Logic（这个函数做什么）:
                根据 colorScheme 值切换 theme 调色板，重新应用 Qt 全局样式表。
                前端 React 通过 CSS prefers-color-scheme 自动适配，无需后端通知。
            """
            dark: bool = scheme == Qt.ColorScheme.Dark
            qt_app.setStyleSheet(theme.apply_theme(dark))

        if style_hints is not None:
            style_hints.colorSchemeChanged.connect(_on_color_scheme_changed)

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

    def _on_exit_signal() -> None:
        """
        Business Logic（为什么需要这个函数）:
            收到退出信号后需要干净地停止事件循环，且只停一次。

        Code Logic（这个函数做什么）:
            读取 socket 数据防止 QSocketNotifier 重复触发，
            禁用 notifier，然后停止 asyncio 事件循环。
        """
        try:
            _sig_r.recv(4096)
        except OSError:
            pass
        _sig_notifier.setEnabled(False)
        logger.info("收到退出信号")
        loop.stop()

    _sig_notifier.activated.connect(_on_exit_signal)

    try:
        loop.run_until_complete(_run())
        loop.run_forever()
    except KeyboardInterrupt:
        logger.info("收到 Ctrl+C，退出...")
    finally:
        loop.run_until_complete(_cleanup())
        loop.close()
        # 从 Finder/Dock 启动的 .app 在 macOS 上，NSApplication 主循环
        # 不会因为 asyncio loop.stop() 自动退出，必须显式 quit Qt，
        # 否则 Python 进程会一直挂住。从终端启动时父进程是 shell，
        # 表现不出来；只有 Finder 启动才暴露。
        qt_app.quit()
        # 兜底：确保进程退出（避免 Qt/NSApp 残留线程让解释器无法 finalize）
        os._exit(0)


if __name__ == "__main__":
    main()
