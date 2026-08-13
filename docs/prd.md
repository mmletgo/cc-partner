# cc-partner - 产品需求文档 (PRD)

## 1. 产品概述

cc-partner 是一款支持 Mac/Windows/Ubuntu 三端的桌面工具，设计用于 Claude Code 用户在局域网环境下的多设备协作。

### 1.1 目标用户
使用 Claude Code 进行开发的程序员，拥有多台局域网设备。

### 1.2 核心价值
- 在多台设备间快速传输文件
- 一键区域截图并粘贴到 Claude Code
- 集中管理常用 Prompt，跨设备同步
- 浏览、搜索、复用和跨设备同步 Claude Code 历史 Prompt
- 在应用内编辑 user 级 `~/.claude/CLAUDE.md`，并主动推送到局域网设备和 GitHub 云端
- 使用多页面速记本记录临时文本，并在局域网与 GitHub 间同步
- 在项目文件夹维度管理 Git worktree、多个普通终端 window/pane，并直接操作当前工作区文件树
- 通过局域网 `/mobile` 入口在手机浏览器操作 Workbench 项目、worktree、终端、文件、Git 和项目级自动化任务（无访问 token）
- 用全局 Inbox（待处理）实时投影当前阻塞工作的事项，并只导航到既有权威界面处理
- 远端设备可只启动独立后端 CLI 暴露 P2P/Workbench/Orchestrator 远端支持，无需完整启动 GUI；可用 doctor 诊断本机后端健康与日志

### 1.3 固定局域网信任边界

cc-partner 仅面向本机与局域网，产品只有一种固定局域网行为：

- 合法 loopback/LAN socket peer 调用 P2P、Mobile、Workbench 与 Orchestrator 业务 API 时，不需要账号、配对、token、cookie、session、签名或设备身份；
- 业务查询、写入与执行对范围内 peer 一律放行，不提供可切换暴露模式、只读模式、逐设备权限或路由级授权矩阵；
- 网络范围、Host/Origin/Content-Type 与资源上限是部署边界与请求完整性保护，不是身份鉴权；
- `/api/backend/control/*` 是本机 loopback 控制面（lifecycle + 运行时权威读写），要求 loopback peer + 控制文件 token；token 与 `controlSchemaVersion` 不进入 LAN 业务 API 或 health capabilities。

**固定风险声明**：同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。

### 1.4 运行时权威（sidecar owner）

- 独立后端 sidecar（`cc-partner-backend serve`）是配置、Cloud Sync、Workbench 终端/远端 bridge、Orchestrator telemetry 的唯一运行时 owner（`HeadlessOwner`）。
- 桌面 GUI 为 `GuiClient`：保留窗口/托盘/系统快捷键与事件转发；凡影响 LAN/Workbench/Orchestrator/Cloud Sync 的运行态 mutation 经 loopback control client 提交到 sidecar，不得在 GUI 进程自建第二套 runtime 写路径。
- 每次权威配置成功落盘后返回稳定 `ownerInstanceId` 与单调递增 `generation`；GUI 以 allowlist patch + expected owner/generation 做 CAS，冲突后刷新再由用户重试。
- 终端 create/restore 仅 sidecar 拥有；失败路径用 RAII 补偿，禁止双进程同时 attach 同一 session。
- Settings「依赖环境」展示脱敏运行诊断（owner/generation/phase/计数/错误类别）；复制摘要不得包含 Prompt、文件内容、终端文本、远端 URL 凭据或 control token。

## 2. 功能需求

### 2.1 局域网文件传输

**描述**：在局域网内的多个设备间互传文件。

**功能点**：
- 选择在线设备作为传输目标
- 支持任意大小文件传输
- 分块传输（约 960KB/块），显示传输进度、phase 与速率
- 断点续传：传输中断后，若双方 resume metadata 与源 fingerprint 仍匹配，可从已确认 offset 继续（复用稳定 `protocolTransferId`）
- SHA256 校验确保文件完整性；receiver finalize 前 hash mismatch 拒绝落盘；source fingerprint `{size,mtimeNsOrNull,sha256}` 变化时拒绝旧 resume
- 桌面发送闭环走真实后端命令：原生 dialog/drag 取得绝对路径 → `send_transfer` 受理 → 任务列表可见
- 生命周期动作（仅渲染后端真实支持的 callback，无回调不渲染）：
  - `pending` / `transferring` / `finalizing` → 仅 `cancel_transfer`
  - `failed + retryable + resume metadata`（Send 且双方支持 `transfer.resume.v1`）→ `resume_transfer(taskId, clientOperationId)`「继续传输」
  - `failed + retryable` 无 resume metadata、或旧 peer 无 resume capability、或 `cancelled` 且源仍匹配 → `retry_transfer`「重新传输」
  - transport timeout / uncertain outcome → 先 `get_transfer_operation(clientOperationId)` 对账，展示「正在确认结果」，禁止 blind retry
  - `direction=Receive` + `completed`（same-device desktop GUI only）→ Open / Reveal；P2P/mobile 明确 unsupported
- 幂等合同：`clientOperationId` 是发送端全局唯一持久键（same id + same payload 回放；same id + different payload → `operationIdConflict`）；`X-CC-Request-Id` / invoke request id 仅追踪，不作幂等键。resume 复用稳定 protocol id 命中 receiver checkpoint；full retry 可 mint 新 protocol id。lost final ACK 时发送端按 protocol id 查询 receiver status，确认成功后本地提交 outcome，不二次破坏性 finalize
- 路径为不透明 UTF-8 字符串透传，UI 只展示 basename；刷新失败保留已有 devices/tasks，不得用空数组覆盖
- 文件接收后保存到用户配置的目录
- 1 GiB 真机断网/重启/续传/SHA 一致性属于 dual-host L3（`L3-DUAL-HOST-LAN-001`），当前保持 **NOT VERIFIED**，不得由单机 CI 宣称通过

### 2.2 区域截图

**描述**：用户触发截图后，框选屏幕区域，截图自动保存到系统剪贴板。

**功能点**：
- 通过快捷键或托盘菜单触发
- 全屏半透明遮罩覆盖
- 鼠标拖拽框选区域，选区内显示原始画面
- 松开鼠标后编辑工具条立即出现，可进行矩形/箭头标注；底图快照加载不应阻塞工具条显示，捕获前不应出现空白/透明闪烁帧
- 确认后截取区域图片
- 自动复制到系统剪贴板
- ESC 键取消截图
- 截图后用户可直接 Ctrl+V 粘贴到 Claude Code

### 2.3 Prompt 管理

**描述**：提供文本记录管理功能，支持标签分类和筛选。

**功能点**：
- 创建 Prompt：标题 + 内容 + 标签
- 编辑已有 Prompt
- 删除 Prompt（软删除，用于同步）：删除以 tombstone 传播，并铸造单调 `delete_epoch`；满足年龄 ≥30 天且近 90 天活跃对端均已 ack 该 epoch 后，才可压缩为 deletion floor，不得在未收敛前物理删除
- 新建/更新/删除采用乐观更新；API reject 必须回滚列表并恢复草稿（create/update），展示错误与原地重试，不得静默伪成功
- 并发且正文不同时保留 winner，并写入冲突副本（conflict copy）；详情提供“版本历史”抽屉（最近 20 个版本或 30 天，先达限；冲突副本至少保留 30 天），可查看摘要、复制内容、恢复为新版本（恢复推进本机向量时钟，不覆盖历史记录）
- 冲突用非阻塞 Pill 标识，不禁用编辑与保存，不提供逐行三方合并编辑器
- 一键复制 Prompt 内容到剪贴板
- 标签管理：添加/移除标签
- 按标签筛选 Prompt 列表
- 文本搜索（搜索标题和内容）
- 列表按后端返回顺序展示；当前页面提供搜索和标签筛选，不提供显式排序控件
- Prompt 优化：用户输入原始编程任务 Prompt 后，调用本机 Claude Code CLI 的 pure/headless 模式生成中文优化版与等价英文优化版
- Prompt 优化结果只用于当前页面展示和复制，不保存历史、不入库、不跨设备同步、不做缓存
- Prompt 优化输出必须以需求方视角写成可直接粘贴给 Claude Code 的委托式 Prompt，不得生成“请确认/是否需要/请指定”等继续询问用户的澄清句；原始信息不足时只能写成待补充占位或执行假设，除非原始 Prompt 明确要求文档或文件输出，否则不得新增 `docs/`、写文件、持久化等确认要求
- Prompt 优化结果区以中文/英文双卡片展示，每张卡片内部包含标题与复制操作区、分隔线和只读文本内容区
- 工作台终端界面可通过工具栏或快捷键唤出 Prompt 优化浮层；浮层只显示一个原始 Prompt 输入框，不显示优化按钮、填入终端按钮、双语结果标题/结果区或关闭按钮；优化时应以当前项目根目录作为 Claude Code 工作目录，使其可读取项目 CLAUDE.md 上下文，并只按设置页选择的中文/英文语种生成一个优化版 Prompt；默认快捷键为轻按 Control 单键，首次触发打开浮层并聚焦原始 Prompt 输入框；浮层打开后再次触发快捷键时，如果输入框为空则直接关闭浮层，如果输入框非空则自动优化；输入框内非空时按 Enter 与再次触发快捷键等价，Shift+Enter 保留换行，输入法 composing 状态下的 Enter 不提交；后端把优化后的 Prompt 边生成边流式写入当前运行中的终端，完成后自动关闭浮层；填入只插入文本，不自动追加回车或执行命令

### 2.4 Claude Code 历史

**描述**：采集本机 Claude Code 历史会话中的用户输入 Prompt，按项目维度浏览、复用和同步。

**功能点**：
- 从本机 Claude Code 历史目录采集用户输入 Prompt，并按 Git 主项目聚合展示；通过 `git worktree list --porcelain -z` 获取项目全部 worktree，以 Git 返回的主工作区作为项目身份，不依赖 worktree 所在目录命名或 Workbench 是否登记；Git 无法证明归属的已删除/不存在路径保持原值，不做字符串猜测
- 历史只收录用户在交互式 Claude Code 中直接输入的 Prompt；子 Agent sidechain 指令、Claude 内部元消息、任务通知、压缩摘要、工具结果与 SDK 自动输入必须过滤。过滤规则升级后应重扫本机 transcript，把旧版本误收项软删除并通过既有同步链路传播删除
- 旧版按 cwd 存储的历史在首次读取项目列表时持久迁移到 Git 主项目路径，同时推进本机向量时钟，使路径变更可通过既有 CC 历史同步链路收敛
- 项目列表展示每个项目的 Prompt 数量和最近更新时间
- 历史页先按历史所属设备筛选项目，默认只显示本机设备采集的项目及对应 Prompt；用户可切换到同步历史中出现过的其他设备（含当前离线设备）。项目下拉筛选器与项目列表选中态同步；选中项目内的 Prompt 时间线支持文本搜索、详情查看和一键复制
- 支持把历史 Prompt 一键转存到正式 Prompt 库
- 支持删除单条历史 Prompt，删除结果参与跨设备同步
- 支持手动刷新采集，并在全局同步中通过独立 `cc-history` 链路跨设备合并
- 跨设备 CC 历史同步为**有界、可收敛**行为：支持分页的对端按摘要页与分批正文交换合并；不支持分页能力的旧对端继续走完整摘要/正文路径；任一轮中断后下一轮从摘要起点重新交换，合并语义幂等，不会出现半批次落库
- 单条历史正文与单批同步体量有固定上限；超限时本轮同步以稳定错误结束该批次，不静默丢弃或半写入
- Claude Code 历史纳入 GitHub 私有仓库云端同步范围

### 2.5 Multi-CLI Agent Hub（Gate A 指令基础 + Gate B 可移植资产 + Gate C Snapshot 复制/备份 + Gate D Plugin/Runtime）

**描述**：Agent Hub 以 `/agent-hub` 为唯一生产入口，在同一 Shell 中管理**本机用户级、局域网远端设备与项目级** Claude / Codex / OpenCode 指令和可移植资产。用户级远端设备复用 same-agent Pull 与 Snapshot Push；本机项目必须先由 Workbench 项目 id 精确解析为唯一 Hub project id，再进行 opt-in、库存扫描、预览和 Apply，禁止退化为“扫描全部已映射项目”。远端项目 shortcut 通过既有 Workbench `projects/open` 在 owning peer 解析成真实 local project id；portable inventory 与 preview/apply/get 均在 owning peer 执行并绑定精确项目快照。项目级 Pull 支持远端项目作为源、本机已 opt-in 项目作为目标，也支持把 peer user scope 资产 Pull 到本机项目；计划同时冻结源/目标项目身份与过滤快照，目标 scope 使用本机 Hub project id 重映射。LAN 仍无调用者身份校验，project id、expected-device、snapshot/hash 仅保证请求不会串设备或串项目，不是安全授权。Hub Canonical 指令块可按 revision CAS 保存；提示词页面已经展示将写入的合成内容与原始文件，用户点击“写入原始文件”后，后端内部生成 expected-hash 一次性 plan 并直接原子写入，不再重复弹出预览确认。旧 `/claude-md`、`/claude-code`、`section`、`assetId/conflictId` 深链只做一次性 URL 规范化，不恢复 legacy matrix。Gate B–D 已实现的领域库与协议继续保留；未获真实 CLI L3 认证的后台自动投影与 portable mutation 保持 scan-only，跨 Agent 仅开放本机用户级 bounded selective preview。

