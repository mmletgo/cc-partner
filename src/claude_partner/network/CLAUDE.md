# network/ - 网络通信层

## 模块概述

负责局域网设备发现和 HTTP 通信，是 P2P 架构的网络基础。

## 文件说明

### discovery.py - mDNS 设备发现
- `DeviceDiscovery(QObject)`: 通过 zeroconf 注册和发现局域网设备
  - mDNS 服务类型: `_claude-partner._tcp.local.`
  - TXT 记录包含 device_id 和 device_name
  - 回调在后台线程执行，通过 Qt 信号跨线程通知
  - 自动过滤本机设备 ID
  - 信号: `device_found(object)`, `device_lost(str)`

### protocol.py - HTTP API 路由
- `APIProtocol`: 定义 19 个 API 端点，包括前端 REST 和 P2P 协议
  - 构造参数（全部可选回调）:
    - `prompt_repo`: PromptRepository，用于 CRUD
    - `on_transfer_init/chunk/status`: 文件接收回调
    - `get_devices`: 设备列表回调（由 app.py 注入 DeviceDiscovery）
    - `on_transfer_send/cancel`: 发送/取消传输回调
    - `get_transfers`: 传输任务列表回调（合并 sender+receiver）
    - `on_choose_dir`: 原生目录选择对话框回调（QFileDialog）
    - `on_check_update`: GitHub Releases 更新检查异步回调
    - `check_permissions`: macOS 权限检查回调（屏幕录制/输入监控）
    - `actual_port`: HTTP 服务端实际监听端口（动态分配时由 app.py 通过 set_actual_port() 设置）
    - 未注册的回调对应端点返回 501/404
  - 前端 REST 端点（14 个）:
    - `GET /api/health`: 健康检查（{ok, device_id, device_name, http_port}）
    - `GET /api/prompts`: Prompt 列表（支持 ?search= &tag=）
    - `POST /api/prompts`: 新建 Prompt（创建时 vector_clock={device_id:1}）
    - `GET|PUT|DELETE /api/prompts/{id}`: 单条 Prompt CRUD（deleted 返回 404）
    - `GET /api/devices`: 设备列表
    - `POST /api/sync`: 触发同步
    - `GET /api/transfer/tasks`: 传输任务列表
    - `POST /api/transfer/send`: 启动文件发送
    - `DELETE /api/transfer/tasks/{id}`: 取消传输
    - `GET /api/config`: 获取应用配置（camelCase）
    - `PUT /api/config`: 更新配置（仅 deviceName/receiveDir/screenshotHotkey 可写）
    - `POST /api/config/choose-dir`: 原生目录选择对话框
    - `GET /api/version`: 版本号 + 构建日期
    - `POST /api/updater/check`: 触发 GitHub Releases 更新检查
    - `GET /api/permissions`: macOS 权限状态检查
  - P2P 协议端点（5 个）:
    - `POST /api/sync/pull|push`: Prompt CRDT 同步
    - `POST /api/transfer/init|chunk/{id}`: 文件分块接收
    - `GET /api/transfer/status/{id}`: 传输状态查询
  - 字段转换: `_prompt_to_frontend_dict()` 将后端 snake_case 转为前端 camelCase

### server.py - HTTP 服务端
- `HTTPServer`: 封装 aiohttp 的 AppRunner + TCPSite
  - 支持 port=0 自动分配端口
  - 通过 `site._server.sockets[0].getsockname()[1]` 获取实际端口
  - 端口文件: `~/.claude-partner/backend.port`，start() 时写入，stop() 时删除
  - `serve_static(web_dir)`: 配置前端静态文件目录，**必须在 start() 之前调用**（runner.setup() 后路由冻结）
  - `_register_static_routes()`: 在 start() 内 runner.setup() 之前执行，注册 /assets + SPA 回退（`/{path:.*}` → index.html）
  - 路由优先级: /api/* 先注册 > /assets > SPA 回退（`/` + `/{path:.*}`）

### client.py - HTTP 客户端
- `PeerClient`: 调用对端 API 的客户端
  - 懒初始化 aiohttp.ClientSession（避免在 __init__ 中访问事件循环）
  - 方法与 APIProtocol 端点一一对应
  - 默认 10 秒超时

## 依赖关系
- 依赖: `models.device`, `models.prompt`, `config`, `storage.prompt_repo`, `sync.vector_clock`
- 被依赖: `sync.engine`（使用 PeerClient）, `app.py`（启动 HTTPServer 和 DeviceDiscovery）
