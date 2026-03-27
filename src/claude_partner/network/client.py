# -*- coding: utf-8 -*-
"""HTTP 客户端模块：用于调用对端设备的 API。"""

import aiohttp
import logging

logger: logging.Logger = logging.getLogger(__name__)

# 默认请求超时（秒）
DEFAULT_TIMEOUT: int = 5


class PeerClient:
    """
    HTTP 客户端，调用对端 Claude Partner 实例的 API。

    Business Logic（为什么需要这个类）:
        P2P 架构中每个实例也是客户端，需要主动向其他设备发起请求：
        同步 pull/push、文件传输 init/chunk/status 等。

    Code Logic（这个类做什么）:
        封装 aiohttp.ClientSession，提供与 APIProtocol 中定义的每个端点
        一一对应的调用方法。使用懒初始化模式创建 session，避免在 __init__
        中访问事件循环（aiohttp 要求在 async 上下文中创建 session）。
    """

    def __init__(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            初始化客户端，session 延迟到第一次请求时创建。

        Code Logic（这个函数做什么）:
            初始化 _session 为 None，实际的 ClientSession 在 _get_session 中懒创建。
        """
        self._session: aiohttp.ClientSession | None = None

    async def _get_session(self) -> aiohttp.ClientSession:
        """
        Business Logic（为什么需要这个函数）:
            aiohttp.ClientSession 需要在运行中的事件循环内创建，
            且应复用同一个 session 以提升连接效率。

        Code Logic（这个函数做什么）:
            如果 _session 为 None 或已关闭，则创建新的 ClientSession
            （带超时配置）。否则返回现有的。
        """
        if self._session is None or self._session.closed:
            timeout: aiohttp.ClientTimeout = aiohttp.ClientTimeout(
                total=DEFAULT_TIMEOUT
            )
            self._session = aiohttp.ClientSession(timeout=timeout)
        return self._session

    async def health_check(self, base_url: str) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            同步前需要检查对端设备是否在线且 HTTP 服务正常。

        Code Logic（这个函数做什么）:
            GET /api/health，返回 True 表示对端可达，False 表示不可达。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            async with session.get(f"{base_url}/api/health") as resp:
                return resp.status == 200
        except Exception as e:
            logger.debug("health_check 失败 (%s): %s", base_url, e)
            return False

    async def sync_pull(
        self, base_url: str, local_summary: list[dict]
    ) -> list[dict]:
        """
        Business Logic（为什么需要这个函数）:
            向对端发送本端的 prompt 摘要，获取对端认为本端需要的 prompt 数据。

        Code Logic（这个函数做什么）:
            POST /api/sync/pull，请求体为 {summaries: local_summary}。
            返回对端响应中的 prompts 列表（dict 形式）。失败时返回空列表。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            payload: dict = {"summaries": local_summary}
            async with session.post(
                f"{base_url}/api/sync/pull", json=payload
            ) as resp:
                if resp.status == 200:
                    data: dict = await resp.json()
                    prompts: list[dict] = data.get("prompts", [])
                    logger.info(
                        "sync_pull 从 %s 获取 %d 条 prompt",
                        base_url,
                        len(prompts),
                    )
                    return prompts
                else:
                    logger.warning(
                        "sync_pull 失败 (%s): HTTP %d", base_url, resp.status
                    )
                    return []
        except Exception as e:
            logger.error("sync_pull 异常 (%s): %s", base_url, e)
            return []

    async def sync_push(
        self, base_url: str, prompts: list[dict]
    ) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            将本端有但对端缺少的 prompt 推送给对端设备。

        Code Logic（这个函数做什么）:
            POST /api/sync/push，请求体为 {prompts: [prompt_dict, ...]}。
            返回 True 表示对端成功接收，False 表示推送失败。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            payload: dict = {"prompts": prompts}
            async with session.post(
                f"{base_url}/api/sync/push", json=payload
            ) as resp:
                if resp.status == 200:
                    data: dict = await resp.json()
                    accepted: int = data.get("accepted", 0)
                    logger.info(
                        "sync_push 到 %s 成功，对端接收 %d 条",
                        base_url,
                        accepted,
                    )
                    return True
                else:
                    logger.warning(
                        "sync_push 失败 (%s): HTTP %d", base_url, resp.status
                    )
                    return False
        except Exception as e:
            logger.error("sync_push 异常 (%s): %s", base_url, e)
            return False

    async def transfer_init(
        self, base_url: str, metadata: dict
    ) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            发起文件传输前，需要向对端发送文件元数据以获取 transfer_id。

        Code Logic（这个函数做什么）:
            POST /api/transfer/init，请求体为 {filename, size, sha256, chunk_size}。
            返回对端响应的字典（含 transfer_id 等）。失败时返回含 error 的字典。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            async with session.post(
                f"{base_url}/api/transfer/init", json=metadata
            ) as resp:
                data: dict = await resp.json()
                return data
        except Exception as e:
            logger.error("transfer_init 异常 (%s): %s", base_url, e)
            return {"error": str(e)}

    async def transfer_chunk(
        self,
        base_url: str,
        transfer_id: str,
        offset: int,
        data: bytes,
    ) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            文件传输过程中，逐块发送文件数据到对端。

        Code Logic（这个函数做什么）:
            POST /api/transfer/chunk/{transfer_id}
            - offset 通过 X-Chunk-Offset header 传递
            - data 作为 raw bytes body 发送
            返回对端响应字典。失败时返回含 error 的字典。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            headers: dict[str, str] = {"X-Chunk-Offset": str(offset)}
            async with session.post(
                f"{base_url}/api/transfer/chunk/{transfer_id}",
                data=data,
                headers=headers,
            ) as resp:
                result: dict = await resp.json()
                return result
        except Exception as e:
            logger.error(
                "transfer_chunk 异常 (%s, transfer=%s): %s",
                base_url,
                transfer_id,
                e,
            )
            return {"error": str(e)}

    async def transfer_status(
        self, base_url: str, transfer_id: str
    ) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            文件传输过程中需要查询对端的接收进度和状态。

        Code Logic（这个函数做什么）:
            GET /api/transfer/status/{transfer_id}
            返回对端响应字典（含传输状态信息）。失败时返回含 error 的字典。
        """
        try:
            session: aiohttp.ClientSession = await self._get_session()
            async with session.get(
                f"{base_url}/api/transfer/status/{transfer_id}"
            ) as resp:
                data: dict = await resp.json()
                return data
        except Exception as e:
            logger.error(
                "transfer_status 异常 (%s, transfer=%s): %s",
                base_url,
                transfer_id,
                e,
            )
            return {"error": str(e)}

    async def close(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用关闭时需要释放 HTTP 连接资源。

        Code Logic（这个函数做什么）:
            关闭 aiohttp.ClientSession，释放底层 TCP 连接。
        """
        if self._session is not None and not self._session.closed:
            await self._session.close()
            self._session = None
            logger.info("PeerClient session 已关闭")
