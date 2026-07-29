/goal

在 `/Users/hans/web_project/cc-partner` 完整交付 Multi-CLI Agent Hub：Claude Code、Codex CLI、OpenCode 在同机自动收敛用户级/项目级/嵌套指令与 Skill、Command、Agent、MCP、Plugin；保留 shared/adapted/targetOnly 边界；支持用户手动 LAN source push、Git device-lane 备份/确认导入及可验证的 OpenCode runtime。不要缩减既定范围，也不要把尚未验证的平台或 CLI 版本宣称为完成。

## 1. 计划白名单

仅执行以下计划，身份与稳定合并顺序为：

1. Gate A — `docs/superpowers/plans/2026-07-29-agent-hub-gate-a-foundation-instructions.md`（10 tasks）
2. Gate B — `docs/superpowers/plans/2026-07-29-agent-hub-gate-b-portable-assets.md`（9 tasks）
3. Gate C — `docs/superpowers/plans/2026-07-29-agent-hub-gate-c-replication-backup.md`（8 tasks）
4. Gate D — `docs/superpowers/plans/2026-07-29-agent-hub-gate-d-plugin-runtime.md`（8 tasks）

`docs/superpowers/plans/2026-07-29-multi-cli-agent-hub-program.md` 仅是跨 Gate 协调和最终认证依据，不作为第五份实施计划重复执行。明确排除 `docs/superpowers/plans/` 下其余全部计划；不得扫描目录后自行扩展白名单。

## 2. 依据与优先级

开始前完整读取根 `AGENTS.md`、目标目录沿途分层指令、`docs/superpowers/specs/2026-07-29-multi-cli-agent-hub-design.md`、Program 计划及四份白名单计划。优先级：用户/分层指令 > 设计中的已确认产品决策与边界 > Program 的共享合同/交付门 > Gate 的任务细节。冲突时采用不扩大产品边界且更严格的验收条件；确实不可调和时保留可恢复点并报告 blocker，不得猜测。

## 3. Plan Dependency Graph

```text
Gate A -> Gate B -> Gate C -> Gate D -> Program Task 5 certification -> final review
```

只有前一 Gate 的 Completion Contract、计划级双审和集成验证全部通过，下一 Gate 才 dependency-ready。白名单顺序只是同波次稳定合并 tie-break，不把无依赖任务隐式串行化；每份计划内唯一的 `Task Dependency Graph` 是任务调度权威。两级图都是最大并行边界：允许进一步串行，禁止增加并发。

## 4. 每任务执行循环

每个 dependency-ready task 使用 fresh implementer：先读取完整任务与适用分层指令；建立可复现失败证据/RED 测试；做最小充分实现；运行任务列出的 focused verification；自审 diff、范围、日志/凭据和未验证项；形成清晰提交与结构化报告；合入计划分支后运行该 task/wave 的 integration verification。任务层只做 self-check，不调用 Codex 或 reviewer subagent。失败先修复再进入后继任务。

## 5. 隔离、波次与集成

计划 worker 与可并行 task worker 均使用独立 branch/worktree；最多 4 个并发计划 worker、4 个并发 task implementer，且只能启动依赖已满足、写集不冲突的节点。本计划图实际一次仅有一个 Gate ready；Gate 内按其任务图并行。每个 wave 从同一已验证 baseline 分支，完成后按计划任务编号稳定合并并运行集成验证；冲突解决、rebase 或 merge 改变已审范围时，相关验证与评审失效并重跑。不得覆盖用户既有未提交改动。

## 6. 每计划完成门

Gate 全部任务与命令通过后，先形成 clean、committed 的 `PLAN_BASE...HEAD` branch-only package。随后启动两个 fresh Superpowers review subagent，二者读取同一 commit range 及相关 Plan/spec：一名返回明确 `Spec compliance` verdict，另一名返回明确 `Code quality` verdict，implementer 不得审自己的 Plan。每轮一次修完全部 High/Medium，并 sweep 同一 invariant 的 sibling call sites/routes、平台分支和状态转换；复审前逐项回归上一轮 findings，执行 ordering/stale-write、retry/idempotency、cancellation/cleanup、locking/shared-state 的适用 adjacent-race checklist，记录命令/结果并重新生成 package，再由两名 fresh reviewer 复审至双通过。通过后才合入 integration 并重跑 Gate completion smoke；记录 base/head、evidence ID、实际 CLI/平台版本及 `NOT VERIFIED` 项。

## 7. Program 认证与终审

四个 Gate 按序集成后，在 integration 执行 Program Task 5 的全部 Rust、前端、协议、文档、仓库无自动 mutation 检查并提交认证事实。然后对固定 `PROGRAM_BASE...integration HEAD --scope branch` 做全局终审：第 1–3 轮只能使用 `codex-plugin-cc`，先 `/codex:setup`，之后只用 `/codex:*`；分别请求 fresh final spec/adversarial review 与 fresh code-quality review，完整保存输出。每轮修完全部 High/Medium，回归全部既有 findings、相邻同类面与 race checklist，再重跑受影响 Gate 和 Program 检查。主 worker 无插件能力时委派 plugin-capable worker；插件不可用只会阻塞 Codex 轮次，不得降级，且禁止直接调用 `codex` CLI、脚本或 Bash 模拟插件。第 3 轮仍未通过时，第 4 轮起改用 fresh Superpowers 双 reviewer，循环至通过或形成真实 blocker。

## 8. Completion Contract

仅当四份 Gate 各自 Completion Contract、Program Task 5、计划级双审、全局终审全部通过，integration clean 且所有实现/迁移/回滚/混合版本/崩溃恢复/无重复发现/无明文凭据日志证据可追溯时，才报告完成。真实设备或平台未执行就保持 `NOT VERIFIED`，不得用模拟结果替代 L3。若受外部环境阻塞，先穷尽安全的范围内替代方案；仍阻塞则提交或保留 clean recovery point，并报告已完成 task、branch/worktree、失败命令与输出摘要、待执行节点和恢复步骤，不得虚报完成。

## 9. Git 权限

允许创建本地 branch/worktree、按 task/计划提交、在 integration 做稳定顺序合并与冲突修复。未经用户另行授权，不得 push、创建 PR、发布 tag/release，或合入/改写受保护主分支。
