#!/usr/bin/env python3
"""
Mock API server - 模拟 aiohttp 后端响应，验证前端真数据联调。

Business Logic:
    前端页面调用的 /api/prompts, /api/devices, /api/transfer/tasks 等端点
    在真实后端 protocol.py 中尚未实现，本脚本用最小 aiohttp app 提供
    这些端点的 mock 响应，验证前端 fetch 链路通畅。

Code Logic:
    - 启动 aiohttp web.Application
    - 注册 /api/health, /api/prompts, /api/devices, /api/transfer/tasks 端点
    - 返回符合 web/src/lib/types.ts 的 mock 数据
    - 监听 0.0.0.0:8765（避开 aiohttp 默认 8000）
"""

from __future__ import annotations

import asyncio
import logging
import sys
import time
from pathlib import Path

from aiohttp import web

logger: logging.Logger = logging.getLogger("mock_api")
logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(name)s] %(message)s")


MOCK_PROMPTS = [
    {
        "id": "p-001",
        "title": "翻译为学术英语",
        "content": "把任意中文段落改写成 Nature/Science 风格的英文学术英文，保留术语与被动语态。",
        "tag": "work",
        "updatedAt": "2026-06-09T15:30:00Z",
        "vectorClock": {"device-aaa": 3},
    },
    {
        "id": "p-002",
        "title": "总结长文为要点",
        "content": "把一篇 5000 字以上的长文压缩为 5-8 条可执行要点，附带原文出处行号。",
        "tag": "work",
        "updatedAt": "2026-06-09T14:10:00Z",
        "vectorClock": {"device-aaa": 2},
    },
    {
        "id": "p-003",
        "title": "代码审查 v2",
        "content": "逐行审查 PR diff，按 Bug / 性能 / 风格分组，每条给出可粘贴的修改建议。",
        "tag": "code",
        "updatedAt": "2026-06-08T22:00:00Z",
        "vectorClock": {"device-aaa": 5},
    },
    {
        "id": "p-004",
        "title": "写单元测试",
        "content": "为给定函数生成 pytest / vitest 用例，覆盖 happy path + 边界 + 异常分支。",
        "tag": "code",
        "updatedAt": "2026-06-08T18:45:00Z",
        "vectorClock": {"device-aaa": 1},
    },
    {
        "id": "p-005",
        "title": "解释正则表达式",
        "content": "用一段中文大白话讲清楚给定正则每一段在匹配什么，附 3 个匹配/不匹配示例。",
        "tag": "code",
        "updatedAt": "2026-06-07T11:20:00Z",
        "vectorClock": {"device-aaa": 2},
    },
    {
        "id": "p-006",
        "title": "周报生成器",
        "content": "根据本周 git commit 列表自动生成结构化周报：完成项 / 进行中 / 风险。",
        "tag": "work",
        "updatedAt": "2026-06-06T17:00:00Z",
        "vectorClock": {"device-aaa": 4},
    },
    {
        "id": "p-007",
        "title": "面试问题设计",
        "content": "针对给定岗位 JD 和候选人简历，生成 5 道深度技术题 + 3 道软素质题。",
        "tag": "personal",
        "updatedAt": "2026-06-05T09:30:00Z",
        "vectorClock": {"device-aaa": 1},
    },
    {
        "id": "p-008",
        "title": "重构建议",
        "content": "阅读一段老代码，识别代码异味，给出最小破坏的重构方案（保留 API 不变）。",
        "tag": "code",
        "updatedAt": "2026-06-04T14:15:00Z",
        "vectorClock": {"device-aaa": 2},
    },
    {
        "id": "p-009",
        "title": "会议纪要整理",
        "content": "从会议录音转写文本中提取：议题、决策、Action Item（含负责人 + 截止日）。",
        "tag": "work",
        "updatedAt": "2026-06-03T16:00:00Z",
        "vectorClock": {"device-aaa": 1},
    },
    {
        "id": "p-010",
        "title": "和 Claude 对话开场",
        "content": "简短说明项目背景 + 当前任务，让 Claude 立即进入工作状态。",
        "tag": "claude",
        "updatedAt": "2026-06-02T10:00:00Z",
        "vectorClock": {"device-aaa": 1},
    },
]

