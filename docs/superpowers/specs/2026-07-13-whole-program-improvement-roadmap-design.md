# cc-partner 全面改进计划总纲

- 日期：2026-07-13
- 状态：方案已确认，按用户授权直接生成分域 spec 与 implementation plan
- 文档类型：改进计划总纲与覆盖追踪；不直接承载单个 PR 的实现细节

## 1. 背景

2026-07-13 全仓只读审计确认：cc-partner 已具备严格 TypeScript、完整 Rust 类型、前端 403 个单元测试、后端 970 个单元测试、12 个浏览器 E2E、P2P 路由清单、跨平台 smoke、Doctor 与受控日志等良好工程基础。2026-07-11 工程改进计划中的 Vitest、Workbench controller、P2P 协议元数据、远端 runtime snapshot、跨平台 smoke、日志/Doctor 与文档校准也已基本落地。

本轮不重复上一轮治理，而是处理审计仍确认存在的产品闭环、数据一致性、局域网暴露边界、交互、性能与规模问题。其中既包含缺陷修复，也包含 LAN peer 范围、浏览器跨站防护、通用 Dialog/Drawer、运行时 DTO 校验、性能预算等新能力。

## 2. 已确认决策

1. 使用一份总纲和六个可独立实施、验证、回滚的子项目；不生成一个跨全仓的巨型实现 PR。
2. 局域网访问继续采用“同网段默认可信”：P2P、Workbench 与 `/mobile` 业务 API 不增加配对、签名、Bearer token、cookie session 或逐设备身份鉴权。
3. 合法 LAN peer 的全部业务读取、写入与执行请求均无鉴权直接允许；socket peer、Host/Origin/WebSocket Origin、资源预算与 backend stop 生命周期控制只约束部署和浏览器滥用，不构成用户身份或业务权限。
4. 产品正确性优先于结构重构：先修 Transfer、Scratchpad、Prompts 等真实用户数据和核心旅程，再做拆包、组件治理与规模优化。
5. 异步数据统一采用 context key、stale guard、single-flight、可见性暂停和事件优先；不引入 Redux/Zustand。
6. 配置与更新状态采用显式事务/代际模型；失败不得留下“内存成功、磁盘失败”或旧包/新 metadata 混用状态。
7. 设计 token、关键 DTO、路由清单和性能预算进入自动化门禁，不再依赖人工约定发现漂移。
8. 数据库协议变化必须提供迁移、混合版本兼容和回滚；非数据库前端重构不维持无期限旧行为。
9. 每个子项目完成后保持主分支可运行；不得把半成品依赖留给下一波补齐。

## 3. 子项目边界

| 编号 | 子项目 | 主要结果 | 对应 spec | 对应 plan |
| --- | --- | --- | --- | --- |
| S1 | LAN 信任边界与暴露控制 | 合法 LAN 全业务直通、peer/bind/mDNS、浏览器跨站防护、资源边界、stop 隔离与 Doctor 风险提示 | `2026-07-13-lan-trust-boundary-hardening-design.md` | `2026-07-13-lan-trust-boundary-hardening.md`（修订为 6 个任务） |
| S2 | 核心产品完整性 | Transfer 真发送、Scratchpad 不丢写、Prompt 回滚、stale guard、局部错误与统一轮询 | `2026-07-13-core-product-integrity-design.md` | `2026-07-13-core-product-integrity.md` |
| S3 | 后端事务运行时 | Cloud Sync 单飞、原子配置、快捷键回滚、Updater generation、Health 校验 | `2026-07-13-backend-transactional-runtime-design.md` | `2026-07-13-backend-transactional-runtime.md` |
| S4 | 前端基础、UX 与性能 | token 合同、Dialog/Drawer、键盘/动效、懒加载、bundle budget、terminal buffer、IA | `2026-07-13-frontend-foundation-ux-performance-design.md` | `2026-07-13-frontend-foundation-ux-performance.md` |
| S5 | 后端规模与可观测性 | 缩短 SQLite 事务、CC History 分页批次、协议上限、性能指标与压测 | `2026-07-13-backend-scale-observability-design.md` | `2026-07-13-backend-scale-observability.md` |
| S6 | 质量与架构治理 | 核心 E2E、真机矩阵、DTO runtime schema、故障注入、模块边界和覆盖追踪 | `2026-07-13-quality-architecture-governance-design.md` | `2026-07-13-quality-architecture-governance.md` |

## 4. 推荐实施波次