**2026-08-10 安全纠正（当前权威行为）**：
- Shell 是 Agent、范围和主 Tab 的唯一上下文真源；主界面只有 Agent 指令、Skill、命令、MCP、Plugin，observed inventory 是资产唯一真源。
- 三栏草稿按 `agent + lane + scope` 隔离；draft lease 的 Canonical base 只在首次 hydrate、明确放弃或成功保存后迁移。刷新只更新观测 head/snapshot，`canonicalDrift` 阻止保存，`sourceDrift` 不阻止 Hub CAS，但在重新载入确认前阻止原生 preview/apply。
- Hub Save 只消费 `blocksDirty`；Original-only Save 是零 API 的诚实 no-op。显式从原始文件解析后形成 dirty Hub 草稿，保存成功不得清另一栏草稿或保存期间的新编辑。
- Agent/lane/Tab/history/deep-link 共用 committed/pending context 与 dirty Dialog；确认前 URL、标签、正文和 CAS lease 都保持旧上下文。
- 远端/项目导航与管理入口不得因写能力未认证而消失；每个动作按 owner、project identity、snapshot/plan 与 capability evidence 独立门禁。缺少精确远端项目身份的 direct inspect/write、legacy writer、未认证 portable 原生 apply、projection create/update/delete、cross-agent apply/full 均 fail-closed。`AGENT_HUB_API_VERSION=4` 阻止新旧 GUI/sidecar 混跑绕过该策略。
- Same-agent Pull 与 LAN Snapshot 任务保留；远端刷新失败会把旧 inventory 标 stale、清 plan 并禁用 Apply，危险冲突策略每次会话恢复为 `skipExisting`。任何原生安装仍受当前 Blocked capability 门禁。
- Portable Action/Pull 的 plan 与异步响应绑定当前 Agent、scope、query/snapshot、item/action、选择与策略；history 或任一输入变化会立即作废旧响应。写动作按 operation 检查精确 capability，不能用 Activate/Render 的认证旁路 disable/uninstall/remove。
- 具体交互、异常与验收合同见 `docs/superpowers/specs/2026-08-10-agent-hub-correction-design.md`。下列 Gate A–D 条目描述保留的数据模型/库路径，不构成当前生产 UI 写入承诺。

**保留的 Gate A 领域能力（非生产 legacy UI）**：
- Hub 可读取 CLI probe、write compatibility 与冲突/阻塞数据；技术诊断按需展示。
- Canonical instruction、Revision DAG、target projection intent 与项目 mapping 数据模型继续保留，但 target matrix、项目 opt-in、legacy conflict drawer 不进入生产 DOM。
- 迁移进 Hub 的用户 CLAUDE.md 记为 Claude **targetOnly** 指令；生成 Codex/OpenCode 差异仅供 preview，原生投影保持 blocked。
- Attention 的 legacy `assetId/conflictId` 目标只允许导航到规范化库存上下文，不直接执行动作。
- N/N+1 的 legacy summary 数据仍可兼容读取；legacy 向量时钟不裁决 Hub 冲突，也不得触发 writer fallback。

**保留的 Gate B 可移植资产库路径（生产写入 scan-only）**：
- Skill / Command / Agent / MCP 的 **canonical Hub 载荷**（common 字段 + `target_extensions`；MCP 凭据原文进 CAS，诊断/日志脱敏）
- 三端 **只读 portable 扫描**（native / compatibility / legacyStandalone / plugin origin；扫描不写盘）
- managed package renderer、activator 与 ownership-aware patch 仍用于预览、测试与未来认证；当前 support manifest 三 target 的原生写 capability 均为 Blocked，任何 writer 前必须 force-inspect，因此生产不会物化/启停/卸载 CLI 资产
- **shared** 资产对三端可见；**targetOnly** 严格隔离（OpenCode 不接收 Claude/Codex targetOnly）
- ownership-aware **TOML/JSONC** 语义 patch：managed 字段 enable/disable/update/remove 后，unmanaged 键与注释仍存活
- legacy adoption 的 planner/恢复合同保留，但 production legacy matrix 与 writer 不可达；旧 URL 只翻译到 canonical inventory，不能 fallback 到旧 `listAssets/getAsset` 或 mutation drawer
- 前端仅保留搜索、资产状态、管理状态、详情与刷新；Agent/kind/scope 来自 Shell。Blocked/stale 时 mutation 动作不可见，并展示稳定原因

**已交付（Gate C Snapshot / LAN source-push / Git device-lane）**：
- SnapshotEnvelope v1：`format=cc-partner-agent-hub`、`formatVersion=1`、`canonicalization=RFC8785-JSON`；硬上限 selection 100k / 未压缩 2GiB / 单 blob 512MiB / manifest 32MiB / LAN chunk ≤8MiB
- LAN **仅源侧选择目标后的 push**（`POST prepare` / `PUT objects` / `POST commit`）；幂等键 `(sourceDeviceId, clientRequestId)`；`sourceDeviceId`/`clientRequestId`/expected-device header **只用于路由绑定与幂等，不是身份认证**
- chunk 中断可从 offset 续传；同 request 同 hash 重放原 outcome；不同 hash → conflict
- 凭据在 Hub / Snapshot / LAN / Git / 目标配置中保持**原字节**；日志、错误与 UI 摘要继续脱敏（UI 仅 boolean 披露）
- Git 每台设备只写 `agent-hub/devices/<deviceId>/`；fetch/rebase **不等于**导入其它 lane；远端 lane 只有 **inspect → preview → 用户 confirm** 后才进 Hub；定时任务永不自动 import
- 未映射项目可导入 canonical backup，但**不得猜测本机路径或自动 opt-in**；mapping 默认 `optedIn=false`
- N/N+1 继续保留旧 CLAUDE.md / Claude asset / content-sync 路由；新 UI 不展示旧 remote inventory/pull；legacy 结果不能算 Hub push 成功；`legacyLossy` 占位不得覆盖 canonical credential
- 验证证据：`L2-AGENT-HUB-C-001` / `L2-AGENT-HUB-C-GIT-001` / `E2E-AGENT-HUB-C-001`

