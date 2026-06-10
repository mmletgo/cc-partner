# -*- coding: utf-8 -*-
"""文件接收模块：负责接收对端发送的文件块并组装为完整文件。"""

from __future__ import annotations

import hashlib
import logging
import os
import uuid
from datetime import datetime
from pathlib import Path

from PyQt6.QtCore import QObject, pyqtSignal

from claude_partner.config import AppConfig
from claude_partner.models.transfer import (
    TransferDirection,
    TransferStatus,
    TransferTask,
)

logger: logging.Logger = logging.getLogger(__name__)


class FileReceiver(QObject):
    """
    文件接收器，负责接收对端发送的文件并保存到本地。

    Business Logic（为什么需要这个类）:
        当对端设备向本机发送文件时，需要处理传输初始化、分块接收、
        完整性校验和文件保存的全部流程，同时支持断点续传。

    Code Logic（这个类做什么）:
        管理接收中的传输任务，将收到的数据块写入临时文件，
        传输完成后校验 SHA256 并重命名为目标文件名。
        通过 Qt 信号通知 UI 层进度和状态变更。
    """

    transfer_initiated = pyqtSignal(object)             # TransferTask（新传输请求到达）
    progress_updated = pyqtSignal(str, float)          # (transfer_id, progress 0.0~1.0)
    transfer_completed = pyqtSignal(str, str)          # (transfer_id, saved_path)
    transfer_failed = pyqtSignal(str, str)             # (transfer_id, error_message)

    def __init__(self, config: AppConfig) -> None:
        """
        Business Logic（为什么需要这个函数）:
            接收器需要知道文件保存目录等配置信息。

        Code Logic（这个函数做什么）:
            保存配置引用，初始化任务字典，确保接收目录存在。
        """
        super().__init__()
        self._config: AppConfig = config
        self._tasks: dict[str, TransferTask] = {}
        os.makedirs(config.receive_dir, exist_ok=True)

    def init_transfer(self, metadata: dict) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            对端发起传输请求时，接收端需要确认是否接受、创建任务记录，
            并告知对端从哪个 offset 开始发送（支持断点续传）。

        Code Logic（这个函数做什么）:
            1. 从 metadata 提取文件信息，生成或使用已有的 transfer_id
            2. 创建 TransferTask 并记录
            3. 检查临时文件 .{transfer_id}.tmp 是否已存在，
               已存在则返回其大小作为 resume_offset
            4. 返回 {transfer_id, accepted, resume_offset}
        """
        transfer_id: str = metadata.get("transfer_id", str(uuid.uuid4()))
        filename: str = metadata["filename"]
        file_size: int = metadata["size"]
        sha256: str = metadata["sha256"]
        chunk_size: int = metadata.get("chunk_size", 1024 * 1024)

        # 临时文件路径
        tmp_path: str = os.path.join(
            self._config.receive_dir, f".{transfer_id}.tmp"
        )

        # 检查断点续传
        resume_offset: int = 0
        if os.path.exists(tmp_path):
            resume_offset = os.path.getsize(tmp_path)
            logger.info(
                "发现临时文件，断点续传: %s, offset=%d", transfer_id, resume_offset
            )

        task = TransferTask(
            id=transfer_id,
            filename=filename,
            file_path=tmp_path,
            size=file_size,
            sha256=sha256,
            chunk_size=chunk_size,
            direction=TransferDirection.RECEIVE,
            peer_device_id=metadata.get("peer_device_id", ""),
            status=TransferStatus.PENDING,
            transferred_bytes=resume_offset,
            created_at=datetime.now(),
        )
        self._tasks[transfer_id] = task

        logger.info(
            "接受传输请求: %s, 文件=%s, 大小=%d", transfer_id, filename, file_size
        )

        # 通知 UI 创建接收任务卡片
        self.transfer_initiated.emit(task)

        return {
            "transfer_id": transfer_id,
            "accepted": True,
            "resume_offset": resume_offset,
        }

    async def receive_chunk(
        self, transfer_id: str, offset: int, data: bytes
    ) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            对端逐块发送文件数据，接收端需要将每块数据写入正确的文件位置，
            并在所有数据接收完毕后自动完成校验和保存。

        Code Logic（这个函数做什么）:
            1. 查找对应的传输任务
            2. 打开临时文件，seek 到指定 offset，写入数据
            3. 更新已传输字节数并发射进度信号
            4. 如果已接收完所有数据，调用 finalize_transfer 完成校验
            5. 返回 {success, received_bytes}
        """
        task: TransferTask | None = self._tasks.get(transfer_id)
        if task is None:
            logger.error("未找到传输任务: %s", transfer_id)
            return {"success": False, "received_bytes": 0}

        task.status = TransferStatus.TRANSFERRING

        try:
            tmp_path: str = task.file_path
            with open(tmp_path, "r+b" if os.path.exists(tmp_path) else "wb") as f:
                f.seek(offset)
                f.write(data)

            task.transferred_bytes = offset + len(data)
            self.progress_updated.emit(transfer_id, task.progress())

            # 检查是否接收完毕
            if task.transferred_bytes >= task.size:
                await self.finalize_transfer(transfer_id)

            return {"success": True, "received_bytes": task.transferred_bytes}

        except Exception as e:
            error_msg: str = str(e)
            task.status = TransferStatus.FAILED
            self.transfer_failed.emit(transfer_id, error_msg)
            logger.error("接收数据块失败: %s, 错误: %s", transfer_id, error_msg)
            return {"success": False, "received_bytes": task.transferred_bytes}

    async def finalize_transfer(self, transfer_id: str) -> bool:
        """
        Business Logic（为什么需要这个函数）:
            文件全部接收后需要校验完整性（SHA256），确保传输无误后
            将临时文件重命名为最终文件名，处理文件名冲突。

        Code Logic（这个函数做什么）:
            1. 计算临时文件的 SHA256 并与预期值比较
            2. 校验通过后调用 _resolve_filename 获取不重复的文件名
            3. 重命名临时文件为最终文件
            4. 更新任务状态并发射完成信号
            5. 校验失败则标记任务为失败并删除临时文件
        """
        task: TransferTask | None = self._tasks.get(transfer_id)
        if task is None:
            logger.error("finalize 时未找到传输任务: %s", transfer_id)
            return False

        tmp_path: str = task.file_path

        # 校验 SHA256
        h = hashlib.sha256()
        with open(tmp_path, "rb") as f:
            while True:
                block: bytes = f.read(8192)  # 8KB
                if not block:
                    break
                h.update(block)

        actual_sha256: str = h.hexdigest()
        if actual_sha256 != task.sha256:
            error_msg: str = (
                f"SHA256 校验失败: 期望={task.sha256}, 实际={actual_sha256}"
            )
            task.status = TransferStatus.FAILED
            self.transfer_failed.emit(transfer_id, error_msg)
            logger.error(error_msg)
            # 删除损坏的临时文件
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            return False

        # 解决文件名冲突并重命名
        final_filename: str = self._resolve_filename(task.filename)
        final_path: str = os.path.join(self._config.receive_dir, final_filename)
        os.rename(tmp_path, final_path)

        task.file_path = final_path
        task.status = TransferStatus.COMPLETED
        task.completed_at = datetime.now()
        self.transfer_completed.emit(transfer_id, final_path)
        logger.info("文件接收完成: %s -> %s", transfer_id, final_path)
        return True

    def get_transfer_status(self, transfer_id: str) -> dict:
        """
        Business Logic（为什么需要这个函数）:
            UI 或对端可能需要查询某个传输任务的当前状态。

        Code Logic（这个函数做什么）:
            根据 transfer_id 查找任务，返回包含状态、进度等信息的字典。
            任务不存在时返回 error 信息。
        """
        task: TransferTask | None = self._tasks.get(transfer_id)
        if task is None:
            return {"error": "传输任务不存在", "transfer_id": transfer_id}

        return {
            "transfer_id": transfer_id,
            "status": task.status.value,
            "progress": task.progress(),
            "transferred_bytes": task.transferred_bytes,
            "size": task.size,
            "filename": task.filename,
        }

    def cancel(self, transfer_id: str) -> None:
        """
        Business Logic（为什么需要这个函数）:
            用户可能决定取消正在接收的文件，需要清理任务和临时文件。

        Code Logic（这个函数做什么）:
            将任务标记为已取消，删除对应的临时文件。
        """
        task: TransferTask | None = self._tasks.get(transfer_id)
        if task is None:
            return

        task.status = TransferStatus.CANCELLED
        # 删除临时文件
        tmp_path: str = os.path.join(
            self._config.receive_dir, f".{transfer_id}.tmp"
        )
        if os.path.exists(tmp_path):
            os.remove(tmp_path)
            logger.info("已删除临时文件: %s", tmp_path)
        logger.info("接收传输已取消: %s", transfer_id)

    def list_tasks(self) -> list[TransferTask]:
        """
        Business Logic（为什么需要这个函数）:
            前端传输面板需要展示所有接收任务（含已结束的历史任务）的列表。

        Code Logic（这个函数做什么）:
            返回内部 _tasks 字典的 values，按 created_at 倒序。
        """
        return sorted(self._tasks.values(), key=lambda t: t.created_at, reverse=True)

    def get_task(self, transfer_id: str) -> TransferTask | None:
        """
        Business Logic（为什么需要这个函数）:
            取消或查询单个接收任务时需要根据 ID 获取 TransferTask 实例。

        Code Logic（这个函数做什么）:
            从 _tasks 字典中查询；不存在时返回 None。
        """
        return self._tasks.get(transfer_id)

    def _resolve_filename(self, filename: str) -> str:
        """
        Business Logic（为什么需要这个函数）:
            接收目录中可能已存在同名文件，需要自动生成不重复的文件名
            避免覆盖已有文件。

        Code Logic（这个函数做什么）:
            检查 receive_dir 下是否存在同名文件，若存在则在文件名
            后添加 (1), (2)... 后缀直到找到不重复的名称。
            例如: file.txt -> file (1).txt -> file (2).txt
        """
        target: Path = Path(self._config.receive_dir) / filename
        if not target.exists():
            return filename

        stem: str = target.stem
        suffix: str = target.suffix
        counter: int = 1
        while True:
            new_name: str = f"{stem} ({counter}){suffix}"
            if not (Path(self._config.receive_dir) / new_name).exists():
                return new_name
            counter += 1
