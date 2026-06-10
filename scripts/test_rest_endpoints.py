#!/usr/bin/env python3
"""
REST 端点集成测试 - 真实启动 aiohttp + Database，验证 protocol.py 新增端点。

Business Logic:
    protocol.py 新增了 /api/prompts (CRUD)、/api/devices、/api/transfer/* 等
    供前端调用的 REST 端点。需要在真实 HTTP 服务器上跑一遍端到端验证。

Code Logic:
    - 创建临时 SQLite 数据库
    - 启动 HTTPServer + APIProtocol（注入 sender/receiver/get_devices 回调）
    - 用 aiohttp ClientSession 异步请求所有新端点
    - 退出码 0 = 全部通过；非 0 = 有失败
"""

from __future__ import annotations

import asyncio
import json
import sys
import tempfile
from pathlib import Path
from datetime import datetime, timezone

SRC = Path(__file__).resolve().parents[1] / "src"
sys.path.insert(0, str(SRC))

from aiohttp import ClientSession, web

from claude_partner.config import AppConfig
from claude_partner.models.device import Device
from claude_partner.models.prompt import Prompt
from claude_partner.models.transfer import TransferDirection, TransferStatus, TransferTask
from claude_partner.network.protocol import APIProtocol
from claude_partner.network.server import HTTPServer
from claude_partner.storage.database import Database
from claude_partner.storage.prompt_repo import PromptRepository
from claude_partner.transfer.sender import FileSender
from claude_partner.transfer.receiver import FileReceiver


class MockDiscovery:
    def __init__(self) -> None:
        now = datetime.now(timezone.utc)
        self._devices: dict[str, Device] = {
            "dev-1": Device(id="dev-1", name="Test iMac", host="192.168.1.50",
                            port=7891, last_seen=now, online=True),
            "dev-2": Device(id="dev-2", name="Test MBP", host="192.168.1.51",
                            port=7891, last_seen=now, online=False),
        }

    def get_devices(self) -> dict[str, Device]:
        return self._devices


class MockPeerClient:
    """Mock PeerClient - 不真发 HTTP，只让 send_file 不报错"""


def _discovery_to_list(discovery: MockDiscovery) -> list[dict]:
    return [
        {"id": d.id, "name": d.name, "address": d.host, "port": d.port,
         "status": "online" if d.online else "offline",
         "lastSeen": d.last_seen.isoformat()}
        for d in discovery.get_devices().values()
    ]


