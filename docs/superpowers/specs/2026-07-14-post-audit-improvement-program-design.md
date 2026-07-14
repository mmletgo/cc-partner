# cc-partner 审计后全面改进计划总纲

- 日期：2026-07-14
- 状态：已确认；用户授权直接生成分域 spec 与对应 implementation plan
- 基线提交：`bb980fd`
- 文档类型：总纲、依赖与覆盖矩阵；不承载单个实现 PR 的函数级步骤

## 1. 背景

2026-07-14 全仓审计确认，cc-partner 已具备成熟的自动质量底座：前端 lint/build、740 个 Vitest、31 个 Playwright E2E、token/i18n/module/bundle 门禁、Rust fmt/clippy/完整测试、115 条 P2P 路由盘点、质量矩阵与文档事实检查均通过。2026-07-13 的 S1–S6 计划已经进入主线，本轮不得把旧计划未勾选的 checkbox 误判为未完成工作。

本轮只处理旧计划落地后的残余正确性问题和新增产品闭环：GUI 与 sidecar 运行时权威分裂、Prompt/SSH/Scratchpad 同步真值、前端异步竞态、移动端弱网、核心信息架构、LAN 首次风险确认、传输恢复、Human Review diff、WORKFLOW 向导、系统通知、导出恢复、针对性性能治理与真实设备认证。

## 2. 历史基线与排除表

| 历史计划 | 合并提交 | 本轮复用且不重做的能力 |
| --- | --- | --- |
| S1 LAN boundary | `9b5b698` | socket/Host/Origin/WebSocket/resource/stop 边界、固定风险文案、无鉴权 LAN 业务模型 |
| S2 Core integrity | `f433b67` | Transfer 基础 send/cancel、Scratchpad 关闭 flush、Prompt 基础回滚、partial loader、visibility polling |
| S3 Transactional runtime | `284bcb9` | 原子配置文件、进程内 Cloud Sync singleflight、Updater generation、Health 校验 |
| S4 Frontend foundation | `db07451` | token、Dialog/Drawer、RouteErrorBoundary、lazy route、bundle gate、terminal buffer、基础 IA |
| S5 Scale/observability | `3257f39` | Orchestrator claim 收敛、CC History 分页/批量、SQLite pool=1 基准与指标 |
| S6 Quality/governance | `39497a6` | runtime schema、E2E/L2、故障注入、模块 ratchet、质量矩阵基础设施 |

`2026-07-13-whole-program-improvement-roadmap*` 两份未跟踪文档仅作为历史输入，不覆盖、不删除，也不作为新一轮完成状态来源。

## 3. 全局产品与技术决策

1. sidecar 是运行中配置、Cloud Sync、Workbench terminal、Orchestrator scheduler/telemetry 与 LAN listener 的唯一权威 owner；GUI 是控制面和展示面。
2. 固定 LAN 边界不变：合法 loopback/LAN peer 无账号、配对、token、cookie、session、签名或设备身份即可读、写和执行。一次性风险确认不是可切换 LAN 模式，也不引入路由权限矩阵。
3. 所有异步写路径必须区分“请求成功”“结果未知”“明确失败”；失败不得伪装成功，旧响应不得覆盖更新草稿或新上下文。
4. 新产品功能必须复用现有 Attention、Dialog/Drawer、Workbench controller、P2P error envelope、request ID、runtime schema 和设计 token，不引入第二套状态模型。
5. 数据库变化必须遵循现有 runtime 幂等 schema upgrade/repo helper，提供混合版本行为和回滚说明，并同步 `migrations/0001_init.sql` 这个 schema 文档；恢复操作必须先预览、再确认、再事务提交。
6. 不扩大为账号系统、云中继、互联网暴露、公网穿透、PR 创建或交互式 Git 冲突解决。
7. 所有新用户可见行为在实现时同步 `docs/prd.md`；运维、协议、测试与分层约定同步最相关权威文档。

## 4. 子项目边界