**已交付（Gate D Plugin 分解 + OpenCode runtime 合同；库路径 + L2/E2E）**：
- Plugin package 不是最低同步单位：拆成固定 revision 的 Skill/MCP/Command/Agent/Hook 与 residual runtime，进入同一 Revision DAG / CAS / Snapshot 路径
- package 投影聚合态必须区分 `full` / `partial` / `sourceOnly` / `activationRequired` / `externalCollision` / `blocked`；source package 写成功不得 overstate full
- Hook 默认 `targetOnly`；只有 support manifest 中具备双端 schema、信任模型与真实 CLI evidence 的 mapping 才跨 target
- residual（OpenCode JS/TS/npm/custom-tool 等）默认只投影回 source target；跨 target 省略并诊断
- 删除 package 只 tombstone 独占 component；shared / standalone 引用从边表实时查询并保留（不维护易漂移计数器）
- OpenCode runtime bridge 是 **app-version 派生物**（`.opencode/plugins/cc-partner-runtime.ts`），**不是**用户 canonical Plugin，不进入 Snapshot；project opt-in preview 必须列出文件写入；hash 验证失败 / 碰撞 / 未 opt-in → fail-closed
- `openCodeVisible` completion 仅接受官方 Plugin event 经 app-private OSC 进入既有 reducer；OSC 字节不得进入 terminal replay/UI；缺 bridge 时禁止 Sentinel/stdout 猜测完成
- 四 provider 目录（`claudeCodeVisible` / `codexVisible` / `genericTerminal` / `openCodeVisible`）在 Settings / Workbench / Orchestrator 同合同；OpenCode 未 ready bridge 不得呈现 available green
- N/N+1 保留旧入口；N+2 实际删除旧表/路由须 `runningVersion ≥ earliestRemovalVersion` 且有稳定迁移 evidence（当前 evidence id 仍未解锁删除）
- 验证证据：`L2-AGENT-HUB-D-PLUGIN-001` / `L2-AGENT-HUB-D-RUNTIME-001` / `E2E-AGENT-HUB-D-001`
- 官方行为参考（文档，非 CI 替代）：[OpenCode CLI](https://opencode.ai/docs/cli/)、[OpenCode Plugins](https://opencode.ai/docs/plugins/)、[Codex CLI reference](https://developers.openai.com/codex/cli/reference/)、[Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference)

**明确未交付（后续 L3；不得宣称已完成）**：
- 真实双主机 mDNS / 全平台 LAN Hub 复制（`L3-AGENT-HUB-C-LAN-001` **NOT VERIFIED**）
- 真实 OpenCode 可见 TUI session/permission/completion/resume/Ctrl-C（`L3-AGENT-HUB-D-OPENCODE-001` / `L3-AGENT-HUB-OPENCODE-RUNTIME-001` **NOT VERIFIED**；L2 library smoke 不是替代）
- 真实多机 / 全平台 CLI 写盘与 exact 版本认证（含 `L3-AGENT-HUB-B-CLI-001` **NOT VERIFIED**）
- 真实 Claude / Codex / OpenCode 产品安装路径写盘族（`L3-AGENT-HUB-CLAUDE-001` / `L3-AGENT-HUB-CODEX-001` / `L3-AGENT-HUB-OPENCODE-001` **NOT VERIFIED**）
- **不得**宣称 LAN 身份认证、自动 Git import，或未 pin 的 OpenCode 版本 runtime 已认证

**Program-wide certification（Program Task 5，integration `sdd/agent-hub-2026-07-29` @ `8d1fa0d7` post Codex R6，2026-07-30）**：
- 已在集成树上复跑 Gate A–D 聚焦 Rust 串行 lib/smoke、前端 unit/build/E2E 与协议/文档门禁；L2/E2E 证据 ID 保持可追溯，**不**把 L1/L2 升格为 L3
- `cargo test --lib agent_hub` **以 `--test-threads=1` 为认证模式**（默认并行下 importer 全局 fault inject 可 flake）；详见 `docs/development/testing.md` program-wide 表
- 前端 `npm run lint` 与默认 strict `check:bundle` 在本轮 **未**宣称通过（lint 债务集中于 Agent Hub hooks/React Compiler；bundle 仅 final-only 硬顶可通过）
- 命令矩阵与完整 NOT VERIFIED 清单：`.superpowers/sdd/reports/program-task-5-report.md`

**Portable inventory 与 same-agent Pull（当前生产边界）**：
- Agent Hub 以 observed inventory 为唯一真源，固定四 kind 主 Tab（Skill/命令/Plugin/MCP）；库存自身只保留 search/state/management，target/kind/scope 不再形成第二套 URL 状态
- 本机原生动作在当前 scan-only manifest 下不可 Apply；stale / blocked / partial / outcomeUnknown 诚实展示，详情动作隐藏时同步展示原因
- mutation 必须按实际动作精确匹配 capability：原生普通 portable 资产写入只认 `RenderPortableAssets`，原生 Plugin 启用只认 `ActivatePackage`，停用/卸载只认精确 `DeactivatePackage=Supported`；Pull 安装 Plugin 同时要求 Render + Activate；Hub managed package（无论内部组件 kind）停用/Absent 只认 Deactivate。target 汇总能力、旧 plan 或其它已认证写能力都不得替代该检查
- MCP 详情与 Pull 仅展示 credential present/hash（及 boolean 披露），不渲染 secret 明文
- same-agent Pull：远端 metadata inventory、选择、冲突/replace preview、progress、canonical-only mapping、per-item report；不提供跨 Agent destination picker
- `/claude-code` 与旧 `section/target/kind/assetId/conflictId` deep-link 进入后规范化为 `/agent-hub?agent=…&tab=…` 或可映射的 `inventoryItemId`；旧 ClaudeCodeAssets/legacy matrix 前端不可达
- L1 证据：`E2E-AGENT-HUB-PORTABLE-001`（`web/tests/agent-hub.spec.ts` backendHarness mock）；后端 L2：`L2-AGENT-HUB-PORTABLE-PARITY-001` / `L2-AGENT-HUB-PORTABLE-PULL-001`
- 真实多机/全平台 CLI 写盘与 dual-host mDNS 仍 **NOT VERIFIED**（L3）

**用户级指令管理 V2（页面核对后人工写入）**：
- 已交付专用三槽工作区、三 Agent inventory/source chain、`unconfigured` 聚合态、公共/适配/独有 Canonical 草稿、严格 DTO 解码，以及逐目标 preview/apply；portable assets、同步与诊断保留为独立次级入口
- owner/control/Tauri 的 inspect、setup/update preview 与 apply 合同使用短期 plan、CAS snapshot、expected hash、独立 ownership、原子 sibling rename 与幂等存储。可读的本机用户级目标允许用户在提示词页面核对合成内容与原始文件后点击“写入原始文件”；后端内部生成短期 plan 并立即应用，不额外展示预览 Dialog；后台自动投影、portable mutation 与删除仍受 support manifest/L3 门禁
- 用户级 instruction 入口已从通用 asset target matrix 改为专用管理工作区，优先展示三个 Agent 的实际生效来源、文件路径、Hub ownership、写入能力与下一步；旧页面的“应不存在 / 无状态 / 已验证 / 不支持 / partial”组合不是 V2 合法用户状态
- 首次管理必须遵循“只读 inventory → 选择公共/专属内容与目标 Agent → 路径、优先级影响和精确 diff → 用户确认 → 逐 target 安全投影”；确认前不写 CLI 文件，不把发现等同于纳管
- Codex 默认持久文件使用 adapter 解析的 `AGENTS.md`，不得静默创建会遮蔽 base 的 `AGENTS.override.md`；OpenCode 必须识别原生 `AGENTS.md`、Claude fallback 及兼容禁用环境变量
- instruction UI 不直接暴露 `desiredPresence × desiredEnabled`；停止管理并保留文件、暂停使用、单 target 移除与 canonical tombstone 是四个不同操作，覆盖/删除必须校验 ownership 与 expected hash
- LAN/Git 导入只表示 canonical 已进入 Hub，不自动应用到本机 Agent；用户仍需在提示词页面点击“写入原始文件”。公共槽不显示 Agent 选择，写入时覆盖所有当前可写目标；适配/独有槽只写当前 Agent
- 权威合同：[`docs/superpowers/specs/2026-08-04-agent-hub-user-instruction-management-v2-design.md`](superpowers/specs/2026-08-04-agent-hub-user-instruction-management-v2-design.md)

### 2.6 设备自动发现与互联

**描述**：局域网内的 cc-partner 实例自动发现彼此并建立连接。

**功能点**：
- 独立后端启动时自动注册 mDNS 服务；远端设备可执行 `cc-partner-backend start` 启动 P2P/Workbench/Orchestrator 远端支持，无需完整启动 GUI
- 独立后端提供 `cc-partner-backend status` 查询运行状态（机器可读 JSON，不含控制 token），提供 `cc-partner-backend stop` 优雅关闭远端支持；生命周期命令成功 exit 0、失败 exit 1
- 独立后端提供 `cc-partner-backend doctor` 与 `cc-partner-backend doctor --json` 做本机诊断：人类可读检查表与机器可读 `DoctorSnapshot`（schemaVersion=1，camelCase）；`healthy → exit 0`、`degraded → exit 1`、`unhealthy` 或 doctor 无法完成 → exit 2；正常 stopped 后端为 healthy/info 而非错误；可选依赖缺失通常 degraded；核心路径（data/db/log）或 backend health 失败为 unhealthy
- 后端诊断日志本地落盘到 `<data_dir>/logs/backend.log`（默认 `~/.cc-partner/logs/backend.log`）：当前文件上限 5 MiB，最多 3 个历史文件（`.1` 最新 … `.3`），不生成 `.4`；路径 home 归一为 `<HOME>`、密钥与 Prompt/body 脱敏；**不上传**、不建远程 Issue、不收集 telemetry；doctor 只读 tail，不写 backend.log
- 数据与日志默认在 `~/.cc-partner/`；可用合法绝对路径环境变量 `CC_PARTNER_DATA_DIR` 隔离到其它目录
- 桌面 GUI 启动时如果独立后端未运行，应自动启动后端进程；GUI 只做前端壳、事件转发和 lifecycle 管理，不重复 advertise 自己，也不在本机 GUI 进程内作为第二 runtime owner 执行配置/Workbench/Orchestrator mutation
- 桌面 GUI 关闭或托盘退出时必须弹窗，让用户选择“仅关闭 GUI 并保留后端”或“前后端都关闭”
- 自动发现同一局域网内的其他实例
- 设备上线/下线实时通知
- 每个实例同时作为 HTTP 服务端和客户端

### 2.7 局域网跨设备同步

**描述**：Prompt、Claude Code 历史、速记本页面和 SSH 连接目标在所有连接的设备间同步。

**功能点**：
- 新设备上线时自动拉取/推送 Prompt，并在同一轮同步中合并 Claude Code 历史、速记本页面和 SSH 连接目标
- Prompt 本地修改后自动同步到对端（500ms 防抖）
- 定时触发全局同步（每 30 秒）
- 各类同步数据均使用向量时钟追踪版本，避免丢失更新
- 并发冲突采用 Last-Writer-Wins 策略，时间戳相等时按设备 ID 稳定决策；并发且正文不同时额外保留 conflict copy（见 2.3 / 2.9）
- 软删除数据需要参与同步传播，避免刷新或同步后复活；tombstone 仅在 age≥30 天且近 90 天活跃 peer 均已 ack `delete_epoch` 后压缩为 deletion floor
- Prompt / SSH 目标 / Scratchpad 使用 typed 同步结果：`Succeeded` / `Partial` / `Unreachable` / `ProtocolError` / `ResourceLimit`；网络、HTTP、JSON、413 与部分失败**不得**折叠为空成功；仅全部领域 `Succeeded` 的设备计入 `synced` / `succeeded_devices`
- 对端宣告 `sync.manifest.v2` 时，三域走有界 manifest-page / items / push-batch：完整拉完排序 manifest 后再算计划；manifest 精确相等时零正文交换；未完整结束的 manifest 流不得推进 delete ack
- 混合版本：v2 客户端对未宣告能力的对端使用 typed legacy pull/push（失败仍 typed）；legacy 客户端继续使用仍挂载的 legacy 路由；不得因 404/空 body 伪造成功空集
- Claude Code 历史在两端均支持分页能力时按有界分页协议交换；任一端缺少该能力时自动回退完整 pull/push，混合版本仍可完成合并（独立于 `sync.manifest.v2`）
- 全局同步不传输普通文件，不同步 Workbench 工作区文件副本；文件传输和 Workbench 远端代理走独立通道
- `CLAUDE.md` 不进入普通全局自动同步，只通过 `CLAUDE.md` 页面主动推送

### 2.8 GitHub 私有仓库云端同步

**描述**：把用户配置的 GitHub 私有仓库作为中心化同步对端，承载多设备之间的离线数据交换与审计历史。

**功能点**：
- 设置页同步 tab 配置私有仓库 URL、分支、启用状态、自动同步开关和同步间隔
- 支持测试 Git 连通性、手动触发同步和后台定时自动同步
- 同步范围包含 Prompt、Claude Code 历史、SSH 连接目标和 Scratchpad 多页面数据
- 同步导入时按各业务对象的向量时钟和 LWW 规则与本机 SQLite 合并
- 同步导出时把当前本机数据快照写成 JSON 文件并提交到配置分支
- 软删除记录也写入云端快照，用于跨设备传播删除语义
- `CLAUDE.md` 不参与普通云端自动同步导入/导出，仅由 `CLAUDE.md` 页面主动推送本机版本到云端
- 云端同步不管理 Git 认证，复用用户本机 git 凭证、SSH key 或 credential helper

### 2.9 速记本

**描述**：提供多页面自动保存文本区域，用于快速记录临时想法、片段和待办。

**功能点**：
- 支持新增、切换、重命名、删除多个 Scratchpad 页面；新增入口位于左侧页面列表末尾
- 每个页面包含标题与正文内容，空标题保存为“未命名”
- 内容权威源为本机 SQLite，页面不再依赖 localStorage
- 用户编辑正文后自动保存；AppShell 常驻 autosave queue，切换页面、删除页面、同步前先 flush 当前页待写内容
- 关闭 GUI / 托盘退出前必须 flush 全部 pending write（含速记本）；flush 失败中止关闭并展示错误，不得静默丢数据
- 保留复制当前页正文和清空当前页操作
- 删除页面使用软删除传播，刷新或同步后不应复活；tombstone / `delete_epoch` / deletion floor 规则与 Prompt 同步一致
- 并发且正文不同时保留 conflict copy；详情提供“版本历史”抽屉（20 版本或 30 天；冲突 ≥30 天），可恢复为新版本（推进向量时钟）与复制内容；冲突用非阻塞 Pill 标识
- 手动/全局同步结果按设备与领域展示 typed outcome；Partial/Unreachable 等不得显示为整机成功
- 支持手动触发一次同步，同时执行局域网同步与 GitHub 云端同步，并纳入全局云端同步范围
- 旧版单页速记本内容升级后保留为标题“速记本”的第一页

### 2.10 SSH 连接目标管理

**描述**：SSH 连接目标（host/username/port/label）作为后端跨设备同步域之一持续运行；前端「设备 / SSH 连接目标」管理页面已下线，不再提供连接目标列表的展示与行内编辑、ssh 命令复制与三端配置指南 UI。

**功能点**：
- 连接目标配置（host/username/port/label）基于向量时钟跨设备同步；模型不含私钥/凭据材料
- SSH 目标同步结果为 typed domain outcome，纳入全局 `trigger_sync` 设备报告；失败不得折叠为空成功

### 2.11 健康提醒

**描述**：监测久坐行为，在长时间连续工作后提醒用户休息，降低健康风险。

**功能点**：
- 久坐监测：每分钟采样前台键鼠活跃度，推进工作/休息状态机
- 工作/休息状态机：Idle / Working / Resting 相位流转，连续工作达阈值触发久坐提醒
- 多通道提醒：健康监测启用后，久坐提醒默认弹全屏遮罩（每屏一层透明置顶遮罩，按钮关闭），系统通知由通知开关单独控制
- 喝水提醒：健康监测启用后始终启动，按可配置间隔（默认 1h）独立计时，到点弹喝水全屏遮罩；「已喝水」按钮记录一次喝水入 water_records
- 免打扰时段：可配置不弹通知的时间区间（支持跨午夜）
- 手动控制：开关监测、暂停/恢复、贪睡、跳过
- 开机自启：监测启用时注册系统开机自启（macOS LaunchAgent），禁用时移除
- macOS 权限引导：首次启动引导 Accessibility / 屏幕录制 / 通知；输入监控没有独立消费者，不在欢迎页或设置页展示；进入页面只查询，所有 Request、打开系统设置和重新打开应用都必须由用户逐项显式触发；首轮失败可「重新检查」
- 健康状态页使用可见性感知轮询（页面 hidden 暂停、visible 立即刷新、single-flight），刷新失败保留已有数据
- 健康提醒页：以状态概览、今日活跃指标、app 使用时长排行图表和 24 小时活跃分布图表展示监控控制台，头部配置入口跳转设置页健康提醒 tab
- 设置页健康提醒 tab：以「健康提醒 / 免打扰 / 通知与隐私」三个分栏目 Card 展示配置表单
- 完整配置表单：健康监测总开关、工作窗口/休息时长、喝水提醒间隔、通知开关、记录窗口标题、免打扰起止 24 小时制时间选择器、数据保留天数；久坐提醒、喝水提醒和全屏遮罩不提供独立开关，均随健康监测启用

### 2.12 GitHub 周热门项目

**描述**：「Github热门」菜单页展示 GitHub 周热门项目，并可选使用本机 Claude CLI 生成中英文项目解说。

**功能点**：
- 抓取 GitHub Trending Weekly 全语言项目列表，展示仓库名称、简介、语言、星标、fork 与本周新增星标
- Github热门页项目卡片使用自适应视口宽度的动态列数网格（CSS Grid `repeat(auto-fill, minmax(var(--card-min-width), 1fr))`，最小列宽 360px），列数随窗口宽度自动增减（如 1280px→3 列、1600px→4 列、980px→2 列、<720px→1 列），排名与标题同行展示，卡片内部按头部、简介、Claude 解说和指标区保留清晰间距，卡片自身高度不强制等高
- 对榜单按天缓存，频繁打开 Github热门页时优先使用缓存，GitHub 刷新失败时可回退最近缓存
- 用户可配置是否启用 Claude CLI 解说、CLI 路径、模型与缓存有效期
- Claude CLI 解说失败时首页仍展示 GitHub 原始简介，并显示可诊断的失败原因
- 解说失败后用户点击刷新按钮视为主动重试信号，后端跳过未过期的失败缓存、用缓存的 GitHub 榜单重新调用 Claude 解说，不重新抓取 GitHub
- 旧的泛化失败缓存不会永久阻挡修复后的解说生成，应用可在合理条件下重新尝试生成

### 2.13 Claude Code 资产管理

**描述**：集中查看本机 Claude Code skills、commands、plugins 与 MCP 配置，并从局域网设备选择性拉取。

**功能点**：
- 顶部按资产类型分别展示启用数量与警告数量，避免只给总数导致定位成本过高
- 页面内用「本机资产 / 局域网拉取」两个 tab 切换，默认打开本机资产
- 搜索框与类型筛选对本机资产 tab 和局域网拉取 tab 的远端资产列表均生效
- 本机资产与局域网远端资产列表在宽屏下两列显示，窄屏自动回到单列
- 局域网拉取 tab 可选择设备、加载远端资产、勾选后拉取
- 本机资产 tab 支持搜索、类型筛选、启用/关闭与卸载
- 页面不提供本机安装卡片，新增资产优先通过文件系统或局域网拉取路径完成

### 2.14 用户设置

**描述**：提供集中偏好设置入口，管理基础配置、权限、快捷键、工作台运行依赖、同步、Claude CLI/AI 能力和版本更新。

**功能点**：
- 常规设置包含设备名、文件接收目录和截图快捷键
- 截图快捷键通过只读录制控件修改，保存时不应覆盖未修改的设备名或接收目录
- Settings 资源按组隔离加载：单组失败不得整页失败；失败组展示错误与重试，已成功组继续可用；恢复默认在 defaults 不可用时禁用并提示
- 常规设置保存失败必须保留脏表单与 isDirty，展示保存错误与重试；不得把 save reject 提升为整页 loadError 卸掉草稿
- 依赖环境页签展示 macOS 权限管理、Workbench tmux dependency manager 状态和局域网互联依赖；Workbench 依赖支持检测、查看后端/版本/路径、查看安装命令预览、触发安装、取消安装和重新检测；局域网互联依赖展示访问项目所需的实际 P2P HTTP TCP 端口、mDNS UDP 5353、局域网 IP、当前系统平台、端口是否已开放和对应打开方法。应用只读检测系统防火墙状态，不自动修改防火墙；无法读取到明确放行规则时按未开放显示；移动端 Workbench 的局域网访问链接与二维码由全局侧栏左下角手机按钮弹层展示
- macOS 权限引导与设置页共用 `usePermissions`：首轮检查失败结束 loading 并显示错误 +「重新检查」；刷新失败保留 stale 状态；Request 与 Open Settings 是独立动作。产品只展示屏幕录制、辅助功能和通知，完成条件不包含输入监控；后端继续保留输入监控四态 DTO 与公开探测接口用于协议兼容和诊断，但前端不提供请求或设置入口。开发壳由 `start.sh` 统一组装到 `~/Applications/cc-partner (Dev).app`；禁止私有 TCC、产品内系统 reset、持久 pending marker、运行时重签和自动重启
- 常规 / 同步 / AI 页签的恢复默认按钮始终可点击；常规恢复为后端按当前设备环境生成的默认设备名、默认接收目录和平台默认截图快捷键，同步和 AI 分别恢复为后端定义的云端同步默认配置与 Claude CLI/AI 默认配置
- 同步、AI 和关于页签分别管理云端同步、Claude CLI/AI 能力和应用更新；AI 页签中的 CLI 路径与模型供 GitHub 项目解说和 Prompt 优化共用，启用开关与缓存时长仅作用于 GitHub 项目解说；AI 页签同时管理 Workbench Prompt 优化浮层快捷键与自动填入语言，默认轻按 Control、默认填入中文优化版；同步和 AI 的恢复默认只重置表单，仍需用户点击“应用配置”持久化
- 同步 tab 局域网卡片展示每设备/领域的 `succeeded` / `partial` / `unreachable` / `protocol` / `resource-limit` 与 pulled/pushed/unchanged；仅全成功设备计入成功计数
- 同步 tab 提供“导出数据 / 从备份恢复”：经 sidecar owner control 路由生成可校验 ZIP 备份；导出不含项目源码、终端 transcript、SSH 私钥、token、凭据 URL、lifecycle control token；配置仅导出只读 report，恢复时永不写回
- 恢复前必须 inspect 预览（流式限制：archive ≤2 GiB、条目 ≤100k、单 entry ≤64 MiB、总解压 ≤4 GiB；拒绝 zip-slip / 符号链接 / 未知 formatVersion / 哈希失败）；用户确认后进入 exclusive maintenance gate，先创建恢复前备份（保留 7 天且最多 3 份，仅新备份完整落盘后清理旧份），再单事务 apply；失败整批回滚，并可一键 rollback 到恢复前备份

### 2.15 工作台

**描述**：以项目文件夹为中心管理 Git worktree、普通交互式终端、当前工作区文件夹、文件内容浏览/编辑和 Git 提交树。支持本机目录、已挂载局域网目录，以及通过局域网 P2P 直连选择远端 cc-partner 设备项目目录（无调用者身份校验）；用户可直接浏览已发现设备的目录并选择远端项目文件夹，文件、Git、worktree、终端和 Prompt 优化均代理到远端设备执行。

**功能点**：
- 工作现场安全恢复：桌面端自动保存最后工作现场结构 metadata（project/worktree/session/view/inspector/browser target；主窗 slot `desktop:auto`，卫星窗 slot `desktop:auto:window:workbench-[1-4]`，`schemaVersion=1`，稳定 selection 变化 500ms debounce 合并；revision CAS）。卫星窗 URL `projectId` 与 slot 内项目不一致时以 URL 为准，跳过该 slot restore。layout **不得**包含 terminal 字节、Prompt/回复、文件正文、env、token、命令、provider 配置或 preview ID。打开 Workbench 时先 side-effect-free preflight，再应用可安全恢复项；tmux target 存在才允许幂等 safe attach，禁止 `tmux new-session/new-window`、raw PTY fallback、terminal write、创建 worktree/shell、Claude/Codex resume。完全成功静默；partial 仅一条可关闭 inline notice。命名 snapshot 仅结构 metadata，非可执行命令配方。remote layout 留在控制设备；owner 只接收 inner ID 做 preflight/attach，capability `workbench.workspace-safe-restore.v1`；Mobile v1 不自动应用 Desktop layout。不新增第 8 个 Workbench controller。
- 多屏卫星窗：主窗可「在新窗口打开」项目，外拓最多 4 个 `workbench-1..4` 卫星窗；同一 `projectId` 同时只属于一扇窗，再开则聚焦已有窗。关卫星窗只释放占用并删除对应 window auto slot，不 `exit_gui`、不拆 PTY。关主窗仍走现有 GUI/后端选择并退出整个桌面进程。
- 工作台布局：侧栏 Work 组含「工作台」导航与 `WorkbenchProjectRail` 项目列表；`/` 保持 GitHub Trending。主区域依次展示工作台标题、terminal sessions 标识、worktree 管理层、依赖提示槽和中心工作区；中心工作区在当前 worktree 的终端层与文件 tab 工作区之间切换，预览文件时终端可以隐藏但 xterm DOM 必须保持挂载并停止接收输入；终端工具栏按钮与文件工作区工具栏保持一致，均显示图标和文字；桌面端终端全屏按钮位于 pane 操作导航栏，全屏时隐藏 Workbench 外围内容和文件预览入口，但保留 terminal window tabs、pane 操作、退出全屏按钮和当前终端输出，确保全屏中仍可切换 window；文件预览按钮仅在已有打开文件 tab 时启用，点击后回到当前或首个文件 tab，与文件工作区的返回终端按钮对称；右侧检查器承载当前 window 状态，并提供当前 worktree 文件夹 / Git 提交树 / 项目笔记 tab，窄宽下排到首屏终端之后
- 工作台顶栏标语：桌面端已选中项目后，标题区与「现场快照 / 项目自动化」之间提供本机全局可编辑标语；支持轻量 Markdown（粗体/斜体/删除线/行内代码/http(s) 链接）与 emoji；单击预览进入编辑，失焦或 ⌘/Ctrl+Enter 保存，Esc 取消未提交草稿；正文自动保存到本机 `localStorage`（`cp-workbench-banner`），不进 `config.json`、SQLite、workspace layout、备份可恢复领域或局域网同步；按区域尺寸二分最大字号使全文可见并尽量撑满，不拉字距、不压扁字形；上限 280 个 UTF-16 单位；零项目/继续工作页/终端全屏/移动端 `/mobile` 不展示。不新增第 8 个 Workbench controller。
- 工作台内置项目预览：可在终端/文件预览旁打开 dev server 浏览器预览，自动发现终端输出和常见框架端口；本机、远端项目 shortcut 与移动端 `/mobile` 均通过 cc-partner 安全代理访问。后端只允许代理 loopback `http(s)` 且显式端口目标，避免成为开放代理；代理请求体上限为 32MB，超限返回 413 且不转发上游；上游响应体必须流式转发，避免超大或持续分块响应在后端整包聚合；远端项目必须在 owning device 上发现和访问目标，当前设备只暴露 relay proxy；前端预览 iframe 必须 sandbox 隔离且不允许 `allow-same-origin`，避免预览项目 JS 同源访问 cc-partner API；候选来源文案由前端按当前语言渲染，不依赖后端 label 字段
- 浏览器自动验证（A5）：在 owning device 按需启动 ephemeral managed Chromium（Chrome for Testing headless-shell 固定版本），仅绑定 live preview registry 的 `previewId`（不接受任意 target URL/CDP）；默认一键 smoke 产出 accessibility snapshot、console 摘要与 screenshot evidence；click/fill 只接受 generation-bound opaque nodeRef 并真实执行 CDP（禁止假成功）；fill 拒绝 password/file/hidden 且 value 永不写入结果/日志；console 订阅 CDP 事件并脱敏；engine 空闲 60s / 最长 30min 退出，cancel 时 kill child 并 await 后再删临时 profile；RemoteRelay 经 capability 门禁代理到 owner（`workbench.browser-verification.v1`）；不导入 cookie/profile、不提供任意 JS eval；验证不可用时现有 iframe 预览仍可用
- 项目自动化控制台：Orchestrator 自动化保持项目级语义，入口位于 Workbench 顶层标题区，文案为“项目自动化 / Project Automation”，不再混入终端工具栏或文件预览工具组；控制台顶部必须明确“范围：项目”并显示当前项目名，主体默认只嵌入项目级任务泳道看板、运行时状态条、remote outbox 区（pending/sending/failed）和创建任务入口；failed outbox 在该区提供 Retry/Discard，pending/sending 仅展示状态无动作；任务列表加载完成后即使当前项目没有任务也必须显示固定空泳道，不用“暂无任务”提示替代看板；不自动选中第一条任务，也不常驻展示任务详情或 Evidence；用户点击具体任务后，任务详情与 Evidence 才在右侧抽屉中显示，关闭抽屉后回到纯看板。“项目自动化”按钮是开关：关闭态点击打开控制台，打开态再次点击直接切回终端视图，控制台内部不再提供“返回终端”按钮。创建任务入口必须是独立弹窗，不固定占用控制台页面；桌面端弹窗必须脱离控制台滚动区域和终端全屏层，避免被工作台底层内容遮挡；弹窗支持用户手动填写标题、目标和验收标准，也支持用户输入简单 Prompt 后由 AI 自动完善这三项，AI 只填充表单，用户通过“创建到 Backlog / 创建到 Todo / 创建并启动”三个按钮确认创建，按钮直接决定 `createAction`，不得用单一提交按钮隐式默认 Backlog。打开控制台时 worktree 管理条必须完全隐藏，终端/文件层可隐藏但 xterm DOM 必须保持挂载并停止接收输入；自动化控制台自身只在打开态挂载可见面板，且作为中心工作区的正常文档流内容撑开高度，避免隐藏或绝对定位空层留下黑屏；任务运行后才绑定 worktree/session。嵌入模式下 blocked 任务的“打开执行现场”入口应应用任务 deep link 并切回终端视图，让对应项目 / worktree / session 聚焦结果可见。旧 `/orchestrator` 深链应重定向到 `/workbench`，侧栏不再提供独立自动化主导航项
- Orchestrator 创建任务契约：所有桌面、移动端 HTTP 和 P2P create 入口统一使用 `createAction: 'backlog' | 'todo' | 'start'`，缺省为 `backlog`。`backlog` 创建 legacy Draft + `workflowState=backlog` + `runState=idle`；`todo` 创建 legacy Queued + `workflowState=todo` + `runState=idle`；`start` 先按 Todo/Idle 落库，再 best-effort 触发 scheduler dispatch/refresh，Settings 关闭、容量不足或 runner 准备失败不得回滚创建，也不得启用 delivery。
- 添加项目文件夹：用户点击项目文件夹区右上角 `+` 后可选择本机项目或局域网设备项目；本机项目继续使用系统目录选择器；局域网设备项目展示已发现设备并打开应用内远端目录选择器，用户可直接浏览远端设备目录、选中项目文件夹并进入 Workbench，不要求该项目已在远端 Workbench 中预先添加或授权；选中远端路径后本机保存 remote project shortcut，远端设备自动创建或复用对应项目记录
- 远端项目离线处理：远端项目卡片应标识 Remote/远端与设备名；当远端设备不在线时，当前远端项目展示离线提示，并暂停终端输入、分屏/关闭 pane、session 重命名/关闭、worktree 创建/commit/push/merge/remove、文件新建/保存/格式化/重命名/删除和 Prompt 优化等写操作；侧栏移除 remote shortcut 仍可执行；后续任一 sessions/worktrees/files/git/pathInfo 读取成功后恢复可写状态
- 远端后端运行时：远端设备的 P2P HTTP、文件传输、Workbench session/event、Git/worktree 和 Orchestrator scheduler/runner/delivery 路径必须能在独立后端进程中运行，业务层不得依赖 GUI AppHandle；GUI 模式只负责把后端事件转发给前端和管理后端进程生命周期，启动时自动确保后端进程存在，关闭时由用户选择是否同时停止后端
- 项目切换：左侧栏项目文件夹列表切换当前项目，中央 worktree/window 列表和右侧文件夹按当前项目刷新；每个项目卡片右下角显示已打开 terminal window 数与 pane 总数，而不是固定“进入工作台”文案；旧项目的异步请求结果不得覆盖新项目 UI
- 移动端 Workbench：桌面端 HTTP server 默认提供 `/mobile` 普通浏览器入口，面向本机/局域网使用，不要求访问 token；合法 LAN peer 与桌面/native peer 能力相同，UI 必须明示“同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份”，不得写“已认证/可信设备/安全设备”或可切换 LAN 模式。HTTP server 尽量使用固定默认端口，端口被占用时自动向上 +1，避免移动访问链接每次启动随机变化；桌面端全局侧栏左下角颜色模式按钮右侧提供手机按钮，点击后展示可复制访问链接与二维码及固定风险提示，不在 Workbench 右侧检查器内嵌该信息。移动端先加载最近项目列表，本机项目和远端项目快捷方式都可进入 worktree、terminal、files、git、prompt 和自动化面板；远端项目通过 `/api/mobile/workbench/*` 走“手机 → 本机 → 远端设备”二级代理，由本机复用 commands 层 remote-aware helper，再转发到 owning device，避免移动端另写一套远端逻辑；手机竖屏导航默认收起，用户需要时通过顶部按钮打开。默认 active worktree 优先主工作区，默认 active session 依次优先匹配当前 worktree 且 running、任意匹配当前 worktree、任意 running、首项，并在移动端 shell 状态栏展示当前项目、worktree 和 session；状态栏中 project/session 保持静态，worktree pill 在本机或远端项目且非 busy 时作为 dialog 触发按钮打开 bottom sheet；移动端 worktree 状态分类遵循真实 DTO 的 `conflicts > 0` 时 conflict、否则按 clean 判定 dirty/clean，worktree switcher 在本机或远端项目且非 busy 时开放，顶部 worktree pill 打开的快速切换器仅提供查看、刷新、切换和进入完整 Worktrees 面板，不提供删除/合并；完整 Worktrees 面板点击工作区卡片成功切换后直接进入 terminal 面板；删除/合并等破坏性动作只允许非主 worktree 且非 busy；移动端终端面板展示真实 xterm，终端输出区一指上下滑动必须映射为 xterm scrollback 并阻止浏览器页面滚动/地址栏显示隐藏，首屏通过 `/api/mobile/workbench/sessions/replay` 读取历史输出，后续通过 `/api/workbench/events` NDJSON 增量 buffer 更新，输入、resize、focus、新增 pane（固定使用 down split-pane）、switch-pane、close-pane 和关闭 window 均调用 `/api/mobile/workbench/sessions/*` HTTP routes；移动端不暴露左右/上下分屏选择，只提供新增 pane、切换 pane、关闭 pane；终端面板提供全屏模式，全屏时隐藏移动端 shell、标题、window tabs、加载/错误提示等外围内容，只保留 pane 功能行、退出全屏按钮和当前终端输出；且在 focus/新增/切换/关闭 pane 后调用 zoom-pane 幂等保持 tmux window zoom，只显示当前 active pane；不新增第二套 tmux/session/window/pane 模型
- 移动端自动化面板：自动化作为移动端 shell 主导航里的项目级同级面板，入口紧跟项目面板之后，不放在顶部 worktree quick switch 中；选择本机或远端项目后，面板通过 task-view route 读取 local/remote/pendingRemote tagged union，真实任务按 `task.workflowState` 渲染为紧凑 grouped-list，不实现横向拖拽；remote outbox 单独展示，不参与 workflow 分组、selection 或 evidence：pending/sending 只展示待发送状态且无用户动作，failed 提供 Retry/Discard（Discard 需确认）。每个真实任务行展示 workflowState、runState、attemptPhase、lastRuntimeMessage、Claude session/transcript runtime 摘要以及 local/remote 来源，缺失 runtime 字段显示 unknown。点击任务展开详情，详情展示 title、goal、acceptance、workflow/run/attempt、runtime summary、blockedReason、evidence 时间线，并提供“打开执行现场”动作；当任务带 worktreeId/sessionId 时，该动作切换到现有移动端 terminal 面板并聚焦对应 worktree/session，不新增第二套 terminal 模型。创建任务入口是独立弹窗而不是固定内嵌表单，弹窗支持手动填写任务标题、目标和验收标准，也支持输入简单 Prompt 后调用 `/api/orchestrator/tasks/complete-prompt` 让 AI 自动完善三字段；AI 完善只填充表单，用户确认时可选择“创建到 Backlog”“创建到 Todo”或“创建并启动”，HTTP `createView` 请求必须携带非空 `clientRequestId` 并显式传 `createAction: backlog|todo|start`；远端离线时返回 pendingRemote outbox 项并在单独 outbox 区展示，成功后关闭弹窗、清空表单并更新列表；无项目或请求失败时展示对应空态/错误。
- Worktree 管理：每个项目默认有一个主 worktree，路径为用户添加的项目根目录；项目载入时必须读取 `git worktree list --porcelain`，把磁盘上已有的 Git worktree 同步进顶部 worktree 管理层，因此用户选择项目后应立即看到主工作区和已有 worktree 工作区；顶部 worktree 管理层只负责创建功能 worktree、切换 active worktree 和移除非主 worktree。创建 worktree 时点击“新建 worktree”先展开页面内表单，分支前缀从固定类型（如 `feature` / `fix`）中选择，用户只输入后缀，确认后组合为 `<prefix>/<suffix>` 并在应用数据目录下执行 `git worktree add -b <branch>`；创建成功后应自动创建 1 个绑定该 worktree 的 terminal window，默认包含 1 个 pane，并切换到新 worktree/window；如果 window 创建失败，不回滚已创建 worktree，只提示终端创建错误；如果同名本地分支已存在但已合入 base/HEAD，后端应先删除旧分支再重建，未合并则返回可操作错误；不得依赖 WebView 可能不可见的浏览器阻塞弹窗。移除 worktree 前必须先关闭该 worktree 下的 terminal window。第一期不做 Git diff 面板、交互式冲突解决和 PR 创建
- Git 提交树：右侧检查器提供“项目文件夹 / Git 历史 / 项目笔记”tab；Git 历史 tab 顶部展示当前 active worktree 的 clean/dirty/conflict 状态和 Commit、Push、Merge 操作，下面只查询当前 worktree `HEAD` 可达的 Git DAG，不混入同仓库其他 worktree 尚未合并的分支提交，切换 worktree 后必须刷新为该工作区自己的历史；提交列表采用类似 VS Code Source Control Graph 的紧凑连续泳道，分支收束时平滑换 lane，以不同节点形态区分 HEAD、merge 与普通提交，并在两行内展示提交摘要、内联 ref badge、短 hash、作者和相对时间，ref badge 区分本地分支、远端分支和 tag；commit 点击后不弹手写输入框，后端执行 `git add -A`、读取 staged diff/stat，并在 active worktree cwd 下用 Claude Code 项目上下文模式生成 commit message 后 `git commit -m`，无可提交改动时作为 no-op 返回最新 worktree 状态；push 优先复用当前分支 upstream 执行 `git push`，没有 upstream 时只选择 `origin` 执行 `git push -u origin <branch>`；完全没有 origin/upstream，或只有 `*-upstream` 等源码上游 remote 时返回配置提示；Git 历史工具条的 Push 按钮只在后端 status 判定当前 worktree 有可用推送目标时启用，本地未发布且没有 origin/upstream 的项目必须禁用；merge 在主 worktree 上：当存在可收集本地分支（排除 home、其他 worktree 占用、已完全合入 home 的分支）时可点；home 探测顺序为 origin/HEAD → 本地 main → 本地 master；后端在隔离 integration worktree 内按分支名顺序 `--no-ff` 合进冻结 home OID，冲突走同一套 Claude Code 隔离解冲突；不关闭主工作区终端、不删除主 worktree；成功后把真实主工作区切回 home 并删除已合并源分支；live 不在 home 时用 CAS `update-ref` 发布再 checkout；live 已在 home 时 `--ff-only`；merge 非主 worktree 时先要求源 worktree clean，有未提交改动立即返回可读错误；源 clean 后后端自动关闭该 worktree 下所有 terminal window/pane，再检查主 worktree clean 并冻结主分支、主 HEAD OID 与源 worktree 实际 HEAD OID；后端在应用数据目录的内部 detached integration worktree（不写入 SQLite、不出现在用户 worktree 列表，且必须位于 main/source checkout 之外以避开 watcher）基于冻结主 OID 执行 `git merge --no-ff <source_oid>`，真实主 worktree 在隔离阶段保持不变；merge 冲突时调用本机 Claude Code CLI（设置页 AI 的 CLI 路径和模型）在该隔离 worktree cwd 下以项目上下文 headless 模式处理：prompt 只传冲突相对路径，不再内嵌全部文件全文或要求完整 JSON 回传；CLI 只开放 Read/Edit/Write/Grep/Glob，不开放 Bash，dontAsk 权限仅预批准隔离 worktree 根内读写和只读搜索，CLI 返回后由 Git 强制限制最终改动只能落在原冲突路径；Claude 按需读取上下文并直接编辑冲突文件，返回后后端拒绝原冲突清单外的 unstaged/untracked 改动，复查所有 marker 已移除，再 stage all 并完成 merge commit；失败只 abort/清理隔离目录，不污染真实主工作区；产物必须严格为 parents `[main_oid, source_oid]` 的双父 merge commit，发布前再次确认真实主 worktree 的分支、HEAD 与 clean 状态，以及源 worktree 的分支、HEAD 与 clean 状态仍与冻结快照一致，然后以 `--ff-only` 短时发布；main 或 source 已漂移/变脏时拒绝发布并保留源的新提交或改动；若 owner 在发布后、cleanup/ledger 成功落盘前重启，同一 `clientOperationId` 必须按冻结 intent 精确确认已发布 merge，幂等完成隔离目录清理，在源仍匹配冻结输入时清理源 worktree，并收敛为 succeeded，不重复调用 Claude；只有发布成功且源仍未漂移时才删除该 worktree session 元数据、Git worktree 本身和已合并本地分支；merge 命令保持返回 `{ok, worktreeId, stages}`，并通过 `workbench:merge-progress` 事件按 `checkSource/closeSessions/mergeMain/resolveConflicts/cleanup` 阶段推送 `pending|running|completed|failed|skipped` 状态，事件 payload 带 `projectId`；前端必须按项目缓存正在运行的 merge 阶段，切换 project 或 worktree 时继续接收后台事件，返回原项目后恢复最新阶段，且 command 终态不得因 active worktree 已变化而被 stale guard 丢弃；阶段条在成功完成后自动隐藏释放 Git 历史空间，失败时保留错误阶段；切换项目/worktree、commit、push 或 merge 后刷新，commit 失败也应刷新当前 worktree 状态，空仓库或无提交时显示空态
- 项目笔记：桌面右侧检查器第三 tab 提供当前 Workbench 项目一份 Markdown 笔记（切 worktree 共用）；复用现有 Tiptap 富文本所见即所得，500ms debounce 写入本机 `~/.cc-partner/data.db` 的 `workbench_project_notes`；不写仓库文件、不进 Git、不 LAN/GitHub 同步、不进备份域；远端项目记在本机 shortcut `projectId` 上，不代理到对端磁盘；关 GUI 前经 pendingWrites flush；删除项目记录时级联删除笔记。第一版 `/mobile` 不提供笔记面板
- tmux 依赖管理：应用启动后自动检测 Workbench 所需 tmux；macOS/Linux 使用原生 tmux，Windows 使用默认 WSL 发行版内的 tmux。状态卡应展示 checking/ready/missing/installing/unsupported/failed 状态、后端、版本、路径、安装命令预览和最近输出；缺失且存在可用安装通道时用户可确认安装，安装完成或失败后可重新检测。tmux 不可用时仍允许普通 PTY fallback，但不承诺 window/pane 语义和重启后 shell 上下文恢复
- 终端窗口与 pane：每个 worktree 优先对应一个真实 tmux session，同一 worktree 内的前端 tab 对应 tmux window，window 内分屏对应 tmux pane；不同 worktree 的 window 必须处于互相独立的 tmux session，不能在 tmux 底部 status/window 列表中互相可见。tmux session 名应由项目名、worktree 显示名和 worktree id 短尾缀派生，不加固定 `cc-partner-worktree` 前缀；缺少显示名时才回退内部 id，避免 status bar 只显示无意义 hash，同时避免清洗后重名 worktree 碰撞。运行期 attach 必须在连接当前 worktree session 后切到对应 tmux window，切换 app tab 时必须让后端执行 tmux `select-window` 切到 tab 绑定 window，用户通过 tmux 底部 status bar 或快捷键切换 window 时顶部 app tab 也必须跟随当前 worktree 的真实 tmux current window，确保 app tab 与 tmux window 一一绑定；没有 tmux、WSL 路径不可转换或后端不可用时退回单普通 PTY window。新建 window 在 active worktree 根目录启动系统 shell（用户自行运行 `claude` 或其他命令），window 元数据持久化 `worktree_id` 与 `cwd`，重新打开 cc-partner 后应恢复之前打开的 window 列表并保持其所属 worktree
- 终端上下文恢复：macOS/Linux 环境优先使用原生 `tmux` 承载真实 shell 上下文，Windows 环境优先通过默认 WSL 发行版内的 `tmux` 承载上下文（盘符项目路径转换为 `/mnt/<drive>/...`，`\\wsl$\<distro>\...` / `\\wsl.localhost\<distro>\...` 转为发行版内 Linux 路径）；应用退出时只断开 attach，重启后重新 attach 到 worktree tmux session 的原 window；没有可恢复 window 时重建 window 并保留 tab 元数据
- 桌面终端始终展示 sidecar PTY 的真实回显，不做前端本地 echo；GUI 正常态通过 owner-sequenced control live stream 接收输出，断线按 cursor catch-up，Gap 时先 replay 再恢复 live。输入按 terminal session 串行提交，响应不确定时不得自动重放；已挂载 xterm 消费 live 增量，完整 bounded buffer 仅用于挂载与 resync。
- 终端输出：PTY 输出必须保留中文与符号等 UTF-8 文本完整性；前端 xterm 必须按 PTY/tmux 原始控制序列渲染，不能启用会改写换行的 `convertEol`；远程项目同一时刻只实时传输当前选中且 `running` 的 terminal window 的 `terminalOutput`/`terminalResync` 正文，后台窗口仅以无正文 cursor 帧推进全局事件游标；`disconnected`/`exited` 历史 tab 可以展示但不得触发 focus、replay、resize、输入或实时正文订阅；切换窗口先恢复该窗口 replay baseline 再继续增量，Gap 恢复也只能 replay 当前窗口，切换设备、本机窗口或离开工作台必须停止旧远端窗口正文流；terminalStatus、mergeProgress 与 agentRuntime 等轻量事件仍保持实时。终端面板 CSS 和 xterm theme 必须从 `--terminal-*` design token 读取，并随应用浅色/深色主题切换同步更新；tmux 底部 status bar 必须由 Workbench 覆盖为无硬编码暗色的本地样式：保留 `session ›`、`#I:#W` 标签结构和选中 window 的浅蓝块，inactive window、右侧时间和整条 status 背景必须使用 terminal default 颜色，不能在浅色主题下保留深色工具条、深色 inactive 标签、暗色时间或 underline 黑线；Workbench 必须 session-local 强制 `mouse off`，避免用户全局 `mouse on` 时滚轮进入 tmux copy-mode（浏览模式）并吞掉键盘输入，工作台复制走 xterm 选区；同时必须 server-level 追加 `terminal-features xterm*:mouse`，让 Claude 等 TUI 的 DECSET 1000/1006 透传到外层 xterm（这不会打开 tmux 自己的 mouse/copy-mode）；桌面 xterm 在 alternate screen 必须拦截滚轮：固定向 transcript 左上角发送 SGR 64/65，禁止使用指针坐标或按固定行数估算输入区高度，也禁止 PageUp/方向键（resume 后输入区高度不固定，PageUp 在 Chat 输入聚焦时无绑定，方向键翻输入历史）；后端启动 Workbench PTY 客户端时必须显式设置 `TERM=xterm-256color` 和真彩色环境，不能继承 `TERM=dumb`；切换 worktree 或 app tab 时必须保留当前项目下所有 terminal window 的 xterm 实例，只隐藏非 active worktree/window，不能卸载后 replay 原始终端流，inactive 常驻 xterm 即使产生 `onData` 也必须丢弃，同时必须同步底层 tmux current window；前端应轻量同步 tmux current window 到顶部 app tab，不能因为保留 tmux status bar 而让两层 window 选中态分裂；Workbench 路由切出时终端输出缓存必须继续由 AppShell 生命周期的常驻 Provider 接收，切回后 xterm 从该缓存 replay，不能把 terminal output 监听和 buffer 只放在 Workbench 页面内部导致 Claude Code/tmux TUI 丢失屏幕态；buffer 截断导致必须 replay 历史输出时，不得把 xterm 生成的设备能力响应写回 PTY，且 replay gate 释放必须延后一轮 macrotask；活跃 xterm 已写入旧 buffer 后，如果常驻 buffer 达到上限并等长滑动截断，前端必须根据旧 buffer 后缀与新 buffer 前缀的重叠追加新尾部，不能只比较长度；创建终端时应使用当前终端可见 viewport 的真实 cols/rows 作为 PTY 初始尺寸，终端运行时 resize 也必须按同一可见 viewport 计算，避免交互式程序状态栏和命令行内容出现替换字符、首屏错位或内容超出工作台可视宽度；桌面端终端工具栏必须提供“适应尺寸”按钮，用户从移动端回到 PC 后可手动把当前 running session resize 回桌面端可见 viewport
- 桌面终端滚轮分流（覆盖上一条中的旧 alternate-only 表述）：应用启用 mouse tracking 时（包括 Claude 以 normal buffer 承载虚拟 transcript）必须拦截滚轮、先退出 xterm 本地重绘历史，再固定向 transcript 左上发送 SGR 64/65；只有未启用 tracking 的 normal buffer 才使用本地 scrollback，alternate screen 始终走固定 SGR。禁止使用指针坐标、PageUp 或方向键。
- tmux `terminal-features[]` 中的 `xterm*:mouse` 必须由后端幂等维护：缺失时仅追加一次，已有精确重复项时只保留首项，并保留用户配置的其他 terminal feature；禁止在每次创建、恢复、聚焦或 resize 时无条件 `-sa` 追加。
- 终端分屏：中央终端区每次 attach 当前 window；桌面端左右/上下分屏调用 tmux `split-window -c <window cwd>` 创建真实 pane，移动端新增 pane 固定走上下分割但随后必须 ensure zoom，让手机端只看到当前 active pane；新 pane 必须从该 window 绑定的 worktree 根目录启动而不是继承当前 pane 中用户 `cd` 后的位置；切换 pane 调用 tmux `select-pane -t <window-target>.+` 在当前 window 内循环切换 active pane；关闭 pane 多 pane 时调用 tmux `kill-pane` 关闭当前 active pane，最后一个 pane 被关闭时应关闭所属 window 并同步移除顶部 tab，不应弹“只有一个 pane”的错误
- 会话状态：右侧只展示当前 window 状态，包含设备、项目名、worktree 名、工作区路径、window 名、命令、状态、运行时长、尺寸、开始时间和退出码
- Agent 自动标题：用户在 Workbench 普通终端内与 Claude Code、Codex 或 OpenCode 对话时，window/tab 名应跟随 provider 生成的真实会话标题；Claude transcript watcher 随 running terminal/worktree 启动，不依赖用户先打开历史搜索。标题在 terminal Ready/native/pane 绑定暂未完成时必须保留并重试，同一标题只有在成功落地、已经同名或被用户手动命名锁定后才能去重；没有 native session 强绑定而使用 cwd 兜底时，标题来源更新时间不得早于当前 window 启动时间，防止已关闭会话的旧标题污染新窗口；多 terminal 同 cwd 或多 pane owner 不明确时继续 fail-closed。自动标题更新必须通过本机桌面、远端桌面和移动端事件链实时投影，Gap 后用 session inventory 对账，无需重连或重新进入页面。
- 会话操作：支持聚焦、读取当前聚焦、重命名、关闭；聚焦 window 必须同步切换底层 tmux current window，读取当前聚焦必须只在当前 worktree 的 tmux session 内把 current window id 映射回 app sessionId；关闭/应用退出清理时如果底层进程已自然退出或被系统回收，应视为清理成功，不向用户展示 No such process 类 IO 错误；关闭 tab 会删除持久化 window 并销毁对应 tmux window，应用退出只清理当前 PTY attach，不能删除可恢复 window 元数据或销毁 worktree tmux session
- Prompt 优化浮层：终端工具栏提供 Prompt 优化入口，默认也可轻按 Control 唤起；浮层悬浮在当前终端输入光标附近，只渲染一个原始 Prompt 输入框，每次从关闭态重新打开时清空输入；首次快捷键触发打开并聚焦原始 Prompt 输入框；浮层打开后再次触发快捷键时，空输入直接关闭浮层，非空输入才自动优化；输入框内非空时按 Enter 与再次触发快捷键等价，Shift+Enter 保留换行，输入法 composing 状态下的 Enter 不提交；后端按设置语言把优化后的 Prompt 边生成边流式写入当前 running active session，完成后自动关闭浮层；优化请求绑定 active worktree 根目录以加载该工作区 CLAUDE.md 上下文，但输出仍必须是需求方视角的直接委托式 Prompt，不能把项目文档规则扩展成向用户确认 `docs/` 或写文件的澄清问题；小组件不显示“优化/填入终端/中文优化版/English optimized/关闭”等按钮或双语结果区，且不请求双语优化
- 项目文件夹：右侧文件树绑定 active worktree 根目录，支持刷新、展开/收起、选中文件/文件夹、新建文件、新建文件夹、重命名、删除确认、复制相对路径，并展示名称、类型、相对路径、大小、修改时间和父目录；点击文件节点在中心文件工作区打开 tab，预览 tab 标签直接显示文件相对路径，格式化/保存/返回终端按钮与 tab 同行显示，不再渲染第二行文件头部或类型标签；支持多文件 tab 激活/关闭，全部关闭后回到终端；文件 tab 未关闭时，终端工具栏的文件预览入口位于按钮组最右侧，用户可在终端和文件工作区之间双向切换；项目或 worktree 切换时清空文件工作区，旧异步请求不得污染新上下文；重命名/删除路径后已打开 tab 必须同步新路径或关闭
- 文件内容浏览/编辑：图片只读预览；CSV 只读表格预览；SQLite 只读枚举表并预览前 100 行，不执行用户 SQL；代码、Markdown、HTML、JSON、TOML、YAML 和普通文本走文件工作区编辑，代码编辑器需要高亮插件体验；Markdown 支持源码、预览和分栏模式，预览模式可直接编辑，体验接近 Typora，预览中的相对图片通过 active worktree 根内只读资源通道内联，外链、data/blob、绝对路径、根外路径和跨根 symlink 不加载且不写回源 Markdown；HTML 支持源码、sandbox 渲染预览和分栏模式，预览可加载 active worktree 根内相对 CSS、图片、字体和媒体资源，外链、data/blob、绝对路径、根外路径和跨根 symlink 不加载，外部/内联 CSS 的相对 `url()` 按 CSS 文件自身路径解析；JSON/TOML/YAML 提供格式化按钮，保存前必须做语义校验；保存文本文件使用 baseHash 乐观锁防止覆盖外部修改
- 终端低延迟（P1，同版本桌面）：正常态由 GUI 经 loopback control **NDJSON stream** 消费真实 PTY 输出（**stream-first**），禁止以固定 250ms catch-up 轮询作为正常路径；catch-up / 慢轮询仅用于 mixed-version stream unsupported 与显式恢复。每 session 最多 1 个 in-flight `writeInput`，失败/不确定批次**永不自动重放**同批输入；live xterm 以 session 外部 store 的 live delta 直写，禁止 live 路径 full-buffer KMP / React effect 背压。后端 replay 有界 120k Unicode scalar 增量 ring，前端 buffer 200k UTF-16 摊销裁剪。Gap/owner change 走显式 resync（含 remote session watch retain-before-release）。**L3 release GUI 性能**（本机 key-to-visible p95≤50ms / p99≤100ms，owner publish→GUI listener p95≤20ms，1000 混合输入零丢失/重复/重排）截至分支 `sdd/p1-workbench-terminal-low-latency` @ `a25f8caa` **尚未在真实 release GUI 上测量**，交付状态为 **`L3 GUI latency: NOT VERIFIED`**；自动化 L0–L2 与 P1 Superpowers 双审通过不得代替 L3 实测。
- 当前仍不做 Git diff 面板、PR 创建、交互式冲突解决、会话日志持久化归档和批量同步副本；运行期 replay buffer 仅用于移动端首屏和终端重挂载恢复最近输出，不等同于持久会话日志

- 终端输入传输（覆盖上文中“输入走 sessions HTTP routes”的旧描述）：桌面 xterm 与 mobile xterm 的交互输入固定走 `cc-partner.terminal-input.v1` 常驻 WebSocket；桌面 invoke 只等待 GUI Rust 有界队列接纳，mobile 使用同源 `/api/mobile/workbench/terminal-input-stream`，本机到远端 owning device 使用 `/api/workbench/terminal-input-stream`。ACK 仅在 PTY write+flush 后返回且不阻塞下一帧发送；断线时已发送未 ACK 输入结果未知、不得自动重放；两个 xterm 入口不得回退 `/sessions/write`。终端输出继续使用既有 NDJSON/replay/gap 通道。

### 2.16 Orchestrator 自动编排器

**描述**：在 Workbench 项目之上提供自动编排器，用内部任务队列把需求拆成可排队、可运行、可验证和可交付的项目级任务。

**功能点**：
- 任务队列按项目隔离，支持按 Backlog / Todo / In Progress / Human Review / Rework / Merging / Done / Canceled 固定业务泳道展示任务；桌面端拖拽只允许移动到前一个或后一个相邻泳道，且不隐式启动 Runner、交付或远端副作用；scheduler 只自动领取本机项目中由已解析 workflow 标记为 active 且运行态为空闲或阻塞的任务，内置默认 active states 为 Todo/Rework，项目 `WORKFLOW.md` 可在固定枚举泳道内覆盖 active states，但不能创建任意新泳道
- 兼容旧任务的创建 Draft、手动入队、自动验证闭环、Blocked 重试和终止语义；桌面端与移动端创建任务统一使用 `createAction`，可选 `backlog` / `todo` / `start`，缺省为 Backlog；`todo` 只进入待执行泳道，`start` 只 best-effort 触发 scheduler，Settings 关闭、容量不足或 Runner 准备失败都不得让创建失败，也不得触发交付
- 显式业务动作是 Orchestrator 的主操作入口：Start 只把 Backlog/Draft 或 Todo/Idle 任务送入 scheduler 路径，不触发交付；Request Rework 只面向 Human Review 任务，记录返工原因和 evidence 后进入 Rework 并保留执行现场；Deliver 只允许 Human Review，且必须 Settings 允许 full-auto delivery 时才进入既有交付 pipeline，否则返回可读错误；Cancel 进入 Canceled/Idle 并保留 worktree/session/evidence；Refresh 触发 best-effort dispatch/reconcile 并返回项目 id 与 dispatched 数量。旧入队、Blocked 重试和终止入口保留兼容，不作为新 UI 主语义
- **WORKFLOW 向导**：Orchestrator 面板提供权威 `get/validate/save` 生命周期（能力 `orchestrator.workflow-document.v1`）；save 使用 CAS `expectedHash`，漂移 conflict `workflow_document_changed` 时保留用户草稿并允许 reload；**save 不 dispatch，也不能启用或改变 delivery**（delivery 仅 Settings）。向导可处理 missing 模板创建、valid 摘要、invalid 诊断行聚焦；桌面/远端/移动共用同一 helper
- **运营系统通知（非 Inbox）**：偏好可控，默认 Human Review / Blocked / remote outbox failed / Agent needsInput / Agent failed / experiment decision 开启、Task Done（completed）关闭；仅消费 owner 事件 `{kind,opaqueSourceId,stateVersion,occurredAt}`（可选 relay 信封 ownerInstanceId/sequence）；title/body 只用通用 i18n，**禁止**任务/项目/goal/diff/evidence/terminal/路径 明文，**禁止** actionType/onAction/extra 业务回调；control snapshot `POST /api/backend/control/operational-notifications/snapshot` 为 loopback+token only，**不是** LAN capability。通知与 Attention 只导航，都不执行 Deliver/Rework/Retry 等业务动作
- 任务模型保留 tracker 对接预留字段 `source`、`externalId`、`externalIdentifier`、`externalUrl`、`externalState`、`externalLabels`；`source` 默认 `internal`，`externalLabels` 为 `string[] | null` 并由后端稳定存储为 JSON array。这些字段必须随本机 DTO、远端 P2P 协议、pending outbox request 和 remote mirror payload 保留，mixed-version 远端缺字段时安全视为 `null`；本轮不参与 scheduling、状态机或交付决策
- 自动化配置统一放在 Settings 的独立 tab 中，控制 scheduler 启用、最大并发、全局默认验证命令、full-auto 交付开关、运营通知 kind 开关和重试相关行为；运行偏好按设备持久化在全局 AppConfig。项目根 `WORKFLOW.md` 是可选项目级策略覆盖：缺失时使用内置默认；存在但解析或校验失败时阻止该项目未来 dispatch，并在 completion 阶段写 failed evidence 后进入 Blocked，不静默回落到 Settings；有效 `WORKFLOW.md` 可覆盖 active states、Runner Prompt 模板和 `validation.commands`。自动交付仍只能由 Settings 控制，`WORKFLOW.md` 不能启用或改变 delivery。后端 legacy `orchestrator_project_config` 仅保留存储、兼容和调试接口，不作为用户可见配置路径或运行时语义。Settings 验证命令以多行文本维护，保存时 trim/filter 空行并限制数量与长度；项目 workflow 未声明 `validation.commands` 时才沿用 Settings 作为全局默认
- 远端项目的 Orchestrator 任务以远端 cc-partner 为权威来源；本机 remote shortcut 只通过 P2P Orchestrator route 创建、列出、Start、Request Rework、Deliver、Cancel、Refresh、重试和读取 evidence，不把远端任务复制成本机可调度任务。真实 remote task 的业务动作必须转发 owning device。本机 remote outbox 生命周期为 pending / sending / mirrored / failed / discarded：pending 与 sending 只展示待发送状态，不允许用户执行 action；failed 仅在 Orchestrator 自动化控制台（桌面与移动端）提供 Retry 与 Discard，Inbox 列表仍只导航不执行动作。Retry 只允许 `status=failed`，将状态改回 pending、清空 `last_error`，并完整保留 `requestJson` 与稳定 `clientRequestId`（不得生成新幂等键）；Discard 只允许 `status=failed`，进入 `discarded` 审计终态并保留 `last_error`/request 审计，discarded 与 mirrored 不再出现在 active/pending 列表、dispatcher 队列或 Inbox 投影。远端设备离线时，本机允许创建 pending remote task，写入本机 outbox 并展示“待发送到远端”；pending outbox 的 `requestJson` 必须保留原始 `createAction`、稳定 `clientRequestId` 和 tracker 预留字段。设备恢复在线后后台 dispatcher 自动投递，投递使用稳定 clientRequestId 防止超时重试重复创建，sending 超过 5 分钟会恢复 pending 重试，投递成功后必须在同一事务内保存远端 task id 并更新远端任务 mirror。远端 mirror 只用于离线展示最近快照，本机 scheduler、验证、交付与任务 action 不得消费 mirror 行
- Runner 必须使用 Workbench 可见的 tmux terminal 和任务 worktree，方便用户随时 takeover 或观察 Claude Code 执行现场；每轮 Runner attempt 记录 prompt、worktree、session、阶段和状态，首轮创建任务 worktree，Blocked retry 或后续修复轮复用同一 worktree、新建终端 session，并使用新的 attempt 序号；首轮 Runner Prompt 每次启动前重新解析项目 workflow 并使用 `prompt_template` 渲染，自定义模板若缺少 worktree 路径或 `ORCHESTRATOR_DEV_DONE` 完成协议，后端会追加标准 Runner guardrail；模板渲染失败会阻塞该 attempt。后端会 best-effort 关联 Claude Code 自身 session/transcript/runtime 字段，关联不到或 JSONL 损坏时显示 unknown fallback，不阻断 Runner
- **统一 Agent session runtime（A1）**：owning device 维护 provider-neutral `workbench_agent_sessions`（phase/version/关联 ID）；普通 Workbench 与 Orchestrator Runner 共享同一权威。terminal 注入非敏感 `CC_PARTNER_{PROJECT,WORKTREE,TERMINAL_SESSION,OWNER_INSTANCE}_ID`；adapter 经 app-private OSC 上报 phase（单帧 ≤16KiB、每 terminal ≤20 events/s，帧从可见输出剥离）。投影 DTO/事件/P2P snapshot **禁止** `nativeSessionId`、Prompt、terminal bytes、transcript path 与凭据；能力 token `workbench.agent-runtime.v1` 仅协议协商。Gap 后取 active snapshot（≤1000）再排空事件；旧 peer 无能力显示 unsupported，不回退猜测 Claude session。Orchestrator 一个版本 dual-write 旧 `claude_session_id`/`runtime_*`，completion 先更新 Agent runtime 再进验证
- **Agent 状态投影（A2）**：Desktop/Mobile 消费 A1 runtime（非 Orchestrator 旧 Claude 字段）自动展示 provider 短标签 + phase（listener-first handshake，Gap/owner 重建）；`needsInput`/`failed` 进入 Attention v2 与默认 OS 通知；working/idle/completed 默认不进 Inbox、completed 通知默认关；Attention/通知只导航不执行输入/审批/retry
- **Agent Metadata Ledger（A9）**：Agent runtime 终态自动写入 metadata-only 历史（`agent_session_id` 唯一；可靠 usage 可 null-fill；失败不阻断 runtime）。默认保留 30 天且每设备最多 10,000 条，启动/每 24h 批删 ≤500。本机 drawer 分页（默认 50/最大 200）可滤 project/provider/outcome/time；unknown token/cost 显示「未提供」。P2P 仅 `POST /api/workbench/agent-ledger/summary`（capability `workbench.agent-ledger-summary.v1`）暴露 24h/7d/30d aggregate，**无** entry 列表/session id/prompt/path。Fleet 详情 join 7d Agent activity（field 失败不阻断其它摘要；旧 peer unsupported 不显示 0）。Settings 一键清除只删 ledger；不进入 Prompt/SSH/Scratchpad/GitHub sync。
- **LAN Agent Fleet（A6）**：控制设备仅聚合**已保存**的 local/remote project shortcut（不枚举对端全部项目）；按 owning device 批量拉取 Agent phase 计数、Attention、terminal、Git clean/dirty/conflict、browser Active/Absent、Orchestrator running/retrying 与 **device-global** scheduler slots（禁止用当前 project `slotsUsed` 推导）。P2P owner batch `POST /api/workbench/lan-fleet/snapshot`（capability `workbench.lan-fleet.v1`，≤100 projects/device，禁止 remote: 递归）；控制侧 fan-out 并发≤3、单设备 5s timeout、进程内 display cache + freshness；可见 30s safety reconcile。Project Rail 仅异常 badge（needsInput/failed）与 offline 文本（无 Fleet 二级入口）；Settings `?tab=fleet` 只读详情且**仅导航**到既有 project/Attention（旧 `/workbench/fleet` 重定向），禁止调度/迁移/复制 repo/改 concurrency/inline mutation；DTO 不含 Prompt、terminal bytes、远端绝对 path。mDNS/capability 只表达可达与协议，不称认证/可信/安全。
- Claude Code 开发 Prompt 必须要求完成代码、测试/验证和必要证据说明后，最后单独输出 `ORCHESTRATOR_DEV_DONE`；后端监听可见开发终端输出，只在检测到换行终结的独立哨兵行且该 session/attempt 仍是任务 active runner 后自动进入既有验证/交付流程，手动“完成 Agent 运行”命令仍作为用户 fallback
- 后端自动执行验证命令并调用验证 Claude 做最终裁决；验证命令来源为项目 workflow 的 `validation.commands`，未声明时沿用 Settings 全局默认；命令在任务 worktree cwd 中执行。验证命令非零退出作为 verifier 输入，不直接阻塞任务，只有 workflow 解析错误、命令启动/读取/超时、verifier CLI/JSON/schema、diff 读取等基础设施失败才进入 Blocked；验证未通过时任务进入 Rework，并在同一 worktree 新建 terminal/Claude 继续修复，直到通过或用户终止
- 目标态验证通过后，只有 Settings 中全局自动化启用且 full-auto 交付开关全部开启才自动完成 commit、推送任务分支、合并主工作区和推送主分支；默认自动化关闭或任一交付开关关闭时任务进入 Human Review 等待用户复核
- 任务进入 Blocked 或循环修复时保留原因、worktree/session 入口和 evidence 链；Evidence 记录验证输出、验证 Claude 裁决、修复 Prompt、交付阶段结果和失败摘要，供前端任务看板展示与追溯
- OrchestratorPanel 只拥有项目级任务看板、点击任务后出现的右侧详情/Evidence 抽屉、创建任务弹窗和编排状态，不展示项目级策略或配置；Workbench 拥有项目级自动化控制台挂载、项目上下文、执行现场 deep link takeover、文件和 Git 操作
- Workbench 自动化状态条通过后端 runtime snapshot 展示生成时间、最近调度 tick/dispatch 时间、scheduler 开关、workflow 来源与校验结果、全局最大并发、当前项目槽位占用、运行中任务摘要、重试/返工任务摘要、最近 scheduler/runner 事件和最近错误；状态条提供手动刷新入口与 Settings 自动化 tab 链接。snapshot 只作为观察/诊断面，不作为任务正确性的唯一来源，也不能驱动 scheduler、验证、交付或任务 action。本机项目直接读取本机权威快照（`remoteStatus=local`）；remote shortcut 必须向 owning device 拉取权威数据：桌面/本机走 P2P `POST /api/orchestrator/runtime-snapshot`（capability `orchestrator.runtime-snapshot.v1`，owner 侧 body snake_case `{project_id}`，只服务本机 local 项目、禁止递归代理），移动浏览器走 `POST /api/mobile/orchestrator/runtime-snapshot`（camelCase `{projectId}`，remote-aware，不向浏览器泄漏 owner base URL）。成功时只映射 shortcut 身份/表面 ID，保留 owner 的 generatedAt/tick/slots/running/retrying/events 等运行时字段，禁止用本机 telemetry、mirror 或 display cache 补空。远端 `remoteStatus` 四态为 `live`（在线权威数据）、`unsupported`（对端无 `orchestrator.runtime-snapshot.v1` 能力）、`offline`（真实传输不可达：设备缺失/网络/传输层 Unavailable|Timeout）、`unavailable`（协议/业务失败：InvalidResponse 或远端业务信封等），状态分支只看类型化错误，不看本地化文案或 404 猜测。桌面与移动端各自维护进程内、按 projectId 隔离的 display-only 最后一次 **live 成功** 缓存：live→offline 可展示缓存并标 `cachedAt`/最后更新时间；cold offline 无缓存则为空态，不得伪造数据；缓存不写 SQLite/localStorage/磁盘，且绝不能进入 scheduler、验证、交付或任务 action 的执行决策。

### 2.17 全局 Inbox（Attention）

**描述**：回答“现在有哪些事情需要我处理，工作才能继续”。桌面与移动端共享同一套实时投影、数量与分类语义，并只导航到既有权威业务界面。

**功能点**：
- 全局 Inbox 是实时投影，不新增 Inbox 表，不做已读/忽略/稍后/关闭/snooze，不保留已解决历史；列表只反映当前权威阻塞，无独立持久化
- v1 四类 source：本机/远端 Human Review（decision）、Blocked 任务（blocked）、failed remote outbox（blocked）、Workbench tmux 依赖缺失/失败/不支持（environment）；设备离线本身不是 source，只有它造成的 cached 任务/outbox 业务后果可以显示；mirrored/discarded outbox 与 ready/installing 依赖不投影
- **v2 增量 source**（capability `attention.v2`）：Agent `needsInput`（decision）与 Agent `failed`（blocked）由 A1 runtime 实时派生，稳定 ID `agent:needs-input:<id>` / `agent:failed:<id>`；working/idle/completed 不投影；`ExperimentNeedsDecision`/`Experiment` 仅合同预留，A4 注册后才发射。v1 响应绝不序列化 Agent/Experiment 变体；客户端优先 v2 回落 v1
- 桌面入口为侧栏固定页第二项「待处理」`/attention`（在 Github热门之后）；移动端在 Projects 后增加第二导航项「待处理」，Projects 仍为默认面板；两端对同一 snapshot 使用相同 badge（0 隐藏、1..99 数字、100+→99+）、相同分组顺序（需要你的决定 → 运行受阻 → 环境受阻）与空组省略规则；legacy 对端缺失 `attention.v1` 时移动端显式 unsupported，不得猜测旧接口
- 条目**只导航**（导航-only）：任务/Evidence 进 Orchestrator 自动化控制台，failed outbox 进自动化控制台 outbox 区，tmux 依赖进 Settings `dependencies`，Agent 进既有 terminal session；列表内与 deep link 目标落地均**不**执行 Deliver、Request Rework、Retry、Discard、依赖安装或 terminal 输入；系统运营通知同样无业务 action 回调，点击至多导航到既有权威界面
- 解决动作仍在 Orchestrator/Settings 原界面；Deliver、Request Rework、task Retry/Refresh、outbox Retry/Discard、依赖 install/recheck 成功后立即失效并刷新 Inbox；失败动作不失效；页面可见时 10 秒轮询仅作远端/外部变化兜底
- 远端在线条目标 live；网络失败回退最近 mirror 时保留真实 `last_synced_at` 并标 cached，不得伪装 live；任一非网络 source 失败则整次快照失败，不得返回误导性部分快照
- 有快照时刷新失败只标 stale 并保留列表/badge；初次失败不显示虚假数字；旧请求不得覆盖新请求
- 目标已解决或状态变化时回退 Inbox 并提示“事项已解决或状态已变化”；remote shortcut 被移除后其 orphan failed outbox 不再投影

## 3. 非功能需求

### 3.1 跨平台
- 支持 macOS、Windows、Ubuntu
- 使用 Tauri 打包为各平台独立桌面应用
- 应用启动后主窗口默认进入系统全屏显示

### 3.2 性能
- 文件传输速度应充分利用局域网带宽
- UI 操作不应因网络/IO 阻塞而卡顿（异步架构）
- 截图工具条响应时间 < 200ms，截图/标注合成不应阻塞工具条出现，选区框与工具条在快照捕获开始前应保持稳定可见

### 3.3 可靠性
- 文件传输支持断点续传
- 数据库使用 SQLite，数据持久化可靠
- 设备离线后重新上线，同步应能恢复
- 关键前端 IPC/HTTP 成功体在写入 UI 状态前经 runtime schema **fail-closed** 解码；畸形/未知必填失败不得用 payload 原文刷状态，错误只暴露 contract/path/primitive kind（及 request id），不序列化业务 payload；legacy 默认值必须在对应 schema 字段显式声明
- Prompt / 速记本 / Settings 等写路径：乐观更新在 API reject 时必须回滚或保留 dirty 草稿并提供重试；关闭 GUI 前 flush 全部 pending write，flush 失败中止关闭
- **Safe-save / operation context**：Settings/ClaudeMd 等编辑保存持有 `editVersion` + `requestSeq`；success 总是更新 baseline，仅 version 未变且 draft 仍等于 submittedSnapshot 时才 hydrate 回填；旧 seq 的 success/error 不改当前态。Workbench Git 等长操作以 `WorkbenchOperationKey` / `isCurrentOperation` 守卫，切换 project/worktree 后旧 success/catch/finally 不得写新 context；mutation timeout/network 结果为 `unknown`，只按稳定 `clientOperationId` 对账，禁止盲重放
- **Mobile transport**：query overall 默认 15s（含 decode）、mutation 30s、longMutation 180s；query/只读 POST 对 timeout/network 最多重试 2 次（页面可见且非 callerAbort）；project/worktree/context 切换 abort 旧 query；mutation 仅在稳定 operation id + 幂等/ledger 对账时可重送
- **Accessible async feedback**：异步成功用 `StatusMessage` `role=status`（polite），阻断失败用 `role=alert`（assertive）恰好一次；busy 按钮保持稳定 accessible name；`TagInput` 必须接收 `ariaLabel` 或 `ariaLabelledBy` 其一，placeholder 不充当名称
- 质量证据分层（L0 合同 / L1 浏览器 mock / L2 后端与 smoke / L3 真机）以 `docs/development/quality-matrix.json` 登记；未执行的真机项保持 `NOT VERIFIED`，不得用 L1 mock 宣称 GUI/系统权限/双机 LAN 已认证

## 4. 技术架构

### 4.1 技术栈
- 桌面宿主：Tauri 2（Rust 主进程）
- 语言：Rust（后端）+ TypeScript（前端）
- 网络：axum（HTTP 服务端，跨设备 P2P）+ reqwest（peer client）
- 发现：mdns-sd（mDNS）
- 存储：SQLite + sqlx
- 抓屏/剪贴板：xcap + arboard
- 终端：portable-pty（工作台交互式 PTY attach）+ tmux（优先承载可恢复 window/pane 上下文）
- 通信：Tauri `invoke()` IPC（本地前端 ↔ Rust）
- 打包/更新：Tauri CLI + tauri-plugin-updater

### 4.2 数据模型

#### Prompt
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| title | str | 标题 |
| content | str | 内容 |
| tags | list[str] | 标签列表 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| device_id | str | 创建设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| deleted | bool | 软删除标记 |

#### ScratchpadPage
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| title | str | 页面标题，空标题归一为“未命名” |
| content | str | 页面正文 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| device_id | str | 最后修改设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| deleted | bool | 软删除标记 |

#### ClaudeHistory
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str | 历史 Prompt 唯一 ID |
| project_path | str | Claude Code 会话所属项目路径 |
| content | str | 用户输入 Prompt 内容 |
| created_at | datetime | 历史记录时间 |
| updated_at | datetime | 最近同步更新时间 |
| device_id | str | 来源或最后修改设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| deleted | bool | 软删除标记 |

#### ClaudeMd
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str | 单例 ID，固定为 `claude_md` |
| content | str | user 级 `~/.claude/CLAUDE.md` 内容 |
| updated_at | datetime | 最近更新时间 |
| device_id | str | 最后修改设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |

#### Device
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 设备唯一 ID |
| name | str | 设备名称 |
| host | str | IP 地址 |
| port | int | HTTP 端口 |
| last_seen | datetime | 最后在线时间 |
| online | bool | 是否在线 |

#### SshTarget
| 字段 | 类型 | 说明 |
|------|------|------|
| host | str | 主键（IP/hostname） |
| port | int | 端口，默认 22 |
| username | str | 用户名（空串=用本机默认用户名） |
| label | str | 备注（可选） |
| device_id | str | 最后修改设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| deleted | bool | 软删除标记 |

#### WorkbenchProject
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| name | str | 项目显示名 |
| kind | str | 项目类型：local（本机/已挂载目录）或 remote（局域网远端快捷方式） |
| device_id | str | 所属设备 ID |
| device_name | str | 所属设备名称 |
| path | str | canonical 项目根路径 |
| last_opened_at | datetime | 最近打开时间 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

#### WorkbenchWorktree
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID / deterministic main id) | 主键 |
| project_id | str | 所属 WorkbenchProject |
| name | str | worktree 显示名，默认取分支名 |
| branch | str? | Git 分支名 |
| base_branch | str? | 创建 worktree 时的基准分支/引用 |
| path | str | worktree canonical 根路径 |
| is_main | bool | 是否为项目主工作区 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