async def run_tests() -> int:
    # ── 1. 准备临时数据库 ──
    tmpdir = Path(tempfile.mkdtemp(prefix="cp-test-"))
    db_path = tmpdir / "test.db"
    receive_dir = tmpdir / "recv"
    receive_dir.mkdir()

    config = AppConfig(device_id="self-test-001", device_name="Integration Test",
                       http_port=0, db_path=str(db_path), receive_dir=str(receive_dir))
    db = Database(str(db_path))
    await db.initialize()
    repo = PromptRepository(db)

    now = datetime.now(timezone.utc)
    for pid, title, content, tag, dev_id, clock in [
        ("p-1", "翻译助手", "中译英", "work", "self-test-001", 3),
        ("p-2", "代码审查", "review pr", "code", "self-test-001", 2),
        ("p-3", "会议纪要", "note", "work", "self-test-001", 1),
    ]:
        await repo.create(Prompt(id=pid, title=title, content=content, tags=[tag],
                                 created_at=now, updated_at=now, device_id=dev_id,
                                 vector_clock={dev_id: clock}, deleted=False))

    discovery = MockDiscovery()
    sender = FileSender(MockPeerClient())  # type: ignore[arg-type]
    receiver = FileReceiver(config)
    sender._tasks["t-send-1"] = TransferTask(
        id="t-send-1", filename="test.bin", file_path="/tmp/test.bin",
        size=1024, sha256="abc", chunk_size=960*1024,
        direction=TransferDirection.SEND, peer_device_id="dev-1",
        status=TransferStatus.TRANSFERRING, transferred_bytes=512,
        created_at=now,
    )

    protocol = APIProtocol(
        config=config, prompt_repo=repo,
        on_transfer_init=receiver.init_transfer,
        on_transfer_chunk=receiver.receive_chunk,
        get_transfer_status=receiver.get_transfer_status,
        get_devices=lambda: _discovery_to_list(discovery),
        on_transfer_send=sender.send_file,
        on_transfer_cancel=lambda tid: bool(sender.get_task(tid) or receiver.get_task(tid)),
        get_transfers=lambda: [
            t.to_dict() for t in sender.list_tasks() + receiver.list_tasks()
        ],
    )

    server = HTTPServer(protocol)
    port = await server.start(0)
    base = f"http://127.0.0.1:{port}"
    print(f"✅ Server started on {base}")

    failures: list[str] = []

    async def check(name: str, expected: int, resp) -> dict:
        status = resp.status
        body = await resp.json()
        if status == expected:
            print(f"  ✅ {name}: {status}")
        else:
            print(f"  ❌ {name}: expected {expected}, got {status}: {body}")
            failures.append(name)
        return body

    async with ClientSession(base) as sess:
        # ── health ──
        print("\n[GET] /api/health")
        async with sess.get("/api/health") as r:
            await check("health", 200, r)

        # ── prompts list ──
        print("\n[GET] /api/prompts")
        async with sess.get("/api/prompts") as r:
            b = await check("list_prompts", 200, r)
            assert len(b) == 3

        # ── prompts search ──
        print("\n[GET] /api/prompts?search=翻译")
        async with sess.get("/api/prompts", params={"search": "翻译"}) as r:
            b = await check("search_prompts", 200, r)
            assert len(b) == 1

        # ── prompts filter by tag ──
        print("\n[GET] /api/prompts?tag=code")
        async with sess.get("/api/prompts", params={"tag": "code"}) as r:
            b = await check("filter_by_tag", 200, r)
            assert len(b) == 1

        # ── create prompt ──
        print("\n[POST] /api/prompts")
        async with sess.post("/api/prompts", json={"title": "新 prompt", "content": "测试", "tag": "test"}) as r:
            b = await check("create_prompt", 201, r)
            new_id = b["id"]

        # ── get prompt ──
        print("\n[GET] /api/prompts/{id}")
        async with sess.get(f"/api/prompts/{new_id}") as r:
            b = await check("get_prompt", 200, r)
            assert b["title"] == "新 prompt"

        # ── update prompt ──
        print("\n[PUT] /api/prompts/{id}")
        async with sess.put(f"/api/prompts/{new_id}", json={"title": "已修改"}) as r:
            b = await check("update_prompt", 200, r)
            assert b["title"] == "已修改"
            assert b["vectorClock"]["self-test-001"] == 2  # 创建时 1，更新 +1 → 2

        # ── delete prompt ──
        print("\n[DELETE] /api/prompts/{id}")
        async with sess.delete(f"/api/prompts/{new_id}") as r:
            await check("delete_prompt", 200, r)

        # ── get deleted (404) ──
        print("\n[GET] /api/prompts/{id} (已删除)")
        async with sess.get(f"/api/prompts/{new_id}") as r:
            await check("get_deleted_404", 404, r)

        # ── devices ──
        print("\n[GET] /api/devices")
        async with sess.get("/api/devices") as r:
            b = await check("list_devices", 200, r)
            assert len(b) == 2
            assert b[0]["status"] in ("online", "offline")

        # ── transfers ──
        print("\n[GET] /api/transfer/tasks")
        async with sess.get("/api/transfer/tasks") as r:
            b = await check("list_transfers", 200, r)
            assert len(b) == 1
            assert b[0]["filename"] == "test.bin"

        # ── transfer send (online) ──
        print("\n[POST] /api/transfer/send (在线)")
        async with sess.post("/api/transfer/send", json={"deviceId": "dev-1", "filePath": "/tmp/fake.bin"}) as r:
            await check("send_online", 202, r)

        # ── transfer send (offline/unknown) ──
        print("\n[POST] /api/transfer/send (离线)")
        async with sess.post("/api/transfer/send", json={"deviceId": "dev-999", "filePath": "/tmp/fake.bin"}) as r:
            await check("send_offline", 404, r)

        # ── cancel transfer ──
        print("\n[DELETE] /api/transfer/tasks/t-send-1")
        async with sess.delete("/api/transfer/tasks/t-send-1") as r:
            await check("cancel_existing", 200, r)

        print("\n[DELETE] /api/transfer/tasks/nonexistent")
        async with sess.delete("/api/transfer/tasks/nonexistent") as r:
            await check("cancel_missing", 404, r)

        # ── sync ──
        print("\n[POST] /api/sync")
        async with sess.post("/api/sync", json={}) as r:
            b = await check("sync_all", 200, r)
            assert b["accepted"] is True

        # ── sync/pull ──
        print("\n[POST] /api/sync/pull")
        async with sess.post("/api/sync/pull", json={"summaries": []}) as r:
            await check("sync_pull", 200, r)

        # ── sync/push ──
        print("\n[POST] /api/sync/push")
        async with sess.post("/api/sync/push", json={"prompts": []}) as r:
            await check("sync_push", 200, r)

    await server.stop()
    await db.close()

    print("\n" + "=" * 60)
    if failures:
        print(f"❌ {len(failures)} 个测试失败:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("✅ 全部 16 个端点测试通过")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(run_tests()))
