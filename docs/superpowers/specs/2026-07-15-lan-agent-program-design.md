# cc-partner LAN Agent Program 设计总纲

- 日期：2026-07-15
- 状态：已批准
- 文档类型：总纲、依赖、边界与覆盖矩阵
- 对应计划：`docs/superpowers/plans/2026-07-15-lan-agent-program.md`

## 1. 产品原则

本计划将 cc-partner 从“能在局域网打开远端工作台”推进为“自动理解并协调局域网 Agent 工作现场”，但不改变产品的固定 LAN 哲学。

唯一默认原则是：

> 自动感知、自动执行、自动收敛；只有歧义、失败或必须由人决定时才打扰用户。

由此派生五条强约束：

1. Agent 状态必须自动发现和恢复，不要求用户手工维护状态。
2. 候选实验必须自动验证和选优，不要求用户逐份阅读 diff。
3. Browser Verification 必须自动发现已有 loopback preview 并生成 evidence，不要求用户先圈选元素。
4. Fleet 与 Ledger 必须由现有运行态自动聚合，不要求用户建立第二套设备或会话台账。
5. CLI 面向 Agent 和自动化调用者，不成为新的必经图形操作流程。

### 1.1 参考项目与本地取舍

本计划只参考公开产品机制，不复制对方代码或数据模型；所有落地均服从 cc-partner 的 owning-device、固定 LAN 无鉴权边界与“减轻用户负担”原则。