MOCK_DEVICES = [
    {
        "id": "device-bbb",
        "name": "Hans's iMac Studio",
        "address": "192.168.1.45",
        "port": 7891,
        "status": "online",
        "lastSeen": "2026-06-10T21:00:00Z",
    },
    {
        "id": "device-ccc",
        "name": "MBP 16 M3",
        "address": "192.168.1.78",
        "port": 7891,
        "status": "online",
        "lastSeen": "2026-06-10T21:05:00Z",
    },
    {
        "id": "device-ddd",
        "name": "Ubuntu Server",
        "address": "192.168.1.100",
        "port": 7891,
        "status": "offline",
        "lastSeen": "2026-06-10T18:30:00Z",
    },
]

MOCK_TRANSFERS = [
    {
        "id": "t-001",
        "fileName": "claude-partner-v0.4.2.zip",
        "filePath": "/Users/hans/Downloads/claude-partner-v0.4.2.zip",
        "fileSize": 28_400_000,
        "direction": "send",
        "status": "transferring",
        "progress": 0.62,
        "peerDeviceId": "device-bbb",
        "peerDeviceName": "Hans's iMac Studio",
        "speed": 1_200_000,
        "startedAt": "2026-06-10T21:08:00Z",
    },
    {
        "id": "t-002",
        "fileName": "design-spec.pdf",
        "filePath": "/Users/hans/Projects/design-spec.pdf",
        "fileSize": 2_300_000,
        "direction": "send",
        "status": "transferring",
        "progress": 0.34,
        "peerDeviceId": "device-ccc",
        "peerDeviceName": "MBP 16 M3",
        "speed": 800_000,
        "startedAt": "2026-06-10T21:10:00Z",
    },
    {
        "id": "t-003",
        "fileName": "screenshot-2026-06-10.png",
        "filePath": "/Users/hans/Pictures/screenshot-2026-06-10.png",
        "fileSize": 1_100_000,
        "direction": "receive",
        "status": "completed",
        "progress": 1.0,
        "peerDeviceId": "device-bbb",
        "peerDeviceName": "Hans's iMac Studio",
        "startedAt": "2026-06-10T20:50:00Z",
        "completedAt": "2026-06-10T20:51:00Z",
    },
    {
        "id": "t-004",
        "fileName": "build-log.txt",
        "filePath": "/Users/hans/Projects/build-log.txt",
        "fileSize": 45_000,
        "direction": "send",
        "status": "completed",
        "progress": 1.0,
        "peerDeviceId": "device-ccc",
        "peerDeviceName": "MBP 16 M3",
        "startedAt": "2026-06-10T20:30:00Z",
        "completedAt": "2026-06-10T20:30:05Z",
    },
    {
        "id": "t-005",
        "fileName": "large-archive.tar.gz",
        "filePath": "/Users/hans/Archives/large-archive.tar.gz",
        "fileSize": 850_000_000,
        "direction": "send",
        "status": "failed",
        "progress": 0.42,
        "peerDeviceId": "device-bbb",
        "peerDeviceName": "Hans's iMac Studio",
        "errorMessage": "Connection reset by peer",
        "startedAt": "2026-06-10T19:15:00Z",
    },
]


async def health(_request: web.Request) -> web.Response:
    return web.json_response({"ok": True, "ts": int(time.time())})


async def list_prompts(_request: web.Request) -> web.Response:
    return web.json_response(MOCK_PROMPTS)


async def list_devices(_request: web.Request) -> web.Response:
    return web.json_response(MOCK_DEVICES)


async def list_transfers(_request: web.Request) -> web.Response:
    return web.json_response(MOCK_TRANSFERS)


async def sync_prompts(_request: web.Request) -> web.Response:
    return web.json_response({"synced": 0, "duration_ms": 12})


def build_app() -> web.Application:
    app = web.Application()
    app.router.add_get("/api/health", health)
    app.router.add_get("/api/prompts", list_prompts)
    app.router.add_get("/api/devices", list_devices)
    app.router.add_get("/api/transfer/tasks", list_transfers)
    app.router.add_post("/api/sync", sync_prompts)
    return app


async def main_async() -> None:
    port = 8765
    app = build_app()
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", port)
    await site.start()
    logger.info("Mock API server listening on http://127.0.0.1:%d", port)
    logger.info("Endpoints: /api/health, /api/prompts, /api/devices, /api/transfer/tasks")
    try:
        await asyncio.Event().wait()
    finally:
        await runner.cleanup()


def main() -> int:
    try:
        asyncio.run(main_async())
    except KeyboardInterrupt:
        logger.info("Mock API server stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