```text
Wave 0 · 三条独立基础线
S1 LAN peer/browser/stop/resource/risk/cross-platform 边界
S2 Transfer/Scratchpad/Prompts/CcHistory/Settings/Permissions
S3 Cloud Sync 单飞、原子配置、快捷键、Updater、Health 校验

Wave 1 · 体验与规模
S4 token、Dialog/Drawer、键盘语义、reduced motion、route/editor split、terminal buffer；消费 S2/S3 已稳定的 Settings 与运行时 DTO 行为
S5 Orchestrator claim、CC History 分页/批次、性能指标与大数据压测；消费 S1 已稳定的 route/browser/resource 边界

Wave 2 · 质量收口
S6 统一 E2E/DTO schema/质量矩阵、剩余模块治理与跨平台真机验收
```

跨计划依赖固定为 `(S1 | S2 | S3) → (S4 | S5) → S6`。S1、S2、S3 可并行；S4 消费 S2/S3，S5 消费 S1，S6 只在前述行为稳定后统一收口。任何后续实现都不得把 socket/browser 防护扩张为身份鉴权或业务权限。

## 5. 审计发现覆盖矩阵

| 审计发现 | 权威子项目 | 交付判断 |
| --- | --- | --- |
| LAN Workbench/Mobile/P2P 采用局域网可信、无鉴权模型 | S1 | 合法 LAN peer 可直接读取、写入与执行全部业务能力；非法 scope、浏览器跨站请求和远端 stop 在各自边界拒绝 |
| Transfer 发送只 `console.info` | S2 | 真实路径进入 `send_transfer`，成功产生任务，失败可重试 |
| Transfer pause/retry/open 可点但无动作 | S2 | 实现前不呈现可点击假动作；实现后有独立契约测试 |
| `send_transfer` 前后端 DTO 不一致 | S2、S6 | typed DTO 与 runtime parser 同时通过 |
| Scratchpad debounce 卸载丢最后输入 | S2 | type→route leave/window close 不丢失 pending 文本 |
| Prompt CRUD 乐观失败不回滚 | S2 | create/update/delete reject 恢复权威 UI 并可重试 |
| CcHistory 旧响应覆盖新选择 | S2 | 逆序 resolve 测试锁定 stale response 丢弃 |
| Settings 11-way `Promise.all` 单点失败 | S2 | 非核心 tab 局部错误不阻塞其它 tab |
| Permissions 失败永久 checking、Welcome 批量请求 | S2 | error/retry/skip 与逐项授权可用 |
| 多处裸轮询后台运行、可重入 | S2 | 统一 visibility-aware single-flight，事件优先 |
| Cloud Sync 共享 Git 工作区无单飞 | S3 | 三入口并发时只有一个完整临界区执行 |
| 配置直接覆盖写且内存不回滚 | S3 | 原子落盘成功后才替换内存，故障可恢复 |
| 快捷键先注销旧值且失败仍成功 | S3 | 新值失败恢复旧注册与旧配置 |
| Updater 多锁无 generation | S3 | metadata/bytes/task/status 同代，安装失败可重试 |
| Health 参数无后端范围校验 | S3 | 非法值稳定 400/IPC validation，算术无溢出 |
| 未定义 CSS variables | S4、S6 | 全部生产 CSS var 有定义或显式 fallback，CI 阻断漂移 |
| Dialog/Drawer 无 focus trap/Escape/restore/inert | S4 | 共享 primitive 通过键盘和读屏契约测试 |
| Attention 嵌套交互、Workbench tabs 语义不完整 | S4 | 单一 tab stop 与 ARIA keyboard pattern |
| Mobile drawer 无 modal 行为 | S4 | focus trap、Escape、restore、inert 与 touch 行为并存 |
| Transfer dropzone 不响应 Enter/Space | S2、S4 | button 语义测试通过 |
| 无 `prefers-reduced-motion` | S4 | 系统减少动效时取消非必要位移/shimmer |
| 12 个一级入口、Workbench 可发现性弱 | S4 | 项目 rail 有明确分区、空态 CTA 和用途说明 |
| 用户可见/i18n 辅助文案硬编码 | S4 | 生产 UI 文案全部走 typed i18n，DesignSystem 除外 |
| App 同步导入全部路由与编辑器 | S4 | route/editor/chart 分包，mobile 首载满足预算 |
| production sourcemap 占 10.8 MiB | S4 | 安装包不分发可直接访问的 source map |
| terminal buffer 高频 O(n) 字符串分配 | S4 | ring/deque + frame batching 压测达标 |
| 缺少路由级 ErrorBoundary | S4 | AppShell、Workbench、mobile 局部恢复可用 |
| Orchestrator claim 在单连接事务内读文件 | S5 | 文件 IO 在事务外，事务内仅有限候选 CAS |
| CC History 全量/N+1/非事务 bulk | S5 | 分页、批量查询、事务 upsert、协议上限和混合版本测试 |
| 缺少 DB wait/scheduler latency 指标 | S5 | Doctor/受控日志可观测聚合延迟且不泄露内容 |
| 核心 E2E 只覆盖 Attention/Screenshot | S6 | Transfer/Scratchpad/Prompts/Workbench/mobile/permission/settings 与固定 LAN 边界有门禁 |
| 关键 DTO 只有 TS 泛型断言 | S6 | health/runtime/task view/transfer 与 LAN listener/exposure metadata 具备运行时 parser |
| 巨型 TS/Rust 模块回归半径大 | S6 | 按领域边界分阶段降复杂度并保持 characterization |
| 真机 GUI、权限、WSL、multi-host mDNS 未验证 | S6 | 验证矩阵明确自动/人工证据与 NOT VERIFIED |

