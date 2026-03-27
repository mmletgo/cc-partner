# -*- coding: utf-8 -*-
"""mDNS 设备发现模块：通过 zeroconf 在局域网自动发现和注册设备。"""

from zeroconf import Zeroconf, ServiceBrowser, ServiceInfo, ServiceStateChange
from PyQt6.QtCore import QObject, pyqtSignal
from claude_partner.models.device import Device
from claude_partner.config import AppConfig
import asyncio
import socket
import logging
import threading
from datetime import datetime

logger: logging.Logger = logging.getLogger(__name__)

SERVICE_TYPE: str = "_claude-partner._tcp.local."


class DeviceDiscovery(QObject):
    """
    mDNS 服务注册和发现，管理局域网中对端设备的生命周期。

    Business Logic（为什么需要这个类）:
        P2P 局域网协作需要自动发现同一网络中的其他 Claude Partner 实例，
        无需用户手动输入 IP 地址和端口。通过 mDNS 协议实现零配置网络发现。

    Code Logic（这个类做什么）:
        整个 Zeroconf 运行在独立的后台线程和独立的 asyncio 事件循环中，
        避免与主线程的 qasync 事件循环冲突。
        通过 Qt 信号（线程安全）将设备发现/丢失事件通知 UI 层。
    """

    device_found = pyqtSignal(object)  # 发现新设备，传 Device 对象
    device_lost = pyqtSignal(str)  # 设备离线，传 device_id

    def __init__(self, config: AppConfig) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化设备发现服务，需要配置中的设备 ID 和名称来注册本机服务。

        Code Logic（这个函数做什么）:
            保存配置引用，初始化内部状态。
        """
        super().__init__()
        self._config: AppConfig = config
        self._devices: dict[str, Device] = {}
        self._zeroconf: Zeroconf | None = None
        self._browser: ServiceBrowser | None = None
        self._service_info: ServiceInfo | None = None
        self._thread: threading.Thread | None = None
        self._mdns_loop: asyncio.AbstractEventLoop | None = None

    def start(self, port: int) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用启动后需要在局域网中注册自己的存在，并开始监听其他设备。

        Code Logic（这个函数做什么）:
            在独立后台线程中创建 Zeroconf（带独立事件循环），
            注册本机服务并启动浏览。不阻塞主线程。
        """
        local_ip: str = self._get_local_ip()
        logger.info("本机 IP: %s, 端口: %d", local_ip, port)

        properties: dict[bytes, bytes] = {
            b"device_id": self._config.device_id.encode("utf-8"),
            b"device_name": self._config.device_name.encode("utf-8"),
        }

        self._service_info = ServiceInfo(
            type_=SERVICE_TYPE,
            name=f"{self._config.device_id}.{SERVICE_TYPE}",
            addresses=[socket.inet_aton(local_ip)],
            port=port,
            properties=properties,
        )

        self._thread = threading.Thread(
            target=self._run_mdns, args=(port,), daemon=True
        )
        self._thread.start()

    def _run_mdns(self, port: int) -> None:
        """
        Business Logic（为什么需要这个函数）:
            zeroconf 内部依赖 asyncio 事件循环，与主线程 qasync 循环冲突。
            在独立线程运行独立事件循环可以完全隔离。

        Code Logic（这个函数做什么）:
            1. 创建并设置独立 asyncio 事件循环
            2. 创建 Zeroconf 实例
            3. 注册 mDNS 服务
            4. 启动 ServiceBrowser 浏览
            5. 保持事件循环运行直到 stop 被调用
        """
        self._mdns_loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._mdns_loop)

        try:
            self._zeroconf = Zeroconf()

            # 注册服务
            assert self._service_info is not None
            self._zeroconf.register_service(
                self._service_info, cooperating_responders=True
            )
            logger.info(
                "mDNS 服务已注册: %s (端口 %d)",
                self._config.device_name,
                port,
            )

            # 启动浏览
            self._browser = ServiceBrowser(
                self._zeroconf,
                SERVICE_TYPE,
                handlers=[self._on_service_state_change],
            )
            logger.info("mDNS 服务浏览已启动")

            # 保持线程运行（zeroconf 需要这个事件循环）
            self._mdns_loop.run_forever()

        except Exception as e:
            logger.error("mDNS 服务启动失败: %s", e, exc_info=True)
        finally:
            if self._zeroconf is not None:
                if self._browser is not None:
                    self._browser.cancel()
                if self._service_info is not None:
                    try:
                        self._zeroconf.unregister_service(self._service_info)
                    except Exception:
                        pass
                self._zeroconf.close()
            if self._mdns_loop is not None:
                self._mdns_loop.close()

    def stop(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用关闭时需要注销 mDNS 服务，释放网络资源，通知局域网本设备下线。

        Code Logic（这个函数做什么）:
            停止后台线程的事件循环，线程的 finally 块会负责清理 zeroconf 资源。
        """
        if self._mdns_loop is not None:
            self._mdns_loop.call_soon_threadsafe(self._mdns_loop.stop)

        if self._thread is not None:
            self._thread.join(timeout=5)
            self._thread = None

        self._devices.clear()
        logger.info("mDNS 服务已停止")

    def get_devices(self) -> dict[str, Device]:
        """
        Business Logic（为什么需要这个函数）:
            同步引擎和 UI 需要获取当前在线的设备列表来发起同步或展示状态。

        Code Logic（这个函数做什么）:
            返回当前发现的在线设备字典的副本 {device_id: Device}。
        """
        return dict(self._devices)

    @staticmethod
    def _get_local_ip() -> str:
        """
        Business Logic（为什么需要这个函数）:
            mDNS 注册需要本机的实际局域网 IP，但系统可能有多个网络接口
            （WiFi、VPN、Docker 等），需要优先选择真实局域网接口的 IP。

        Code Logic（这个函数做什么）:
            1. 通过 ip/ifconfig 获取所有网络接口 IP
            2. 过滤掉 loopback、docker、veth、utun 等虚拟接口
            3. 优先选择私有局域网段（192.168.x, 10.x, 172.16-31.x）
            4. 如果找不到，回退到 UDP socket 探测
        """
        import re
        import subprocess

        try:
            # Linux/Mac: 获取所有接口 IP
            result = subprocess.run(
                ["ip", "-4", "addr", "show"],
                capture_output=True, text=True, timeout=3,
            )
            if result.returncode != 0:
                # macOS 没有 ip 命令，用 ifconfig
                result = subprocess.run(
                    ["ifconfig"],
                    capture_output=True, text=True, timeout=3,
                )

            # 解析出 (接口名, IP) 对
            candidates: list[tuple[str, str]] = []
            current_iface: str = ""
            for line in result.stdout.split("\n"):
                # ip addr 格式: "2: wlp4s0: <...>"
                iface_match = re.match(r"^\d+:\s+(\S+?):", line)
                if iface_match:
                    current_iface = iface_match.group(1)
                # ip addr 格式: "    inet 192.168.6.17/24 ..."
                inet_match = re.search(r"inet\s+(\d+\.\d+\.\d+\.\d+)", line)
                if inet_match:
                    ip: str = inet_match.group(1)
                    candidates.append((current_iface, ip))

            # 过滤虚拟接口
            skip_prefixes: tuple[str, ...] = (
                "lo", "docker", "br-", "veth", "utun",
                "tun", "tailscale", "zt",
            )
            lan_ips: list[str] = []
            for iface, ip in candidates:
                if any(iface.startswith(p) for p in skip_prefixes):
                    continue
                if ip.startswith("127."):
                    continue
                # 私有局域网段优先
                if (ip.startswith("192.168.")
                        or ip.startswith("10.")
                        or re.match(r"^172\.(1[6-9]|2\d|3[01])\.", ip)):
                    lan_ips.append(ip)

            if lan_ips:
                logger.debug("局域网 IP 候选: %s", lan_ips)
                return lan_ips[0]

        except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
            pass

        # 回退方案
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.connect(("8.8.8.8", 80))
            ip = s.getsockname()[0]
            s.close()
            return ip
        except OSError:
            return socket.gethostbyname(socket.gethostname())

    def _on_service_state_change(self, **kwargs: object) -> None:
        """
        Business Logic（为什么需要这个函数）:
            当局域网中有设备上线或下线时，需要更新本地设备列表并通知 UI。

        Code Logic（这个函数做什么）:
            zeroconf ServiceBrowser 的回调，在后台线程中执行。
            注意：新版 zeroconf (>=0.131) 使用关键字参数调用回调，
            因此使用 **kwargs 接收以保证兼容性。
            - Added: 解析 ServiceInfo，创建 Device 对象，存入 _devices，emit device_found
            - Removed: 从 _devices 移除对应设备，emit device_lost
            过滤掉自己的设备 ID，避免发现自己。
            Qt 信号机制自动处理跨线程分发。
        """
        zeroconf: Zeroconf = kwargs["zeroconf"]  # type: ignore[assignment]
        service_type: str = kwargs["service_type"]  # type: ignore[assignment]
        name: str = kwargs["name"]  # type: ignore[assignment]
        state_change: ServiceStateChange = kwargs["state_change"]  # type: ignore[assignment]
        logger.debug(
            "mDNS 服务状态变化: name=%s, state=%s", name, state_change
        )

        if state_change == ServiceStateChange.Added:
            info: ServiceInfo | None = zeroconf.get_service_info(
                service_type, name
            )
            if info is None:
                logger.warning("无法获取服务信息: %s", name)
                return

            logger.debug(
                "服务详情: name=%s, addresses=%s, port=%s, properties=%s",
                name,
                [socket.inet_ntoa(a) for a in info.addresses] if info.addresses else [],
                info.port,
                info.properties,
            )

            # 解析 TXT 记录
            properties: dict[str, str] = {}
            if info.properties:
                for key, value in info.properties.items():
                    k: str = key.decode("utf-8") if isinstance(key, bytes) else key
                    v: str = value.decode("utf-8") if isinstance(value, bytes) else value
                    properties[k] = v

            device_id: str = properties.get("device_id", "")
            device_name: str = properties.get("device_name", "unknown")

            # 过滤掉自己
            if device_id == self._config.device_id:
                return

            if not device_id:
                logger.warning("服务 TXT 记录缺少 device_id: %s", name)
                return

            # 解析 IP 地址
            host: str = ""
            if info.addresses:
                host = socket.inet_ntoa(info.addresses[0])
            elif info.parsed_addresses():
                host = info.parsed_addresses()[0]

            if not host:
                logger.warning("无法解析服务地址: %s", name)
                return

            device: Device = Device(
                id=device_id,
                name=device_name,
                host=host,
                port=info.port,
                last_seen=datetime.now(),
                online=True,
            )

            self._devices[device_id] = device
            self.device_found.emit(device)
            logger.info("发现设备: %s (%s:%d)", device_name, host, info.port)

        elif state_change == ServiceStateChange.Removed:
            # 从服务名中提取 device_id
            # 服务名格式: "{device_id}._claude-partner._tcp.local."
            device_id_from_name: str = name.replace(f".{SERVICE_TYPE}", "")

            # 过滤掉自己
            if device_id_from_name == self._config.device_id:
                return

            if device_id_from_name in self._devices:
                del self._devices[device_id_from_name]
                self.device_lost.emit(device_id_from_name)
                logger.info("设备离线: %s", device_id_from_name)
