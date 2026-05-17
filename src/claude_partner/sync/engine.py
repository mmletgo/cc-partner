# -*- coding: utf-8 -*-
"""同步引擎模块：协调 Prompt 在多设备间的同步流程。"""

from PyQt6.QtCore import QObject, pyqtSignal
import logging

from claude_partner.config import AppConfig
from claude_partner.storage.prompt_repo import PromptRepository
from claude_partner.network.client import PeerClient
from claude_partner.models.device import Device
from claude_partner.models.prompt import Prompt
from claude_partner.sync.merger import PromptMerger

logger: logging.Logger = logging.getLogger(__name__)


class SyncEngine(QObject):
    """
    Prompt 同步引擎，负责协调与所有对端设备的数据同步。

    Business Logic（为什么需要这个类）:
        多设备编辑 Prompt 时，需要一个中心协调器来管理同步流程：
        与谁同步、如何处理冲突。

    Code Logic（这个类做什么）:
        核心同步流程（sync_with_peer）:
        1. 获取本端 summary，POST sync/pull 到对端（带本端 summary）
        2. 对端返回本端需要的 prompts，逐个与本地对比，用 merger 决定更新
        3. 获取本端有但对端没有的 prompts，POST sync/push 到对端
        触发机制：由外部（UI 层）显式调用 sync_with_peer / sync_all 触发，
        用户在 Prompt 管理面板点击"同步"按钮时发起。
    """

    sync_completed = pyqtSignal()
    sync_error = pyqtSignal(str)

    def __init__(
        self,
        config: AppConfig,
        prompt_repo: PromptRepository,
        peer_client: PeerClient,
    ) -> None:
        """
        Business Logic（为什么需要这个函数）:
            同步引擎需要配置信息、本地数据仓库和网络客户端来执行同步。

        Code Logic（这个函数做什么）:
            保存依赖组件引用。
        """
        super().__init__()
        self._config: AppConfig = config
        self._prompt_repo: PromptRepository = prompt_repo
        self._peer_client: PeerClient = peer_client

    async def sync_with_peer(self, device: Device) -> None:
        """
        Business Logic（为什么需要这个函数）:
            与单个对端设备执行完整的双向同步，确保双方数据一致。

        Code Logic（这个函数做什么）:
            1. 获取本端 summary
            2. POST sync/pull 到对端（带本端 summary），获取对端返回的 prompts
            3. 对返回的 prompts 逐个和本地对比，用 PromptMerger 决定是否更新
            4. bulk_upsert 需要更新的 prompts
            5. 通过 diff_summaries 找出本端有但对端需要的 prompts
            6. POST sync/push 将这些 prompts 推送到对端
        """
        base_url: str = device.base_url()
        logger.info("开始与设备 %s (%s) 同步", device.name, base_url)

        try:
            # 1. 健康检查
            if not await self._peer_client.health_check(base_url):
                logger.warning("设备 %s 不可达，跳过同步", device.name)
                return

            # 2. 获取本端摘要
            local_summary: list[dict] = await self._prompt_repo.get_sync_summary()

            # 3. Pull: 发送本端摘要给对端，获取对端认为我们需要的 prompts
            remote_prompts_dicts: list[dict] = await self._peer_client.sync_pull(
                base_url, local_summary
            )

            # 4. 处理拉取到的 prompts
            if remote_prompts_dicts:
                prompts_to_upsert: list[Prompt] = []

                for remote_dict in remote_prompts_dicts:
                    remote_prompt: Prompt = Prompt.from_dict(remote_dict)
                    local_prompt: Prompt | None = await self._prompt_repo.get_by_id(
                        remote_prompt.id
                    )

                    if local_prompt is None:
                        # 本地没有，直接接收
                        prompts_to_upsert.append(remote_prompt)
                    else:
                        # 本地有，用 merger 决定合并结果
                        merged: Prompt = PromptMerger.merge_prompt(
                            local_prompt, remote_prompt
                        )
                        # 只有当合并结果与本地不同时才更新
                        if (
                            merged.vector_clock != local_prompt.vector_clock
                            or merged.updated_at != local_prompt.updated_at
                            or merged.content != local_prompt.content
                            or merged.title != local_prompt.title
                            or merged.deleted != local_prompt.deleted
                        ):
                            prompts_to_upsert.append(merged)

                if prompts_to_upsert:
                    await self._prompt_repo.bulk_upsert(prompts_to_upsert)
                    logger.info(
                        "从 %s 拉取并更新了 %d 条 prompt",
                        device.name,
                        len(prompts_to_upsert),
                    )

            # 5. Push: 找出需要推送给对端的 prompts
            # 重新获取本端摘要（可能被 pull 阶段更新了）
            local_summary = await self._prompt_repo.get_sync_summary()
            _, need_push_ids = PromptMerger.diff_summaries(
                local_summary,
                # 对端的摘要可以从 pull 返回的数据中推导，但更简单的做法是
                # 用对端返回的 prompts 构建摘要
                [
                    {"id": d["id"], "vector_clock": d["vector_clock"]}
                    for d in remote_prompts_dicts
                ]
                if remote_prompts_dicts
                else [],
            )

            # 不过更准确的方式是：本端有而对端 pull 未返回的（说明对端不需要的
            # 可能对端已经有了）。这里用更保守的做法：推送所有本端独有的。
            # 由于 sync/pull 的对端逻辑已经过滤了对端已有的，这里只推送
            # 本端独有（对端摘要中没有的）的 prompts
            remote_ids: set[str] = {d["id"] for d in remote_prompts_dicts} if remote_prompts_dicts else set()
            push_prompts: list[dict] = []
            for s in local_summary:
                if s["id"] not in remote_ids:
                    # 这个 prompt 对端可能没有，推送过去
                    prompt: Prompt | None = await self._prompt_repo.get_by_id(s["id"])
                    if prompt is not None:
                        push_prompts.append(prompt.to_dict())

            if push_prompts:
                success: bool = await self._peer_client.sync_push(
                    base_url, push_prompts
                )
                if success:
                    logger.info(
                        "向 %s 推送了 %d 条 prompt",
                        device.name,
                        len(push_prompts),
                    )
                else:
                    logger.warning("向 %s 推送 prompt 失败", device.name)

            logger.info("与设备 %s 同步完成", device.name)

        except Exception as e:
            error_msg: str = f"与设备 {device.name} 同步失败: {e}"
            logger.error(error_msg, exc_info=True)
            self.sync_error.emit(error_msg)

    async def sync_all(self, devices: dict[str, Device]) -> None:
        """
        Business Logic（为什么需要这个函数）:
            某些事件（如定时触发、本地变更后）需要与所有在线设备同步。

        Code Logic（这个函数做什么）:
            遍历所有在线设备，依次调用 sync_with_peer。
            全部成功后 emit sync_completed 信号。
            任一失败会 emit sync_error 但不中断其他设备的同步。
        """
        if not devices:
            logger.debug("没有在线设备，跳过同步")
            return

        logger.info("开始与 %d 个设备同步", len(devices))

        for device_id, device in devices.items():
            try:
                await self.sync_with_peer(device)
            except Exception as e:
                logger.error(
                    "与设备 %s 同步异常: %s", device.name, e, exc_info=True
                )
                self.sync_error.emit(f"与设备 {device.name} 同步异常: {e}")

        self.sync_completed.emit()

    async def stop(self) -> None:
        """
        Business Logic（为什么需要这个函数）:
            应用关闭时的占位钩子，保持与 app.py 调用接口的一致性。

        Code Logic（这个函数做什么）:
            无异步任务需要停止（已无定时/防抖 task），仅记录日志。
        """
        logger.info("同步引擎已停止")