## 6. 全局非目标

- 不重写 Tauri/React/Rust 技术栈。
- 不引入 Redux、Zustand、GraphQL、云端身份平台或遥测 SaaS。
- 不把个人可信 LAN 产品变成多租户互联网服务。
- 不在本轮实现用户账号、云端中继、端到端文件内容加密或公网穿透。
- 不借机重新设计所有页面视觉；只处理可用性、可访问性、信息架构和 token 一致性。
- 不一次性拆完所有大文件；只有已被当前计划触达且已有 characterization 的领域才拆。
- 不宣称 hosted runner 已验证真实系统权限、WSL+tmux 或多机 mDNS。

## 7. 跨项目接口与迁移纪律

### 7.1 LAN 边界发布

1. 先固定真实 socket peer 范围、bind/mDNS，并复用现有 method/path/retry inventory。
2. 再接入 browser Origin/Host、WebSocket Origin、Content-Type、body/deadline/concurrency 防护，以及 loopback + control-file token 的 backend stop 隔离。
3. 最后让 Settings、Mobile access、Doctor 与 CLI 展示实际 listener、端口、mDNS、wildcard fallback 和固定风险说明；新旧版本均保持合法 LAN 业务请求无鉴权全读写执行。
4. mixed-version 只验证旧客户端与新服务端继续完成既有业务，不协商身份、模式或权限。

### 7.2 前端行为迁移

- Dialog/Drawer primitive 先以适配现有 DOM/文案方式替换 destructive flows，不同时重做视觉。
- 路由拆包不得改变 URL、Provider 生命周期、xterm DOM 常驻或 Tauri overlay 启动路径。
- polling 迁移必须保留事件丢失后的低频兜底刷新。

## 8. 全局成功指标

1. 文件传输、速记本和 Prompt 的失败路径不再伪装成功或静默丢数据。
2. 合法 LAN peer 无需凭据即可读取、写入和执行终端、文件、Git、Worktree 与 Orchestrator；非法 peer、浏览器跨站调用和远端 backend stop 有完整回归测试。
3. Cloud Sync、配置、快捷键和 Updater 在并发/磁盘失败/乱序下保持单一权威状态。
4. 生产 CSS 不包含无定义且无 fallback 的 token；核心 Dialog/Drawer 满足键盘焦点合同。
5. mobile initial gzip 不超过子 spec 固定预算，生产包不携带可直接服务的完整 source maps。
6. SQLite 长事务不执行磁盘文件读取；CC History 同步有单批 item/bytes 上限。
7. 新增核心旅程 E2E 与故障注入测试进入 CI；真机未验证项在质量矩阵中保持诚实标注。

## 9. 文档与实施纪律

- 六份 spec 是产品/技术行为权威；六份 plan 是执行步骤权威。
- 总纲只规定依赖、范围和覆盖，不复制子计划的函数级实现。
- 每个实施计划使用独立 worktree/branch，按任务提交；不得跨计划 broad stage。
- 每个任务先写失败测试，再写最小实现，再运行领域测试和影响面验证。
- 产品持久行为变化在实现时同步 `docs/prd.md`；架构/命令/陷阱同步最相关 `AGENTS.md` 或 `CLAUDE.md`。
- 每个 wave 完成后重新运行全仓事实校验，禁止让计划文档变成新的陈旧来源。