#### WorkbenchSession
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| project_id | str | 所属 WorkbenchProject |
| worktree_id | str? | 所属 WorkbenchWorktree；旧记录可为空并视为主工作区 |
| name | str | 终端 tab 显示名 |
| command | str | 启动 shell 命令 |
| cwd | str | terminal window 绑定的 worktree 根路径 |
| status | str | running / exited / disconnected |
| cols | int | 最近一次 PTY 列数 |
| rows | int | 最近一次 PTY 行数 |
| started_at | datetime | 首次创建时间 |
| exited_at | datetime? | 最近断开或退出时间 |
| exit_code | int? | 子进程退出码 |
| backend | str | pty / tmux |
| backend_id | str? | tmux session 名称等后端标识；tmux 模式下由项目名、worktree 显示名和短 id 尾缀派生 |
| backend_window_id | str? | tmux window id |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

### 4.3 网络协议

#### mDNS 服务
- 类型：`_cc-partner._tcp.local.`
- TXT 记录：device_id, device_name

#### HTTP API
| 方法 | 路径 | 说明 |
|------|------|------|
| GET | /api/health | 健康检查 |
| GET | /api/mobile/access-info | 返回多网段 `entries`（及兼容 `urls`）供桌面弹层切换局域网链接/二维码（无身份鉴权） |
| POST | /api/sync/pull | 拉取 Prompt（含向量时钟摘要） |
| POST | /api/sync/push | 推送 Prompt |
| POST | /api/scratchpad/sync/pull | 拉取 Scratchpad 页面（含向量时钟摘要） |
| POST | /api/scratchpad/sync/push | 推送 Scratchpad 页面 |
| POST | /api/cc-history/sync/pull | 拉取 Claude Code 历史 Prompt（legacy；缺分页能力时使用） |
| POST | /api/cc-history/sync/push | 推送 Claude Code 历史 Prompt（legacy；缺分页能力时使用） |
| POST | /api/cc-history/sync/manifest-page | 分页拉取 CC 历史摘要（capability `cc-history.paged-sync.v1`） |
| POST | /api/cc-history/sync/items | 按 ID 批取 CC 历史正文（capability `cc-history.paged-sync.v1`） |
| POST | /api/cc-history/sync/push-batch | 分批事务推送 CC 历史（capability `cc-history.paged-sync.v1`） |
| POST | /api/ssh-target/sync/pull | 拉取 SSH 目标（含向量时钟摘要） |
| POST | /api/ssh-target/sync/push | 推送 SSH 目标 |
| POST | /api/sync/claude_md/pull | 拉取 user 级 CLAUDE.md（兼容协议） |
| POST | /api/sync/claude_md/push | 主动推送 user 级 CLAUDE.md |
| POST | /api/transfer/init | 发起文件传输 |
| POST | /api/transfer/chunk/{id} | 发送文件块 |
| POST | /api/transfer/complete/{id} | 显式 finalize 握手（空文件/满 tmp 续传） |
| GET | /api/transfer/status/{id} | 查询传输状态 |
| GET | /api/claude-code/assets/inventory | N/N+1 旧 Claude Code assets inventory facade（权威管理已迁 Agent Hub portable inventory；前端已删除） |
| POST | /api/claude-code/assets/bundle | N/N+1 旧 Claude Code assets bundle facade（same-agent Pull 走 portable pull 命令；前端已删除） |
| GET/POST | /api/workbench/fs/* | Workbench 远端目录根、目录列表和路径信息 |
| GET/POST | /api/workbench/projects/* | Workbench P2P 远端项目打开与对端 local 项目访问 |
| POST | /api/workbench/worktrees/* | Workbench worktree 列表、创建、查询、commit、push、merge、remove |
| POST | /api/workbench/git/commits | 查询 active worktree 的 Git 提交 DAG |
| POST | /api/workbench/files/* | Workbench 文件树、文件打开/保存、预览、创建、重命名和删除 |
| GET | /api/workbench/events | Workbench HTTP NDJSON 事件流（移动端与远端事件桥） |
| POST | /api/workbench/sessions/* | Workbench terminal window/pane 列表、创建、replay、输入、resize、focus、分屏、关闭和重命名 |
| POST | /api/workbench/prompt-optimizer/stream-to-session | P2P Workbench 远端网关 Prompt 优化并流式写入对端 local 终端 |
| GET/POST | /api/mobile/workbench/* | Mobile Workbench 本机入口；可继续代理 remote shortcut 的 worktree、terminal、files、Git 和 Prompt 操作 |


## 事务化配置与运行时

- 配置写入经统一事务路径原子落盘；失败不改变内存与旧文件
- Cloud Sync 同一工作区单飞：手动/CLAUDE.md 推送可等待最多 5 分钟，自动同步忙时跳过
- 截图快捷键热更新：先注册新值再注销旧值；配置保存失败则恢复旧快捷键，无法恢复时提示重启
- 应用内更新：检查/下载/安装使用 generation 状态机；安装失败可保留已下载包重试
- 健康提醒：数值与免打扰格式由后端强制校验；非法输入不改变已保存配置与运行时状态
- 本能力不涉及 SQLite schema 变更


## 前端基础、无障碍与性能合同（S4）

**描述**：桌面与 mobile 前端共享可自动验证的 design token、模态、键盘、错误隔离、拆包与巨型页所有权边界。

**功能点 / 约束**：
- 颜色/间距/圆角/阴影仅使用 `tokens.css` 已定义 token；`prefers-reduced-motion: reduce` 下全局归零 animation/transition duration
- 普通小字/次要信息文本使用 `--fg-muted-readable`（浅/深均与 `--bg`/`--surface` 对比度 ≥4.5:1）；`--meta` 仅用于 placeholder/disabled/装饰，且 `color: var(--meta)` 必须落在 checker 评审 allowlist；`check:css-tokens` 自动断言正文语义色对
- 产品模态统一 `Dialog`/`Drawer` 原语：portal、focus trap、Escape、背景 inert、scroll lock、关闭焦点恢复；业务页禁止手写 focus trap
- 路由级 `RouteErrorBoundary` + lazy routes：单页 render 失败不得拖垮 AppShell；DEV 提供 `/__cp_route_error_fixture` 崩溃恢复夹具供 E2E
- desktop main initial ≤ 320 KiB gzip；mobile initial ≤ 280 KiB gzip；mobile initial 禁止 xterm/Tiptap/CodeMirror/Recharts；生产默认无 sourcemap
- Settings / 桌面 Orchestrator / Mobile Automation 按 controller + pure views 拆分：views 无 API/transport，controllers 无 board/modal JSX
- Workbench 仅七个域 controller，页面组合层 ≤1200 行；终端 window tabs 使用 roving tablist（Arrow/Home/End）
- Attention 列表每行仅一个 tab stop（单 button，动作文案为 span）
- 侧栏按 Explore/Work/Knowledge/Connect/System 分组；`/` 仍是 GitHub Trending；Workbench 在 Work 组；短窗口侧栏 content 可滚、footer 不覆盖
- GUI 首次启动 LAN listener 前必须完成风险披露确认（首选 TCP 62116 / 端口递增 / mDNS UDP 5353 / 无身份校验）；确认写入 `gui-bootstrap.json`，不是可切换 LAN 模式
- Workbench 无项目时仅聚焦空态 CTA（添加本机/连接远端/检查 tmux）；有项目未选中时展示“继续工作”启动页；Mobile 导航双模式：global=Projects/Inbox/Tools/System，project=Workbench 工具 + Shortcuts（Prompt 全局，项目绑定入口仅 project 模式）
- 侧栏 `WorkbenchProjectRail` 为项目导航权威入口；添加来源/远端选择走共享 Dialog
- 固定布局回归 viewport：1024×768、1280×720、390×844、844×390；断言无横向溢出、关键入口键盘可达，命名截图供人工评审（本轮不宣称像素 baseline）
- 验证：`npm run check:css-tokens && npm run check:i18n && npm run lint && npm run build && npm run check:bundle && npm test && npm run test:e2e`；E2E foundation 冒烟 `frontend-foundation.spec.ts`；手动 VoiceOver/NVDA 覆盖 Dialog/Drawer/Attention/终端 tabs
- Orchestrator Agent Adapter：WORKFLOW `runner.provider` 支持 `claudeCodeVisible|codexVisible|genericTerminal`；attempt 创建时冻结 max_turns/stall_timeout；owner probe catalog 不泄露 path/env；旧 peer 降级前必须 quiesce 非 Claude Runner。
