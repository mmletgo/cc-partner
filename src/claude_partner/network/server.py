# -*- coding: utf-8 -*-
"""HTTP 服务端模块：基于 aiohttp 的 HTTP 服务器。"""

from pathlib import Path

from aiohttp import web
import logging

from claude_partner.config import CONFIG_DIR
from claude_partner.network.protocol import APIProtocol

logger: logging.Logger = logging.getLogger(__name__)


class HTTPServer:
    """
    aiohttp HTTP 服务端，承载 P2P 通信的 API 接口和前端静态文件。

    Business Logic（为什么需要这个类）:
        每个 Claude Partner 实例需要监听 HTTP 端口接收其他设备的请求
        （同步拉取/推送、文件传输等），这是 P2P 架构中服务端角色的核心。
        同时需要为前端 UI 提供静态文件服务和 SPA 路由回退。

    Code Logic（这个类做什么）:
        封装 aiohttp 的 web.Application、AppRunner 和 TCPSite，
        提供启动（自动分配端口）和停止的生命周期管理。
        支持 port=0 让操作系统自动分配可用端口。
        启动时将端口号写入端口文件，停止时删除端口文件。
        支持通过 serve_static() 挂载前端静态资源目录。
    """

    PORT_FILE: Path = CONFIG_DIR / "backend.port"

    def __init__(self, protocol: APIProtocol) -> None:
        """
        Business Logic（为什么需要这个函数）:
            HTTP 服务器需要知道路由配置（由 APIProtocol 提供）来处理请求。

        Code Logic（这个函数做什么）:
            保存 protocol 引用，初始化内部状态。
        """
        self._protocol: APIProtocol = protocol
        self._app: web.Application | None = None
        self._runner: web.AppRunner | None = None
        self._site: web.TCPSite | None = None
        self._port: int = 0
        self._web_dir: Path | None = None

    def serve_static(self, web_dir: Path) -> None:
        """
        Business Logic（为什么需要这个函数）:
            前端打包后的静态文件需要由后端 HTTP 服务提供，
            这样桌面应用只需启动一个服务端口即可同时提供 API 和 UI。

        Code Logic（这个函数做什么）:
            保存前端静态文件目录路径，在 start() 中注册路由。
            必须在 start() 之前调用，因为 AppRunner.setup() 后路由会被冻结。
        """
        self._web_dir = web_dir

    def _register_static_routes(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            SPA 应用需要静态资源服务和前端路由回退，
            直接访问 /prompts 等路径时后端返回 index.html。

        Code Logic（这个函数做什么）:
            1. 将 web_dir/assets 挂载到 /assets 路径（StaticResource）
            2. 添加 SPA 回退路由 /{path:path} 返回 index.html
            在 setup_routes() 之后注册，确保 /api/* 路由优先匹配。
        """
        if self._web_dir is None or self._app is None:
            return

        # 挂载 /assets 静态资源
        assets_path = self._web_dir / "assets"
        if assets_path.exists():
            static_resource = web.StaticResource("/assets", str(assets_path))
            self._app.router.register_resource(static_resource)

        # SPA 回退：读取 index.html 内容
        index_html = self._web_dir / "index.html"

        async def _spa_fallback(_request: web.Request) -> web.Response:
            """
            Business Logic:
                SPA 应用使用前端路由（如 /prompts、/settings），
                直接访问这些路径时后端需要返回 index.html 而不是 404。

            Code Logic:
                返回 index.html 文件内容，Content-Type 设为 text/html。
            """
            return web.Response(
                text=index_html.read_text(encoding="utf-8"),
                content_type="text/html",
            )

        # SPA 回退路由：显式注册根路径 + 通配路径（{path:.*} 匹配含 / 的任意路径）
        self._app.router.add_route("GET", "/", _spa_fallback)
        self._app.router.add_route("GET", "/{path:.*}", _spa_fallback)

    async def start(self, port: int = 0) -> int:
        """
        Business Logic（为什么需要这个函数）:
            应用启动时需要开始监听 HTTP 端口，以接收其他设备的通信请求。

        Code Logic（这个函数做什么）:
            1. 创建 aiohttp web.Application
            2. 注册 APIProtocol 定义的路由
            3. 注册前端静态文件路由（如果已配置）
            4. 使用 AppRunner + TCPSite 启动服务
            5. port=0 时由操作系统自动分配端口
            6. 从 socket 获取实际监听端口并返回
        """
        # client_max_size 设为 2MB，文件传输 chunk 大小为 1MB + HTTP 开销
        self._app = web.Application(client_max_size=2 * 1024 * 1024)
        self._protocol.setup_routes(self._app)
        # 静态文件路由必须在 runner.setup() 之前注册（之后路由冻结）
        self._register_static_routes()

        self._runner = web.AppRunner(self._app)
        await self._runner.setup()

        self._site = web.TCPSite(self._runner, "0.0.0.0", port)
        await self._site.start()

        # 获取实际监听端口（_server.sockets 是 aiohttp 内部属性，无公开 API）
        if self._site._server is not None and self._site._server.sockets:  # type: ignore[union-attr]
            self._port = self._site._server.sockets[0].getsockname()[1]  # type: ignore[union-attr]
        else:
            self._port = port

        logger.info("HTTP 服务已启动，监听端口: %d", self._port)

        # 将端口号写入文件，供前端开发服务器等外部进程读取
        self.PORT_FILE.write_text(str(self._port))

        return self._port

    async def stop(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用关闭时需要优雅地停止 HTTP 服务，释放端口资源。

        Code Logic（这个函数做什么）:
            依次停止 TCPSite、清理 AppRunner，释放所有网络资源。
            删除端口文件，避免残留过期信息。
        """
        # 删除端口文件
        self.PORT_FILE.unlink(missing_ok=True)

        if self._site is not None:
            await self._site.stop()
            self._site = None

        if self._runner is not None:
            await self._runner.cleanup()
            self._runner = None

        self._app = None
        logger.info("HTTP 服务已停止")

    @property
    def port(self) -> int:
        """
        Business Logic（为什么需要这个函数）:
            mDNS 注册和 UI 显示需要知道当前服务实际监听的端口号。

        Code Logic（这个函数做什么）:
            返回启动时获取的实际端口号。
        """
        return self._port
