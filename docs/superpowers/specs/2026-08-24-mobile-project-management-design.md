# 移动端项目管理（添加本机 / 局域网项目）

## 问题

`/mobile` 目前只能打开主机上已经添加过的 Workbench 项目。用户无法从手机把主机本机目录或局域网对端目录加入最近项目列表，也无法从列表拿掉记录。

## 产品

- 项目面板提供「添加本机项目」「选择局域网项目」。
- **本机** = 当前提供 `/mobile` 的主机。系统目录框不可用，改为应用内目录浏览（根入口 → 一级目录 → 选中打开）。
- **局域网** = 主机已发现的在线对端（不含合成「这台电脑」）。先选设备，再走同一套目录浏览。打开后在主机写入 remote shortcut，后续仍走手机 → 主机 → 对端二级代理。
- 加成功后直接进入该项目工作台（与桌面选中项目一致）。
- 行尾 `⋯` → 移除 → 确认 Dialog。只删主机最近记录，不删磁盘。移除当前项目则回到项目列表。
- 不排序、不按设备筛选、不粘贴路径。

## 实现

共用目录浏览状态机；两条 HTTP 适配：

- 本机：`GET/POST /api/mobile/workbench/fs/{roots,list,info}` 复用 P2P `remote_directory` helper，再 `POST …/projects/open`（已有）。
- 局域网：`POST /api/mobile/workbench/remote/{roots,list,info,open}` 复用 owner `RemoteWorkbenchClient`（与桌面 invoke / control op 同语义）。
- 移除：`POST /api/mobile/workbench/projects/remove`，复用 `remove_workbench_project_for_state`。
- 设备列表复用 `GET /api/mobile/devices`，前端过滤 `!isSelf && status==='online'`。
- 前端：`MobileProjectPanel` 入口 + `MobileProjectPicker` Drawer + 确认 Dialog。不把桌面 `WorkbenchRemoteProjectPicker` 硬塞进手机。

## 错误与边界

- 空 path / 空 deviceId / 空 projectId → 400 `路径不能为空` / 同类校验。
- 对端离线：打开失败展示错误，不写 shortcut。
- 无在线局域网设备：设备列表空态。
- LAN 无身份校验，风险提示沿用现有 `/mobile` 文案，不新增鉴权。

## 验证

- 纯 helper：本机打开门闩、LAN 设备过滤、picker reducer。
- `workbenchHttp` 路由/body 契约。
- Rust handler 空输入拒绝。
- `node scripts/check-p2p-route-inventory.mjs`。
- 移动端面板：添加入口、⋯ 移除确认。