| 编号 | 子项目 | 权威结果 | Spec | Plan |
| --- | --- | --- | --- | --- |
| N1 | Runtime Authority & Operational Diagnostics | sidecar 单一 owner、generation、terminal runtime role/RAII、权威 snapshot、bridge 生命周期、运行诊断 | `2026-07-14-runtime-authority-and-operational-diagnostics-design.md` | `2026-07-14-runtime-authority-and-operational-diagnostics.md` |
| N2 | Sync Integrity, Conflict & Recovery | typed sync、manifest/batch、事务、冲突副本、tombstone GC、导出恢复 | `2026-07-14-sync-integrity-conflict-and-recovery-design.md` | `2026-07-14-sync-integrity-conflict-and-recovery.md` |
| N3 | Frontend Async State & Mobile Transport | safe-save、stale guard、错误恢复、operation context、移动端 timeout/cancel/reconcile | `2026-07-14-frontend-async-state-and-mobile-transport-design.md` | `2026-07-14-frontend-async-state-and-mobile-transport.md` |
| N4 | Core Workbench Experience & LAN Onboarding | Trending 默认首页、Workbench Continue Working 启动页、导航分组、空态、LAN 首次确认、移动布局与对比度 | `2026-07-14-core-workbench-experience-and-lan-onboarding-design.md` | `2026-07-14-core-workbench-experience-and-lan-onboarding.md` |
| N5 | Transfer Lifecycle & Recovery | retry/resume、失败阶段、结果对账、open/reveal、可操作历史 | `2026-07-14-transfer-lifecycle-and-recovery-design.md` | `2026-07-14-transfer-lifecycle-and-recovery.md` |
| N6 | Orchestrator Review, Workflow & Notifications | Human Review diff、WORKFLOW 向导、系统通知、deep link 与去重 | `2026-07-14-orchestrator-review-workflow-and-notifications-design.md` | `2026-07-14-orchestrator-review-workflow-and-notifications.md` |
| N7 | Targeted Performance & Maintainability | 1Hz 隔离、编辑器语言拆包、索引预算、timeout 分类、模块 ratchet | `2026-07-14-targeted-performance-and-maintainability-design.md` | `2026-07-14-targeted-performance-and-maintainability.md` |
| N8 | Real-Device Release Certification | 当前 Apple Silicon Mac GUI/权限与 VoiceOver 真机证据、90 天有效期、`macos-aarch64-beta` go/no-go；其他平台延期 | `2026-07-14-real-device-release-certification-design.md` | `2026-07-14-real-device-release-certification.md` |

## 5. 依赖波次

```text
Wave 0: N1
Wave 1: N2 | N3
Wave 2: N4 | N5
Wave 3: N6
Wave 4: N7
Wave 5: N8
```

- N1 先固定 owner、generation、控制面和生命周期，N4 的 listener 首次确认才能有可靠启动语义。
- N2 与 N3 可并行：N2 负责后端数据真值，N3 负责前端请求和草稿真值。
- N5 消费 N3 的 mutation 状态合同，可与 N4 并行；N4 先拥有 AppShell、Settings 响应式、deep-link/导航整合，合并后 N6 再增量接入 review/workflow/notification，避免并行修改同一 UI shell/config 文件。
- N6 同时消费 N1 的权威 runtime snapshot 与 N3 safe-save 合同。
- N7 只优化已经稳定的行为，不在性能任务中修改产品语义。
- N8 必须针对 N1–N7 合并后的候选版本执行，不能用 L1/L2 替代真机证据；当前只执行本机 `macos-aarch64-beta`，其他平台保持 `NOT VERIFIED` 且不阻断该 beta。

## 6. 审计发现覆盖矩阵

