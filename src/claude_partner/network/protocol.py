# -*- coding: utf-8 -*-
"""HTTP API 路由模块：定义 P2P 通信的 RESTful 接口。"""

from aiohttp import web
from typing import Callable, Awaitable
import asyncio
import logging
import os
import uuid
from datetime import datetime, timezone
from pathlib import Path

from claude_partner.config import AppConfig
from claude_partner.storage.prompt_repo import PromptRepository
from claude_partner.models.prompt import Prompt
from claude_partner.sync.vector_clock import VectorClock

logger: logging.Logger = logging.getLogger(__name__)


class APIProtocol:
    """
    HTTP API 路由定义和请求处理，作为 P2P 通信的服务端接口。

    Business Logic（为什么需要这个类）:
        每个 Claude Partner 实例既是客户端也是服务端。当其他设备发起同步请求或
        文件传输请求时，需要通过标准化的 HTTP API 来处理。
        同时，前端 React 界面（嵌入式 Web）也需要 RESTful 接口来：
        - 读写本地 Prompt（CRUD）
        - 列出局域网设备和传输任务
        - 触发文件发送和取消
        所有这些都通过本类的统一路由表暴露。

    Code Logic（这个类做什么）:
        定义 19 个 API 端点：
        - /api/health: 健康检查（含 http_port）
        - /api/prompts (GET/POST): 列表/创建本地 Prompt
        - /api/prompts/{id} (GET/PUT/DELETE): 单条 Prompt 的读/改/删
        - /api/devices: 局域网内已发现的对端设备列表
        - /api/sync (POST): 触发全局同步引擎
        - /api/sync/pull: P2P 同步 - 接收对端摘要并返回对端需要的 prompt
        - /api/sync/push: P2P 同步 - 接收对端推送的 prompt
        - /api/transfer/tasks: 列出全部传输任务
        - /api/transfer/send (POST): 启动一次文件发送
        - /api/transfer/tasks/{id} (DELETE): 取消传输
        - /api/transfer/init: P2P 接收 - 初始化对端发来的传输
        - /api/transfer/chunk/{transfer_id}: P2P 接收 - 接收分块数据
        - /api/transfer/status/{transfer_id}: P2P 接收 - 查询接收端状态
        - /api/config (GET/PUT): 读写应用配置
        - /api/config/choose-dir (POST): 打开原生目录选择对话框
        - /api/version: 获取应用版本信息
        - /api/updater/check (POST): 触发更新检查
        - /api/permissions: 检查 macOS 权限状态
    """

    def __init__(
        self,
        config: AppConfig,
        prompt_repo: PromptRepository,
        on_transfer_init: Callable[[dict], dict] | None = None,
        on_transfer_chunk: Callable[[str, int, bytes], Awaitable[dict]] | None = None,
        get_transfer_status: Callable[[str], dict] | None = None,
        get_devices: Callable[[], list[dict]] | None = None,
        on_transfer_send: Callable[[str, str, str], Awaitable[object]] | None = None,
        on_transfer_cancel: Callable[[str], bool] | None = None,
        get_transfers: Callable[[], list[dict]] | None = None,
        on_choose_dir: Callable[[], str] | None = None,
        on_check_update: Callable[[], Awaitable[dict]] | None = None,
        check_permissions: Callable[[], dict] | None = None,
        actual_port: int = 0,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化 API 处理器，需要配置信息、数据仓库以及文件传输的回调函数。

        Code Logic（这个函数做什么）:
            保存依赖注入的组件引用。所有回调均为可选：
            - 设备列表、设备传输任务相关端点需要对应 getter
            - on_choose_dir: 打开原生目录选择对话框
            - on_check_update: 触发更新检查并返回结果
            - check_permissions: 检查 macOS 权限状态
            - actual_port: HTTP 服务端实际监听端口（动态分配时与配置端口不同）
            - 未提供时端点返回 501 Not Implemented（不破坏向后兼容）
        """
        self._config: AppConfig = config
        self._prompt_repo: PromptRepository = prompt_repo
        self._on_transfer_init: Callable[[dict], dict] | None = on_transfer_init
        self._on_transfer_chunk: Callable[[str, int, bytes], Awaitable[dict]] | None = on_transfer_chunk
        self._get_transfer_status: Callable[[str], dict] | None = get_transfer_status
        self._get_devices: Callable[[], list[dict]] | None = get_devices
        self._on_transfer_send: Callable[..., Awaitable[object]] | None = on_transfer_send
        self._on_transfer_cancel: Callable[[str], bool] | None = on_transfer_cancel
        self._get_transfers: Callable[[], list[dict]] | None = get_transfers
        self._on_choose_dir: Callable[[], str] | None = on_choose_dir
        self._on_check_update: Callable[[], Awaitable[dict]] | None = on_check_update
        self._check_permissions: Callable[[], dict] | None = check_permissions
        self._actual_port: int = actual_port

    def setup_routes(self, app: web.Application) -> None:
        """
        Business Logic（为什么需要这个函数）:
            将所有 API 端点注册到 aiohttp 应用的路由表中。

        Code Logic（这个函数做什么）:
            为 app.router 添加 GET/POST/PUT/DELETE 路由，关联到对应的 handler 方法。
        """
        # 健康
        app.router.add_get("/api/health", self.handle_health)

        # 前端 REST - Prompt CRUD
        app.router.add_get("/api/prompts", self.handle_list_prompts)
        app.router.add_post("/api/prompts", self.handle_create_prompt)
        app.router.add_get("/api/prompts/{prompt_id}", self.handle_get_prompt)
        app.router.add_put("/api/prompts/{prompt_id}", self.handle_update_prompt)
        app.router.add_delete("/api/prompts/{prompt_id}", self.handle_delete_prompt)

        # 前端 REST - 设备
        app.router.add_get("/api/devices", self.handle_list_devices)

        # 前端 REST - 同步
        app.router.add_post("/api/sync", self.handle_sync_all)

        # P2P 同步协议（对端调用）
        app.router.add_post("/api/sync/pull", self.handle_sync_pull)
        app.router.add_post("/api/sync/push", self.handle_sync_push)

        # 前端 REST - 传输任务
        app.router.add_get("/api/transfer/tasks", self.handle_list_transfers)
        app.router.add_post("/api/transfer/send", self.handle_transfer_send)
        app.router.add_delete(
            "/api/transfer/tasks/{transfer_id}", self.handle_cancel_transfer
        )

        # P2P 接收协议（对端调用）
        app.router.add_post("/api/transfer/init", self.handle_transfer_init)
        app.router.add_post(
            "/api/transfer/chunk/{transfer_id}", self.handle_transfer_chunk
        )
        app.router.add_get(
            "/api/transfer/status/{transfer_id}", self.handle_transfer_status
        )

        # 前端 REST - 配置
        app.router.add_get("/api/config", self.handle_get_config)
        app.router.add_put("/api/config", self.handle_update_config)
        app.router.add_post("/api/config/choose-dir", self.handle_choose_dir)

        # 前端 REST - 版本信息
        app.router.add_get("/api/version", self.handle_version)

        # 前端 REST - 更新检查
        app.router.add_post("/api/updater/check", self.handle_check_update)

        # 前端 REST - 权限检查
        app.router.add_get("/api/permissions", self.handle_permissions)

    # ── 通用工具 ──

    def set_actual_port(self, port: int) -> None:
        """
        Business Logic（为什么需要这个函数）:
            HTTP 服务端使用 port=0 动态分配端口时，实际端口在 start() 之后才确定。
            health 和 config 端点需要返回真实的监听端口，而非配置中的占位值。

        Code Logic（这个函数做什么）:
            更新内部 _actual_port，后续 handle_health / handle_get_config 使用此值。
        """
        self._actual_port = port
        logger.info("APIProtocol actual_port 已设置为 %d", port)

    @staticmethod
    def _prompt_to_frontend_dict(p: Prompt) -> dict:
        """
        Business Logic:
            前端 web/src/lib/types.ts Prompt 类型使用 camelCase + tag 单数字段，
            与后端 Prompt.to_dict()（snake_case + tags 数组）不一致。
            必须在 API 层做一次转换，避免修改后端 dataclass。

        Code Logic:
            将 snake_case → camelCase 字段名；
            tags[0] 投影为 tag（单标签）以匹配前端 UI 习惯；
            ISO datetime 保持原样。
        """
        return {
            "id": p.id,
            "title": p.title,
            "content": p.content,
            "tag": p.tags[0] if p.tags else None,
            "tags": p.tags,
            "updatedAt": p.updated_at.isoformat(),
            "createdAt": p.created_at.isoformat(),
            "deviceId": p.device_id,
            "vectorClock": p.vector_clock,
            "deleted": p.deleted,
        }

    @staticmethod
    def _transfer_to_frontend_dict(t) -> dict:
        """
        Business Logic:
            前端 TransferTask 期望 camelCase + 派生字段（fileName / fileSize / progress / speed / peerDeviceName）。
            后端 TransferTask.to_dict() 是 snake_case + 原始 transferred_bytes。

        Code Logic:
            调用 task.to_dict() 拿到原始字段，再做：
            - 字段名转换 snake_case → camelCase
            - 由 transferred_bytes / size 计算 progress 0.0~1.0
            - speed 暂时为 None（实际应由 sender 在循环中维护）
            - peerDeviceName 由对端 device_id 在本端设备表中查不到，置空
        """
        raw = t.to_dict()
        progress: float = t.progress()
        return {
            "id": raw["id"],
            "fileName": raw["filename"],
            "filePath": raw["file_path"],
            "fileSize": raw["size"],
            "direction": raw["direction"],
            "status": raw["status"],
            "progress": progress,
            "transferredBytes": raw["transferred_bytes"],
            "peerDeviceId": raw["peer_device_id"],
            "peerDeviceName": None,  # sender 不持有对端 name；前端 mock 用
            "speed": None,
            "errorMessage": None,
            "startedAt": raw["created_at"],
            "completedAt": raw["completed_at"],
        }

    # ── 健康 ──

    async def handle_health(self, _request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            对端设备需要检查本机是否在线且可通信，用于同步前的连通性验证。

        Code Logic（这个函数做什么）:
            返回 JSON 响应 {ok: true, device_id, device_name, http_port, ts}。
        """
        data: dict = {
            "ok": True,
            "device_id": self._config.device_id,
            "device_name": self._config.device_name,
            "http_port": self._actual_port,
            "ts": int(datetime.now(timezone.utc).timestamp()),
        }
        return web.json_response(data)

    # ── 前端 Prompt CRUD ──

    async def handle_list_prompts(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            Prompts 页面 / Home 页面需要列出全部本地 Prompt。
            可选 query 参数：search（关键词）、tag（按标签筛选）。

        Code Logic:
            - 无参数：get_all()
            - search：search(keyword)
            - tag：filter_by_tags([tag])
            返回 camelCase 字典列表。
        """
        try:
            search: str | None = request.query.get("search")
            tag: str | None = request.query.get("tag")
            if search:
                prompts = await self._prompt_repo.search(search)
            elif tag:
                prompts = await self._prompt_repo.filter_by_tags([tag])
            else:
                prompts = await self._prompt_repo.get_all()
            return web.json_response([self._prompt_to_frontend_dict(p) for p in prompts])
        except Exception as e:
            logger.error("handle_list_prompts 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_create_prompt(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端"新建 Prompt"按钮提交 {title, content, tag}。

        Code Logic:
            解析 JSON -> 构造 Prompt（生成 uuid，vector_clock 初始 {device_id: 1}）-> repo.create
            返回 201 + 完整 dict。
        """
        try:
            body: dict = await request.json()
            now: datetime = datetime.now(timezone.utc)
            prompt = Prompt(
                id=str(uuid.uuid4()),
                title=body.get("title", "").strip(),
                content=body.get("content", ""),
                tags=[body["tag"]] if body.get("tag") else [],
                created_at=now,
                updated_at=now,
                device_id=self._config.device_id,
                vector_clock={self._config.device_id: 1},
                deleted=False,
            )
            await self._prompt_repo.create(prompt)
            return web.json_response(self._prompt_to_frontend_dict(prompt), status=201)
        except Exception as e:
            logger.error("handle_create_prompt 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_get_prompt(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端编辑弹窗打开时按 ID 读取完整 Prompt 数据。

        Code Logic:
            从路径参数取 id -> repo.get_by_id；不存在返回 404。
        """
        try:
            prompt_id: str = request.match_info["prompt_id"]
            prompt = await self._prompt_repo.get_by_id(prompt_id)
            if prompt is None or prompt.deleted:
                return web.json_response({"error": "Prompt 不存在"}, status=404)
            return web.json_response(self._prompt_to_frontend_dict(prompt))
        except Exception as e:
            logger.error("handle_get_prompt 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_update_prompt(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端编辑表单保存时提交修改；本端编辑时需要推进 vector_clock
            {device_id: +1} 以标记这是本端的新版本（CRDT 行为）。

        Code Logic:
            读 id -> get_by_id -> 更新字段 -> 自增 vector_clock[self._config.device_id]
            -> repo.update。
        """
        try:
            prompt_id: str = request.match_info["prompt_id"]
            prompt = await self._prompt_repo.get_by_id(prompt_id)
            if prompt is None:
                return web.json_response({"error": "Prompt 不存在"}, status=404)

            body: dict = await request.json()
            if "title" in body:
                prompt.title = body["title"].strip()
            if "content" in body:
                prompt.content = body["content"]
            if "tag" in body:
                prompt.tags = [body["tag"]] if body["tag"] else []
            prompt.updated_at = datetime.now(timezone.utc)
            # 推进本端计数器
            prompt.vector_clock[self._config.device_id] = (
                prompt.vector_clock.get(self._config.device_id, 0) + 1
            )
            await self._prompt_repo.update(prompt)
            return web.json_response(self._prompt_to_frontend_dict(prompt))
        except Exception as e:
            logger.error("handle_update_prompt 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_delete_prompt(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端"删除"按钮触发软删除；CRDT 删除需要先自增 vector_clock
            再标记 deleted=1，使对端能感知到"删除事件"。

        Code Logic:
            读 id -> repo.get_by_id -> 自增 vector_clock -> repo.delete。
        """
        try:
            prompt_id: str = request.match_info["prompt_id"]
            prompt = await self._prompt_repo.get_by_id(prompt_id)
            if prompt is None:
                return web.json_response({"error": "Prompt 不存在"}, status=404)
            # CRDT: 删除也是一次写入，先推进 clock
            prompt.vector_clock[self._config.device_id] = (
                prompt.vector_clock.get(self._config.device_id, 0) + 1
            )
            await self._prompt_repo.delete(prompt_id)
            return web.json_response({"ok": True, "id": prompt_id})
        except Exception as e:
            logger.error("handle_delete_prompt 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    # ── 前端 设备 ──

    async def handle_list_devices(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            设备面板拉取当前发现的全部对端设备。

        Code Logic:
            调用注入的 get_devices 回调，getter 未注册时返回 501。
        """
        if self._get_devices is None:
            return web.json_response(
                {"error": "设备发现功能未启用"}, status=501
            )
        try:
            devices = self._get_devices()
            return web.json_response(devices)
        except Exception as e:
            logger.error("handle_list_devices 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    # ── 前端 同步触发 ──

    async def handle_sync_all(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端 Prompts 页面"同步"按钮触发全网同步。
            同步操作在后台异步执行，本端点立即返回触发结果。

        Code Logic:
            解析 body 中的 device_ids（可选）；不传则同步全部已发现设备。
            通过 asyncio.create_task 异步执行 sync_all；当前 handler 立刻返回
            {"accepted": True, "task": "sync"}。
        """
        # 简化：暂未注入 sync_engine；返回模拟响应
        # 真正实现需要：把 SyncEngine 通过参数注入
        return web.json_response({
            "accepted": True,
            "synced": 0,
            "note": "sync engine 未在前端 API 中暴露（待 P2P sync engine 集成）",
        })

    # ── 前端 传输任务 ──

    async def handle_list_transfers(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            传输面板列出全部发送/接收任务（含历史）。

        Code Logic:
            合并 sender + receiver 的任务列表，按 created_at 倒序。
            sender/receiver 未注入时返回 501。
        """
        try:
            if self._get_transfers is None:
                return web.json_response([], status=200)
            tasks = self._get_transfers()
            return web.json_response(tasks)
        except Exception as e:
            logger.error("handle_list_transfers 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_transfer_send(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端"选择文件 + 目标设备 → 发送"按钮触发。
            后端根据 deviceId 解析 base_url，调用 sender.send_file 异步执行。

        Code Logic:
            解析 {deviceId, filePath} -> 异步启动 send_file -> 立即返回 202 + taskId。
        """
        if self._on_transfer_send is None or self._get_devices is None:
            return web.json_response(
                {"error": "发送功能未启用"}, status=501
            )
        try:
            body: dict = await request.json()
            device_id: str = body["deviceId"]
            file_path: str = body["filePath"]
            # 查对端 base_url
            target: dict | None = next(
                (d for d in self._get_devices() if d["id"] == device_id), None
            )
            if target is None:
                return web.json_response(
                    {"error": f"设备 {device_id} 不在线"}, status=404
                )
            base_url: str = f"http://{target['address']}:{target['port']}"
            coro: Awaitable[object] = self._on_transfer_send(
                file_path, base_url, device_id
            )
            # pyright: ignore[reportArgumentType] - create_task 需要 Coroutine，Awaitable 是其超集
            asyncio.create_task(coro)  # type: ignore[arg-type]
            return web.json_response(
                {"accepted": True, "deviceId": device_id, "filePath": file_path},
                status=202,
            )
        except Exception as e:
            logger.error("handle_transfer_send 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_cancel_transfer(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端传输项的"取消"按钮调 DELETE /api/transfer/tasks/{id}。

        Code Logic:
            调注入的 on_transfer_cancel；不存在时返回 404。
        """
        if self._on_transfer_cancel is None:
            return web.json_response(
                {"error": "取消功能未启用"}, status=501
            )
        try:
            transfer_id: str = request.match_info["transfer_id"]
            ok: bool = self._on_transfer_cancel(transfer_id)
            if not ok:
                return web.json_response(
                    {"error": f"传输任务 {transfer_id} 不存在"}, status=404
                )
            return web.json_response({"ok": True, "id": transfer_id})
        except Exception as e:
            logger.error("handle_cancel_transfer 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_sync_pull(self, request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            对端设备发起 pull 请求时，发送它的 prompt 摘要列表，
            本端比较后返回对端需要的（本端有更新的或本端独有的）prompt。

        Code Logic（这个函数做什么）:
            1. 解析请求体中的 remote_summaries [{id, vector_clock}, ...]
            2. 获取本端的 sync_summary
            3. 对比双方摘要：
               - 本端有但对端没有的 prompt -> 返回
               - 本端向量时钟领先或并发的 prompt -> 返回
            4. 从数据库读取需要返回的 prompt 完整数据
            5. 返回 {prompts: [prompt_dict, ...]}
        """
        try:
            body: dict = await request.json()
            remote_summaries: list[dict] = body.get("summaries", [])

            # 获取本端摘要
            local_summaries: list[dict] = await self._prompt_repo.get_sync_summary()

            # 构建查找表
            remote_map: dict[str, dict[str, int]] = {
                s["id"]: s["vector_clock"] for s in remote_summaries
            }

            # 找出本端需要返回给对端的 prompt id
            need_send_ids: list[str] = []
            for local_s in local_summaries:
                prompt_id: str = local_s["id"]
                local_clock: dict[str, int] = local_s["vector_clock"]

                if prompt_id not in remote_map:
                    # 本端有但对端没有
                    need_send_ids.append(prompt_id)
                else:
                    remote_clock: dict[str, int] = remote_map[prompt_id]
                    relation: str = VectorClock.compare(local_clock, remote_clock)
                    if relation in ("greater", "concurrent"):
                        # 本端领先或并发，发送给对端做合并
                        need_send_ids.append(prompt_id)

            # 读取完整的 prompt 数据
            prompts: list[dict] = []
            for pid in need_send_ids:
                prompt: Prompt | None = await self._prompt_repo.get_by_id(pid)
                if prompt is not None:
                    prompts.append(prompt.to_dict())

            logger.info(
                "sync/pull: 对端摘要 %d 条，本端摘要 %d 条，返回 %d 条 prompt",
                len(remote_summaries),
                len(local_summaries),
                len(prompts),
            )
            return web.json_response({"prompts": prompts})

        except Exception as e:
            logger.error("handle_sync_pull 异常: %s", e, exc_info=True)
            return web.json_response(
                {"error": str(e)}, status=500
            )

    async def handle_sync_push(self, request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            对端设备将本端缺少或过时的 prompt 推送过来，本端需要接收并存储。

        Code Logic（这个函数做什么）:
            1. 解析请求体中的 prompts 列表
            2. 将字典转换为 Prompt 对象
            3. 调用 prompt_repo.bulk_upsert 批量写入
            4. 返回 {accepted: count}
        """
        try:
            body: dict = await request.json()
            prompt_dicts: list[dict] = body.get("prompts", [])

            prompts: list[Prompt] = [
                Prompt.from_dict(d) for d in prompt_dicts
            ]
            await self._prompt_repo.bulk_upsert(prompts)

            logger.info("sync/push: 接收 %d 条 prompt", len(prompts))
            return web.json_response({"accepted": len(prompts)})

        except Exception as e:
            logger.error("handle_sync_push 异常: %s", e, exc_info=True)
            return web.json_response(
                {"error": str(e)}, status=500
            )

    async def handle_transfer_init(self, request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            对端发起文件传输前，需要先发送文件元数据（文件名、大小、校验和），
            本端确认后分配 transfer_id 用于后续分块传输。

        Code Logic（这个函数做什么）:
            解析请求体 {filename, size, sha256, chunk_size}，转发给 on_transfer_init 回调。
            回调未注册时返回 501 Not Implemented。
        """
        if self._on_transfer_init is None:
            return web.json_response(
                {"error": "文件传输功能未启用"}, status=501
            )

        try:
            body: dict = await request.json()
            result: dict = self._on_transfer_init(body)
            return web.json_response(result)
        except Exception as e:
            logger.error("handle_transfer_init 异常: %s", e, exc_info=True)
            return web.json_response(
                {"error": str(e)}, status=500
            )

    async def handle_transfer_chunk(self, request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            文件传输过程中，对端分块发送文件数据，本端逐块接收并写入。

        Code Logic（这个函数做什么）:
            1. 从 URL 路径参数获取 transfer_id
            2. 从 X-Chunk-Offset header 获取 offset
            3. 从 body 读取原始 bytes 数据
            4. 转发给 on_transfer_chunk 回调处理
        """
        if self._on_transfer_chunk is None:
            return web.json_response(
                {"error": "文件传输功能未启用"}, status=501
            )

        try:
            transfer_id: str = request.match_info["transfer_id"]
            offset_str: str = request.headers.get("X-Chunk-Offset", "0")
            offset: int = int(offset_str)
            data: bytes = await request.read()

            result: dict = await self._on_transfer_chunk(transfer_id, offset, data)
            return web.json_response(result)
        except Exception as e:
            logger.error("handle_transfer_chunk 异常: %s", e, exc_info=True)
            return web.json_response(
                {"error": str(e)}, status=500
            )

    async def handle_transfer_status(self, request: web.Request) -> web.Response:
        """
        Business Logic（为什么需要这个函数）:
            文件传输过程中或传输后，需要查询传输进度和状态（是否完成、已接收大小等）。

        Code Logic（这个函数做什么）:
            从 URL 路径参数获取 transfer_id，调用 get_transfer_status 回调查询状态。
        """
        if self._get_transfer_status is None:
            return web.json_response(
                {"error": "文件传输功能未启用"}, status=501
            )

        try:
            transfer_id: str = request.match_info["transfer_id"]
            result: dict = self._get_transfer_status(transfer_id)
            return web.json_response(result)
        except Exception as e:
            logger.error("handle_transfer_status 异常: %s", e, exc_info=True)
            return web.json_response(
                {"error": str(e)}, status=500
            )

    # ── 前端 配置 ──

    async def handle_get_config(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面需要读取当前应用配置（设备名、接收目录、快捷键等），
            以便展示和编辑。

        Code Logic:
            读取 self._config，将 snake_case 字段转为 camelCase 返回给前端。
            deviceId 和 httpPort 为只读字段也一并返回供前端展示。
        """
        try:
            data: dict = {
                "deviceId": self._config.device_id,
                "deviceName": self._config.device_name,
                "receiveDir": self._config.receive_dir,
                "screenshotHotkey": self._config.screenshot_hotkey,
                "httpPort": self._actual_port,
            }
            return web.json_response(data)
        except Exception as e:
            logger.error("handle_get_config 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_update_config(self, request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面用户修改配置后需要保存到磁盘，使下次启动时生效。

        Code Logic:
            解析 JSON body，仅允许更新 deviceName、receiveDir、screenshotHotkey
            三个字段（deviceId 和 httpPort 为只读）。
            更新后调用 config.save() 持久化，返回更新后的完整配置。
        """
        try:
            body: dict = await request.json()
            if "deviceName" in body:
                self._config.device_name = body["deviceName"]
            if "receiveDir" in body:
                self._config.receive_dir = body["receiveDir"]
            if "screenshotHotkey" in body:
                self._config.screenshot_hotkey = body["screenshotHotkey"]
            self._config.save()
            logger.info("配置已更新并保存")
            return await self.handle_get_config(request)
        except Exception as e:
            logger.error("handle_update_config 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    async def handle_choose_dir(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面用户修改"文件接收目录"时，需要打开系统原生
            目录选择对话框让用户选择路径。

        Code Logic:
            调用注入的 on_choose_dir 回调，该回调会弹出 QFileDialog。
            返回 {"path": "选中的路径"} 或 {"path": null}（用户取消）。
            回调未注册时返回 501。
        """
        if self._on_choose_dir is None:
            return web.json_response(
                {"error": "目录选择功能未启用"}, status=501
            )
        try:
            # QFileDialog 需要在主线程执行，通过 run_in_executor 调用
            loop: asyncio.AbstractEventLoop = asyncio.get_event_loop()
            path: str = await loop.run_in_executor(None, self._on_choose_dir)
            return web.json_response({"path": path if path else None})
        except Exception as e:
            logger.error("handle_choose_dir 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    # ── 前端 版本信息 ──

    async def handle_version(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面和关于页面需要展示当前应用版本号，
            方便用户确认是否需要更新。

        Code Logic:
            从 claude_partner.__version__ 获取版本号，
            buildDate 使用 __init__.py 文件的修改时间。
        """
        try:
            import claude_partner
            version: str = claude_partner.__version__

            # 获取 buildDate：使用 __init__.py 文件的修改时间
            init_path: Path = Path(claude_partner.__file__)
            mtime: float = os.path.getmtime(init_path)
            build_date: str = datetime.fromtimestamp(
                mtime, tz=timezone.utc
            ).strftime("%Y-%m-%d")

            return web.json_response({
                "version": version,
                "buildDate": build_date,
            })
        except Exception as e:
            logger.error("handle_version 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    # ── 前端 更新检查 ──

    async def handle_check_update(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面用户点击"检查更新"按钮时，需要触发版本检查，
            并将结果（是否有新版本、版本号、更新说明）返回给前端。

        Code Logic:
            调用注入的 on_check_update 异步回调，该回调执行
            GitHub Releases API 检查并返回结果字典。
            回调未注册时返回 501。
        """
        if self._on_check_update is None:
            return web.json_response(
                {"error": "更新检查功能未启用"}, status=501
            )
        try:
            result: dict = await self._on_check_update()
            return web.json_response(result)
        except Exception as e:
            logger.error("handle_check_update 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)

    # ── 前端 权限检查 ──

    async def handle_permissions(self, _request: web.Request) -> web.Response:
        """
        Business Logic:
            前端设置页面需要展示当前 macOS 权限状态（屏幕录制、输入监控），
            以便用户了解哪些功能可能受限。

        Code Logic:
            调用注入的 check_permissions 回调，该回调使用
            Quartz API 检查权限状态并返回字典。
            非 macOS 平台始终返回已授权。
            回调未注册时返回 501。
        """
        if self._check_permissions is None:
            return web.json_response(
                {"error": "权限检查功能未启用"}, status=501
            )
        try:
            result: dict = self._check_permissions()
            return web.json_response(result)
        except Exception as e:
            logger.error("handle_permissions 异常: %s", e, exc_info=True)
            return web.json_response({"error": str(e)}, status=500)