| 参考项目 | 公开机制 | cc-partner 吸收 | 明确不照搬 |
|---|---|---|---|
| [dark-hxx/CLI-Manager](https://github.com/dark-hxx/CLI-Manager) | Hook/OSC 会话绑定、terminal tab 状态、Claude/Codex、多维 usage、分屏、历史 Diff、命令面板/模板与供应商切换 | A1 runtime、A2低噪音投影、A3 provider adapter、A9可靠 metadata Ledger | 不做用户 Diff 审查；不做全局命令面板/模板；不按静态价格估算 cost；不跨设备同步 provider credential |
| [stablyai/orca](https://github.com/stablyai/orca) | 多种 CLI Agent、隔离 worktree、同题多 Agent 候选、移动端监控与 winner workflow | A3多 provider、A4同 owning-device candidate experiment、A2 Mobile projection、A6 Fleet | 不要求用户比较/批注 diff；不自动把候选派往其他设备；不引入账号/订阅切换或云控制面 |
| [manaflow-ai/cmux](https://github.com/manaflow-ai/cmux) | OSC/Hook 通知、workspace 状态提示、可编程 CLI/socket、browser snapshot/ref/click/fill、重启恢复 | A1 OSC runtime、A2 Attention/通知、A5受限 Browser Verification、A7 typed CLI、A8 safe restore | 不开放 arbitrary JavaScript eval、任意 URL/keystroke socket、cookie/profile 导入、全局 unread jump或可执行 workspace recipe |

由此形成的关键差异是：参考项目多把“更多可控原语”直接交给用户；cc-partner 只保留能自动收敛、且不会扩大 LAN 权限或日常维护负担的原语。

## 2. 明确排除

以下能力不进入任何子项目，也不得以相近名称重新引入：

- P0-2：面向用户的 Diff → 批注 → Request Rework 审查闭环。
- P1-4：全局 Quick Open、Command Recipe、命令模板或可执行布局配方。
- PR 创建、交互式冲突解决、完整 IDE diff 编辑器。
- 自动跨设备迁移任务、自动复制仓库或自动把任务派到“最空闲设备”。
- 账号、配对、设备 token、LAN capability token、路由授权矩阵或可切换 LAN 模式。
- 公网中继、云控制面、Tailscale 等作为主路径。
- 第二个常驻 daemon、默认绕过 sandbox/approval、同步 Agent 凭据或浏览器 cookie/profile。

Orchestrator verifier 内部继续允许读取有界 diff 作为机器验证输入；该内部事实不得产生用户 patch DTO、Review Diff route、Changes UI 或人工 digest 确认门。

## 3. 子项目与唯一职责

| 编号 | 子项目 | 唯一职责 | Spec | Plan |
|---|---|---|---|---|
| A0 | LAN Agent Program | 统一原则、依赖、兼容、文档废止与完成合同 | 本文 | `2026-07-15-lan-agent-program.md` |
| A1 | Agent Session Runtime | 统一 Agent 会话身份、生命周期、快照与事件 | `2026-07-15-agent-session-runtime-design.md` | `2026-07-15-agent-session-runtime.md` |
| A2 | Agent State Projection | Desktop/Mobile/Attention/通知的低噪音状态投影 | `2026-07-15-agent-state-projection-design.md` | `2026-07-15-agent-state-projection.md` |
| A3 | Agent Adapter Platform | Claude、Codex、generic terminal 的启动/恢复/完成合同 | `2026-07-15-agent-adapter-platform-design.md` | `2026-07-15-agent-adapter-platform.md` |
| A4 | Automated Candidate Experiments | 同题多候选、验证、自动选优和唯一 winner 交付 | `2026-07-15-automated-candidate-experiments-design.md` | `2026-07-15-automated-candidate-experiments.md` |
| A5 | Browser Verification Surface | owner 侧按需浏览器自动化与 evidence | `2026-07-15-browser-verification-surface-design.md` | `2026-07-15-browser-verification-surface.md` |
| A6 | LAN Agent Fleet | 保存 shortcut 范围内的 device/project/Agent 聚合视图 | `2026-07-15-lan-agent-fleet-design.md` | `2026-07-15-lan-agent-fleet.md` |
| A7 | Agent-first CLI | 稳定 ID、JSON 和显式 device selector 的 Agent 接口 | `2026-07-15-agent-first-cli-design.md` | `2026-07-15-agent-first-cli.md` |
| A8 | Workspace Safe Restore | 零配置保存与安全恢复 UI 工作现场 | `2026-07-15-workspace-safe-restore-design.md` | `2026-07-15-workspace-safe-restore.md` |
| A9 | Agent Metadata Ledger | metadata-only Agent 历史、保留与聚合 | `2026-07-15-agent-metadata-ledger-design.md` | `2026-07-15-agent-metadata-ledger.md` |

## 4. 依赖波次

```text
Wave 0: A0
Wave 1: A1 | A5 | A8
Wave 2: A2 | A3
Wave 3: A4 | A6 | A9 backend；A9 Fleet join在A6 UI/collector后
Wave 4: A7
Wave 5: 跨项目验证与真实设备认证
```

依赖关系：

- A2 依赖 A1 的 typed runtime snapshot/event。
- A3 依赖 A1 的统一会话身份；A1 不依赖任一具体 provider。
- A4 依赖 A1、A2投影合同、A3 与 A5；candidate 继续复用现有 task/worktree/attempt/evidence，实际experiment Attention source由A4注册。
- A6 依赖 A1/A2；A9 的repo/writer/retention可与A6并行，但remote summary与Fleet UI join必须在A6 collector/view稳定后接入，且不阻塞 Fleet 首版。
- A9 依赖 A1/A3 的可靠 metadata，unknown 必须保持 `null`。
- A7 最后暴露已经稳定的领域合同，不为 CLI 另建业务实现。
- A5 与 A8 可以和 A1 并行，因为二者分别复用现有 browser preview 与持久 session。

## 5. 共享权威模型

### 5.1 Owning device

- project、worktree、terminal、Agent runtime、experiment、browser runtime、task evidence 的唯一权威始终位于 owning device。
- remote shortcut 只保存 `{deviceId,path}` 指针并映射 DTO；不得成为第二份调度或状态真值。
- current/control device 只缓存 display-only snapshot，所有 mutation 必须转发 owner。
- Fleet 只汇总用户已经保存的 local project 与 remote shortcut，不枚举或接管对端全部项目。

### 5.2 事件与重连

- 复用现有 sidecar `RuntimeEventBus` 的 `{ownerInstanceId,sequence}`、replay、Gap 与 owner restart 语义。
- 每个新增实时领域必须同时提供 bounded snapshot；Gap 后从 snapshot 建立 baseline，再消费 cursor 之后的 event。
- 旧 decoder 在协议扩展前必须先改为安全忽略未知 event，禁止未知 variant 导致重连循环。

### 5.3 Capability

- capability 只声明协议版本和功能存在性，不表达权限、认证或设备信任。
- 旧 peer 缺少 capability 时返回明确 `unsupported`；不得静默回退成 Claude、自动本机执行或更改目标设备。

## 6. 隐私与内容边界

以下内容不得进入 P2P event、Fleet DTO、Ledger、系统通知、日志或现有 Cloud Sync：

- Prompt、assistant 回复、完整 terminal bytes；
- transcript path、cwd 绝对路径的非必要部分、env 值；
- API key、token、cookie、browser profile/history；
- control descriptor/token、Agent provider credential；
- 用户源码或浏览器页面正文，除非它是用户明确触发的本机 evidence，且受单项大小和保留策略约束。

允许的最小 metadata 包括稳定功能 ID、provider/model（可靠时）、phase、时间戳、duration、outcome、结构化 token/cost、错误 code 和有限通用状态文案。

## 7. 自动收敛合同

### 7.1 正常路径

- Agent adapter 自动 probe，可用时自动关联 native session。
- session phase 自动进入 Desktop/Mobile/Fleet；用户不维护状态。
- experiment 通过硬门禁与 comparative verifier 产生唯一 winner。
- full-auto 且置信为 `high` 时只交付 winner；loser 永不 commit/push/merge。
- Browser Verification 自动采集 load/console/assertion/screenshot evidence。

### 7.2 例外路径

只在以下条件产生 Attention 或一次性决策入口：

- Agent 明确 `needsInput`；
- Agent/task/verification 失败且自动修复预算用尽；
- experiment 没有合格 candidate、出现并列或 comparative confidence 非 `high`；
- owner offline/unsupported 导致用户请求无法继续；
- Workspace restore 需要创建新 shell 或执行有副作用操作——首版默认不执行，只报告缺失项。

Attention 始终只导航到权威现场，不在列表中批准、输入或运行命令。

## 8. 数据库、混合版本与回滚

- 所有表和列采用 additive migration，并同步 `src-tauri/migrations/0001_init.sql` schema 文档。
- 新版本读取旧数据时使用明确默认值；旧版本无法安全解释新活动状态时必须先 quiesce，再降级。
- Agent runtime 与 Orchestrator 旧 Claude 字段允许一个版本 dual-write；完成迁移后再删除旧读路径。
- experiment 降级前必须停止创建、完成或取消活动组，并将 loser child 置终止态，避免旧版本逐个交付。
- Browser runtime、event cache、Workspace restore preflight 都必须可丢弃重建；不得将瞬时状态伪装为持久真值。
- 回滚不 drop 新表；新二进制关闭功能后，旧普通 task/session/browser preview 路径必须继续工作。

## 9. 旧规格处置

`2026-07-14-orchestrator-review-workflow-and-notifications-*` 作为一个串行执行单元被本计划 supersede：

- 明确取消其 Review Diff、review digest、Changes UI、mobile diff、Diff→Rework E2E 与 Deliver 人工确认门。
- 通知 event/snapshot/dedupe/privacy 合同迁移到 A2，并扩展为 Agent/experiment 异常投影。
- `WORKFLOW.md` 向导不与本次排除直接冲突，但不属于 A0–A9 范围；保留历史设计，不在本轮实施。
- 上层 `2026-07-14-post-audit-improvement-program-*` 的 N6 不能再作为未修改的执行入口。

## 10. 全局验证与完成合同

每个子项目必须同时满足：

1. focused unit/integration tests 覆盖状态机、owner 映射、mixed-version、资源上限与失败语义。
2. 前端触达时运行 `cd web && npm run lint && npm run build && npm test`，关键旅程补 Playwright。
3. Rust 触达时运行 `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`。
4. P2P 触达时运行 route inventory、docs check 与 capability/unsupported 测试。
5. Windows/Ubuntu/macOS 的 managed browser、CLI 打包、tmux/PTY 行为只有真实执行后才能标记通过；未执行项保持 `NOT VERIFIED`。
6. 没有用户 Review Diff、全局 Quick Open、Command Recipe、自动跨设备调度或 LAN 鉴权模型回流。

## 11. 总纲自审

- A0–A9 每个产品点有唯一 owner spec 和对应 plan。
- 自动路径与例外路径分离，正常完成不要求人工审阅 diff。
- LAN、隐私、owner、event Gap、mixed-version 与 rollback 是所有子项目的共享前置。
- 旧 N6 仅取消冲突轨道，未把 WORKFLOW 向导误称为 P1-4。