| 审计发现或新增能力 | 唯一 owner | 完成判断 |
| --- | --- | --- |
| GUI/sidecar 两份 AppState 与配置不一致 | N1 | 设置保存后返回 owner instance + generation，sidecar 实际行为立即一致 |
| Cloud Sync 跨进程重复执行 | N1 | GUI 手动与 sidecar 自动入口共享唯一 owner/singleflight |
| terminal registry 与 Orchestrator telemetry 分裂 | N1 | runtime role/RAII session claim 与 runtime snapshot 均由 owner 提供，GUI 不读空本地 runtime |
| remote event bridge 永久轮询、NDJSON 无上限 | N1 | bridge 可取消/回收/关机；行、pending buffer、错误正文均有限制 |
| Prompt/SSH/Scratchpad 网络错误被当成空远端 | N2 | typed error 不触发全量 push，partial failure 不计成功 |
| bulk write 非事务、tombstone 无限增长 | N2 | 单批事务；ack/watermark 驱动安全 GC |
| LWW 静默覆盖、缺少导出恢复 | N2 | 冲突副本/有限历史可见；恢复有校验、预览和事务回滚 |
| Settings/ClaudeMd 旧保存响应覆盖新输入 | N3 | 保存期间继续输入不会被响应回填覆盖 |
| Scratchpad 快速切页、Devices/CcHistory 失败恢复 | N3 | 逆序响应被丢弃；失败保留草稿或回滚后可重试；无后端 restore 合同时不展示假 Undo |
| Mobile project 失败无法重试、HTTP 无统一 timeout | N3 | error 状态有重试；query 可取消/重试；mutation 可对账且不盲重放 |
| Git 长操作污染新 project/worktree | N3 | success/catch/finally 全部校验 operation context |
| Trending 默认首页需要保持，Workbench 仍缺“继续工作”入口 | N4 | `/` 保持 Trending；`/workbench` 有项目未选中时展示最近工作摘要，完全无项目只显示聚焦 CTA |
| 侧栏短窗口溢出、Workbench 空态噪声 | N4 | 内容区独立滚动；无项目时只展示聚焦 CTA 与依赖次级动作 |
| LAN 无身份模型缺少首次知情确认 | N4 | GUI 首次启动 listener 前确认本机地址候选、首选 port/递增规则与风险，启动后展示实际监听地址；确认后仍是固定无鉴权模型 |
| `--meta` 对比度与移动十面板认知负担 | N4 | 正文达到 4.5:1；移动导航分组并完成横屏/键盘/safe-area 设计合同 |
| Transfer 只有 send/cancel，恢复操作不完整 | N5 | retry/resume/open/reveal 与失败阶段均有真实后端合同和 UI |
| Transfer 超时后结果未知 | N5 | 稳定 client operation ID 对账；trace request ID 可变化，不重复创建或破坏 durable finalize |
| Human Review 无 diff、WORKFLOW 无编辑入口 | N6 | bounded diff/stat 与 Evidence 同屏；向导可创建/打开/校验并定位错误 |
| 缺少关键状态系统通知 | N6 | Human Review、Blocked、outbox failed、done 可配置通知；系统通知只提醒，Attention/应用内 badge 提供 deep link，不从通知执行动作 |
| Workbench 每秒整页重渲染 | N7 | 时钟下沉，Profiler/测试证明只更新运行时文本子树 |
| CodeMirror 全语言静态导入 | N7 | 按语言动态加载缓存，bundle budget 不回退 |
| Claude session 同步扫描与巨型模块 | N7 | `spawn_blocking` + 扫描预算；module exceptions 按期限收敛 |
| 当前只有 Apple Silicon Mac 可执行 L3 | N8 | 只消费 GUI/权限与 VoiceOver 的 `macos-aarch64` execution；Windows、Ubuntu、Intel Mac、dual-host、mobile、NVDA 保持 `NOT VERIFIED`，仅阻断对应未来宣称 |

## 7. 全局非目标

- 不重新实现 S1–S6 已完成的基础设施。
- 不引入 Redux、Zustand、GraphQL、第三方 modal、遥测 SaaS 或新的设计系统。
- 不把一次性 LAN 风险确认设计成可切换 LAN 模式、权限 capability 或“认证设备”。
- 不为 Human Review 实现 PR 创建、交互式冲突解决或完整 IDE diff 编辑器。
- 不在导出包中包含项目源码、终端 transcript、认证 token、SSH 私钥或 lifecycle control token。
- 不为了拆文件而拆文件；只拆当前计划触达且有 characterization 的模块。
- 不自动修改防火墙，不把未执行真机项标为通过。
- 不让延期的 Windows/Ubuntu 等平台阻断固定 `macos-aarch64-beta`，也不把该 beta 扩写成 stable/full/cross-platform 认证。

## 8. 全局完成合同

1. N1–N7 每条计划均有独立 focused tests、集成门禁、回滚说明和持久文档更新。
2. 前端继续满足 `lint + build + test + test:e2e + token/i18n/module/bundle` 全部门禁。
3. Rust 继续满足 `fmt + clippy -D warnings + cargo test --locked`，P2P 路由与 docs 检查保持一致。
4. 任何失败、partial 或 uncertain 状态都能在 DTO、日志和 UI 中被区分，且不泄漏用户内容或凭据。
5. N8 对候选版本执行本机 Apple Silicon GUI/权限与 VoiceOver 认证；任一当前必需 execution 失败则不得发布该 beta，延期项保持 `NOT VERIFIED`，不得宣称全平台、双机 LAN 或 1GiB resume 已认证。
6. 最终仅按事实更新 README、PRD、测试矩阵和分层指令；旧未跟踪路线图保持用户所有权。

## 9. Spec 自审

- 未决占位项：无；N4 固定 Trending 默认首页，N8 固定 `macos-aarch64-beta` 当前执行范围。
- 一致性：八条子项目各有唯一 owner，LAN 固定无鉴权边界在所有子项目中一致。
- 范围：总纲只定义依赖和验收，不复制子计划实现步骤。
- 歧义：旧 S1–S6 明确为已完成基线，旧 checkbox 不作为待办来源。
