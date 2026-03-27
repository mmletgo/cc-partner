# -*- coding: utf-8 -*-
"""文件发送模块：负责将本地文件分块发送到对端设备。"""

from __future__ import annotations

import asyncio
import hashlib
import logging
import os
import uuid
from datetime import datetime
from typing import TYPE_CHECKING

from PyQt6.QtCore import QObject, pyqtSignal

from claude_partner.models.transfer import (
    TransferDirection,
    TransferStatus,
    TransferTask,
)

if TYPE_CHECKING:
    from claude_partner.network.client import PeerClient

logger: logging.Logger = logging.getLogger(__name__)

CHUNK_SIZE: int = 1024 * 1024  # 1MB


class FileSender(QObject):
    """
    文件发送器，负责将本地文件分块发送到对端。

    Business Logic（为什么需要这个类）:
        用户需要在局域网内向其他设备发送文件，发送器封装了
        文件哈希计算、初始化握手、分块传输、断点续传和取消等全部逻辑。

    Code Logic（这个类做什么）:
        使用 PeerClient 进行 HTTP 通信，通过 transfer_init 协商传输参数，
        然后逐块读取文件并通过 transfer_chunk 发送。通过 Qt 信号通知
        UI 层传输进度、完成和失败事件。
    """

    progress_updated = pyqtSignal(str, float)   # (transfer_id, progress 0.0~1.0)
    transfer_completed = pyqtSignal(str)         # transfer_id
    transfer_failed = pyqtSignal(str, str)       # (transfer_id, error_message)

    def __init__(self, peer_client: PeerClient) -> None:
        """
        Business Logic（为什么需要这个函数）:
            发送器创建时需要绑定网络客户端，用于后续的 HTTP 通信。

        Code Logic（这个函数做什么）:
            保存 PeerClient 引用，初始化任务字典和取消集合。
        """
        super().__init__()
        self._peer_client: PeerClient = peer_client
        self._tasks: dict[str, TransferTask] = {}
        self._cancelled: set[str] = set()

    async def send_file(
        self,
        file_path: str,
        peer_base_url: str,
        peer_device_id: str,
    ) -> TransferTask:
        """
        Business Logic（为什么需要这个函数）:
            用户选择文件并指定目标设备后，需要完整执行从哈希计算到分块传输的全流程，
            并支持断点续传和中途取消。

        Code Logic（这个函数做什么）:
            1. 计算文件 SHA256
            2. 调用 transfer_init 发送元数据并获取 transfer_id 和 resume_offset
            3. 从 resume_offset 处开始逐块读取文件并发送
            4. 每发送一块后更新进度并检查取消标志
            5. 全部发送完毕后标记任务为已完成
        """
        file_size: int = os.path.getsize(file_path)
        filename: str = os.path.basename(file_path)

        logger.info("开始计算文件 SHA256: %s", filename)
        sha256: str = self._calculate_sha256(file_path)

        transfer_id: str = str(uuid.uuid4())
        task = TransferTask(
            id=transfer_id,
            filename=filename,
            file_path=file_path,
            size=file_size,
            sha256=sha256,
            chunk_size=CHUNK_SIZE,
            direction=TransferDirection.SEND,
            peer_device_id=peer_device_id,
            status=TransferStatus.PENDING,
            transferred_bytes=0,
            created_at=datetime.now(),
        )
        self._tasks[transfer_id] = task

        try:
            # 初始化传输
            metadata: dict = {
                "transfer_id": transfer_id,
                "filename": filename,
                "size": file_size,
                "sha256": sha256,
                "chunk_size": CHUNK_SIZE,
            }
            init_resp: dict = await self._peer_client.transfer_init(
                peer_base_url, metadata
            )

            if "error" in init_resp and init_resp["error"]:
                raise RuntimeError(f"连接对端失败: {init_resp['error']}")
            if not init_resp.get("accepted", False):
                raise RuntimeError("对端拒绝接收文件")

            # 断点续传：从对端告知的 offset 处继续
            resume_offset: int = init_resp.get("resume_offset", 0)
            task.transferred_bytes = resume_offset
            task.status = TransferStatus.TRANSFERRING

            logger.info(
                "传输初始化完成: %s, resume_offset=%d", transfer_id, resume_offset
            )

            # 逐块发送
            with open(file_path, "rb") as f:
                f.seek(resume_offset)
                offset: int = resume_offset

                while offset < file_size:
                    # 检查是否被取消
                    if transfer_id in self._cancelled:
                        task.status = TransferStatus.CANCELLED
                        logger.info("传输已取消: %s", transfer_id)
                        self._cancelled.discard(transfer_id)
                        return task

                    read_size: int = min(CHUNK_SIZE, file_size - offset)
                    chunk_data: bytes = f.read(read_size)

                    await self._peer_client.transfer_chunk(
                        peer_base_url, transfer_id, offset, chunk_data
                    )

                    offset += len(chunk_data)
                    task.transferred_bytes = offset
                    self.progress_updated.emit(transfer_id, task.progress())

                    # 让出事件循环，避免阻塞 UI
                    await asyncio.sleep(0)

            # 传输完成
            task.status = TransferStatus.COMPLETED
            task.completed_at = datetime.now()
            self.transfer_completed.emit(transfer_id)
            logger.info("文件发送完成: %s", transfer_id)

        except Exception as e:
            task.status = TransferStatus.FAILED
            error_msg: str = str(e)
            self.transfer_failed.emit(transfer_id, error_msg)
            logger.error("文件发送失败: %s, 错误: %s", transfer_id, error_msg)

        return task

    def _calculate_sha256(self, file_path: str) -> str:
        """
        Business Logic（为什么需要这个函数）:
            文件传输完成后需要校验完整性，发送端需要预先计算 SHA256 并
            随元数据发送给接收端。

        Code Logic（这个函数做什么）:
            以 8KB 块逐步读取文件并更新 SHA256 哈希，避免大文件一次性读入内存。
        """
        h = hashlib.sha256()
        with open(file_path, "rb") as f:
            while True:
                block: bytes = f.read(8192)  # 8KB
                if not block:
                    break
                h.update(block)
        return h.hexdigest()

    def cancel(self, transfer_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户可能在传输过程中决定取消，需要一种机制安全地中断正在进行的传输。

        Code Logic（这个函数做什么）:
            将 transfer_id 加入取消集合，send_file 循环中会检查该集合并停止传输。
        """
        self._cancelled.add(transfer_id)
        logger.info("请求取消传输: %s", transfer_id)
