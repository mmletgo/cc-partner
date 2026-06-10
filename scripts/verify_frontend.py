#!/usr/bin/env python3
"""
前端渲染验证脚本：用 Playwright 打开 Vite dev server，截图所有页面

Business Logic:
    前端 6 个页面 + DesignSystem 预览页需要视觉验证渲染正确性，
    没有自动化测试覆盖时，用 Playwright 截图作为回归基线。

Code Logic:
    - 启动 Playwright headless chromium
    - 依次访问 6 个页面 + DesignSystem
    - 等页面加载完成（networkidle），截图保存到 docs/frontend/screenshots/
    - 收集 console 错误
"""

import asyncio
import sys
from pathlib import Path

from playwright.async_api import async_playwright

PAGES = [
    ("home", "/"),
    ("prompts", "/prompts"),
    ("transfer", "/transfer"),
    ("devices", "/devices"),
    ("settings", "/settings"),
    ("welcome", "/welcome"),
    ("design-system", "/design-system"),
]

BASE_URL = "http://localhost:5173"
OUTPUT_DIR = Path(__file__).resolve().parents[1] / "docs" / "frontend" / "screenshots"


async def main() -> int:
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    failures: list[str] = []
    console_errors: dict[str, list[str]] = {}

    async with async_playwright() as p:
        browser = await p.chromium.launch(headless=True)
        context = await browser.new_context(viewport={"width": 1280, "height": 800})
        page = await context.new_page()

        for name, path in PAGES:
            url = BASE_URL + path
            print(f"📸 {name:15} {url}")
            errors: list[str] = []

            def on_pageerror(exc: object, errs: list[str] = errors) -> None:
                errs.append(f"pageerror: {exc}")

            def on_console(msg: object, errs: list[str] = errors) -> None:
                msg_type = getattr(msg, "type", "")
                msg_text = getattr(msg, "text", "")
                if msg_type in ("error", "warning"):
                    errs.append(f"console.{msg_type}: {msg_text}")

            page.on("pageerror", on_pageerror)
            page.on("console", on_console)
            try:
                await page.goto(url, wait_until="networkidle", timeout=15000)
                await page.wait_for_timeout(500)  # 让动画/字体稳定
                shot = OUTPUT_DIR / f"{name}.png"
                await page.screenshot(path=str(shot), full_page=True)
                print(f"   ✅ 截图: {shot.name} ({shot.stat().st_size // 1024} KB)")
            except Exception as e:
                print(f"   ❌ 失败: {e}")
                failures.append(f"{name}: {e}")
            if errors:
                console_errors[name] = errors

        await browser.close()

    print("\n" + "=" * 60)
    if failures:
        print(f"❌ {len(failures)} 个页面失败:")
        for f in failures:
            print(f"  - {f}")
    else:
        print(f"✅ 全部 {len(PAGES)} 个页面渲染成功")

    if console_errors:
        print(f"\n⚠️  {sum(len(v) for v in console_errors.values())} 条 console 错误/警告:")
        for page_name, errs in console_errors.items():
            for err in errs[:5]:  # 最多展示 5 条
                print(f"  [{page_name}] {err}")
            if len(errs) > 5:
                print(f"  [{page_name}] ... 还有 {len(errs) - 5} 条")
    else:
        print("✅ 无 console 错误/警告")

    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
