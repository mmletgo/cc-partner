# Claude Code Subagent-Driven 开发交接 Prompt

下面整段内容应原样交给 Claude Code。它是一份执行授权与流程契约，不是让你重新讨论产品方案。

---

你现在是 cc-partner 工程改进项目的主控 Agent（controller）。请在同一 Claude Code 会话中，严格使用 **Superpowers Subagent-Driven Development** 执行仓库里已经确认的 8 份 implementation plan。

## 一、目标与完成定义

你的目标不是重新设计方案，而是把下列 8 份 plan 按指定顺序全部实施、测试、逐任务审查，并形成可独立回滚的阶段提交。

仓库：

```text
/Users/hans/web_project/cc-partner
```

当前已确认的设计与计划提交：

```text
6c71f29 docs: define global inbox and engineering improvement specs
62b2f21 docs: add inbox and engineering implementation plans
```

开始前必须确认当前 HEAD 包含这两个提交：

```bash
cd /Users/hans/web_project/cc-partner
git merge-base --is-ancestor 6c71f29 HEAD
git merge-base --is-ancestor 62b2f21 HEAD
git status --short
```

如果任一 ancestor 检查失败，或现有未提交改动会与计划重叠，不得覆盖、reset 或清理用户改动；把所有冲突一次性整理成一个 blocker 报告给用户。

“全部完成”只成立于：

1. 8 份 plan 的每一项 checkbox task 都已实施。
2. 每个 task 都经过“实现 → 自审 → spec compliance review → code quality review → 必要修复 → re-review”。
3. 每份 plan 的 Completion Contract 均有新鲜验证证据。
4. 每份 plan 保留独立 branch、提交边界和 review 记录，未压成一个不可审查的大提交。
5. 最终 whole-program review 没有未解决的 Critical / Important finding。
6. 工作区干净；所有未验证的 hosted/platform 行为被如实标记，不能用“应该通过”代替证据。
7. 未经用户额外授权，不 push、不创建 PR、不合并到 master。

## 最高优先级：唯一允许执行的 Plan 白名单

本次开发**只允许执行下面 8 份 plan**，并且只能按数组顺序执行：

```bash
bash <<'BASH'
PLAN_FILES=(
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-vitest-frontend-ci.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-workbench-controller-extraction.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-p2p-protocol-metadata-errors.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-remote-orchestrator-runtime-snapshot.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-global-inbox.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-cross-platform-smoke-ci.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-backend-logs-doctor.md
  /Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-documentation-calibration.md
)

PLAN_TASK_COUNTS=(8 9 9 8 10 6 8 7)

test "${#PLAN_FILES[@]}" -eq 8
for i in "${!PLAN_FILES[@]}"; do
  test -f "${PLAN_FILES[$i]}"
  actual_tasks=$(rg -c '^### Task ' "${PLAN_FILES[$i]}")
  test "$actual_tasks" -eq "${PLAN_TASK_COUNTS[$i]}"
done
BASH
```

这是一个自包含 Bash 校验块；即使当前交互 shell 是 zsh，也必须按上面方式交给 Bash 执行，避免数组下标语义差异。

严格禁止：

1. 禁止通过 `rg --files docs/superpowers/plans`、`find docs/superpowers/plans` 或目录遍历自动发现“还要执行哪些 plan”。
2. 禁止执行、续跑、补做或重新解释 `docs/superpowers/plans/` 中不在 `PLAN_FILES` 数组里的任何历史 plan。
3. 禁止因为旧 plan 仍有未勾选 checkbox，就把它加入 todo、ledger、branch 或验收范围。
4. 禁止让 implementer/reviewer 自行选择 plan 文件；主控只能把当前白名单 plan 的单个 task brief 交给它。
5. 白名单文件缺失、Task 数量不匹配或内容损坏时，立即报告 blocker；不得选择文件名相似、日期更早或主题相近的旧 plan 替代。
6. 历史 plan 只能在当前白名单 plan 明确引用、且为理解已存在代码所必需时作为只读背景；它永远不能成为本次执行任务来源或验收清单。
7. 当前阶段的 `PLAN_FILE` 必须逐字等于白名单中同阶段的绝对路径，不得由搜索结果、最近修改时间或 Agent 自主判断产生。

本次总任务数固定为：

```text
8 + 9 + 9 + 8 + 10 + 6 + 8 + 7 = 65 tasks
```

ledger 只能登记这 65 个 task。出现第 66 个 task、未列出的 plan 名或历史 plan 路径时，视为流程错误并停止派发。

## 二、权威资料与指令层级

主控 Agent 必须亲自完整读取这些文件；不要让 subagent 替你解释流程或方案。除下列两份 spec 和上面的 8-plan 白名单外，不得把其他历史 spec/plan 当成本次执行范围：

```text
/Users/hans/web_project/cc-partner/AGENTS.md
/Users/hans/web_project/cc-partner/docs/superpowers/specs/2026-07-11-global-inbox-design.md
/Users/hans/web_project/cc-partner/docs/superpowers/specs/2026-07-11-engineering-improvement-program-design.md
```

进入前端或后端目录前，再读取：

```text
/Users/hans/web_project/cc-partner/web/CLAUDE.md
/Users/hans/web_project/cc-partner/src-tauri/CLAUDE.md
```

执行时的优先级：

```text
用户当前 Prompt
→ 根 AGENTS.md
→ 目标目录 CLAUDE.md / 更深层指令
→ 对应书面 spec
→ 对应 implementation plan
→ 当前代码与测试证据
```

若 plan 与 spec、项目指令或当前实现出现真实矛盾：

- 在开始任何实现前完成一次 pre-flight scan。
- 把所有矛盾合并成一次问题清单，每项并列引用冲突文本，请用户决定哪一条优先。
- 不得在执行中逐个打断用户。
- 非阻塞实现细节按 plan 和现有代码模式做最小合理判断，记录在 ledger。

涉及 Inbox UI 时，遵守已经确认的 huashu-design 视觉方向：桌面独立页面、移动端导航第二项。不要重新打开 A/B 方案讨论，也不要新增浮层 Inbox、顶栏常驻按钮或自动抢占移动首屏。

## 三、必须使用的 Superpowers 工作流

开始时确认并读取下列 skills：

```text
superpowers:using-git-worktrees
superpowers:subagent-driven-development
superpowers:test-driven-development
superpowers:systematic-debugging
superpowers:verification-before-completion
superpowers:finishing-a-development-branch
```

如果 `superpowers:subagent-driven-development` 或其脚本不存在，停止并报告 blocker；不要静默模拟一个缩水流程。

所有 reviewer 必须由已安装的 `codex-plugin-cc` 调用 Codex 执行，Claude Code 自身或 Claude subagent 不得充当 reviewer。开始实现前先运行 `/codex:setup`，确认插件、Codex CLI 和认证状态可用；若不可用，停止并报告 blocker，不得降级为 Claude 自审或跳过 review。

Subagent-Driven 的不可变规则：

1. 一个 plan task 对应一个 fresh implementer subagent。
2. 不允许两个写入型 implementer 并行；即使任务看起来独立，也要串行，避免共享 worktree 冲突。
3. implementer 只读取自己的 task brief、相关分层指令和必要接口，不把完整会话历史或整份 plan 塞给它。
4. implementer 必须使用 TDD：先建立失败证据，再实现，再跑聚焦测试。
5. implementer 完成后必须提交、自审并写 report 文件。
6. 每个 task 必须有独立 Codex reviewer，由 `codex-plugin-cc` 的 `/codex:adversarial-review` 调用；reviewer 同时给出：
   - Spec compliance：✅ / ❌
   - Code quality：Approved / Findings
7. reviewer 有 Critical / Important finding 时，不得进入下一 task。
8. 修复 Agent 必须运行覆盖修复的测试，把命令、结果和输出摘要追加到同一个 report，再交原 reviewer re-review。
9. reviewer 的 “⚠️ Cannot verify from diff” 由主控亲自查证；未查证不得标 task complete。
10. implementer 自审、Claude 主控审查或另一个 Claude subagent 都不能替代 Codex reviewer。
11. 全部 task 完成后，还要通过 `codex-plugin-cc` 进行一次 plan-level broad review；8 个阶段都完成后再通过该插件做一次 whole-program final review。
12. 不要在任务间询问“是否继续”。只有真实 blocker、必须由用户选择的矛盾、或全部完成时才停。

## 四、Worktree 与 branch 拓扑

绝不直接在 master 上实现。

先用 `superpowers:using-git-worktrees` 创建一个长期集成 worktree：

```text
integration branch: sdd/cc-partner-improvement-program
suggested worktree: ../cc-partner-sdd-integration
```

然后每份 plan 建独立 plan branch/worktree，起点都是当时 integration branch 的 HEAD：

```text
sdd/01-vitest-frontend-ci
sdd/02-workbench-controller-extraction
sdd/03-p2p-protocol
sdd/04-remote-runtime-snapshot
sdd/05-global-inbox
sdd/06-cross-platform-smoke
sdd/07-backend-logs-doctor
sdd/08-documentation-calibration
```

每个 plan 的规则：

1. 在独立 worktree 内按 Task 1..N 串行执行。
2. 每个 task 使用 plan 指定的 commit 边界；不要 squash。
3. plan-level final review 通过后，使用 `superpowers:finishing-a-development-branch` 完成该阶段。
4. 不合并 master；把 plan branch 以 `--no-ff` 合并到本地 integration branch。
5. 保留 plan branch，方便后续按阶段审查或创建顺序 PR。
6. 下一 plan 从更新后的 integration HEAD 建分支。
7. 禁止跨 plan 并行，尤其以下共享文件冲突：
   - Workbench controller 与 Inbox 都修改 Workbench。
   - Runtime Snapshot 与 Inbox 都修改 Orchestrator/Workbench。
   - Cross-platform smoke 与 Logs/Doctor 都修改 backend CLI 与 workflow。
   - Documentation calibration 必须看到所有前置实现。

每个阶段 clean 后的本地集成动作使用明确变量，不依赖当前 shell 目录：

```bash
PHASE_SLUG=01-vitest-frontend-ci
git -C "$INTEGRATION_WORKTREE" status --short
git -C "$INTEGRATION_WORKTREE" merge --no-ff "$PLAN_BRANCH" \
  -m "merge: complete $PHASE_SLUG"
git -C "$INTEGRATION_WORKTREE" status --short
```

合并前后都必须为空；若 integration 出现冲突，先建立冲突原因和覆盖测试，不得用 ours/theirs 整体覆盖。

未经用户授权不得 push、创建 PR 或合并 master。跨平台 hosted runner 验证需要 push/dispatch 时，这是一个真实外部权限 blocker：到达该阶段后一次性说明需要的授权、要 push 的 branch 和将触发的 workflow。

## 五、Durable Progress Ledger

在 integration worktree 初始化：

```text
.superpowers/sdd/progress.md
```

ledger 至少包含：

```markdown
# cc-partner SDD Progress

Program base: <commit>
Integration branch: sdd/cc-partner-improvement-program

## Phase 01 — Vitest
- status:
- plan base:
- Task 1:
- ...
- plan final review:
- merge commit:
- minor findings carried forward:

## Phase 02 — Workbench
...
```

规则：

- 每次开始/resume/上下文压缩后，先读 ledger，再读 `git log`。
- ledger 标记 complete 的 task 不得重复派发。
- reviewer clean 后立即追加：
  `Task N: complete (commits <base7>..<head7>, spec ✅, quality approved)`
- Minor finding不能丢弃，记录到阶段条目并交 final reviewer 统一复核。
- ledger 是 gitignored scratch，不要提交到产品仓库，也不要运行会删除它的 `git clean -fdx`。

## 六、每个 Task 的标准执行循环

对每个 plan 的 Task N 严格执行：

### 1. 生成 task brief

定位已安装的 `superpowers:subagent-driven-development` skill 目录，把包含 `SKILL.md` 的目录记录为 `SDD_SKILL_DIR`，然后使用：

```bash
PLAN_FILE=/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-vitest-frontend-ci.md
TASK_NUMBER=1
"$SDD_SKILL_DIR/scripts/task-brief" "$PLAN_FILE" "$TASK_NUMBER"
```

进入后续阶段时只使用白名单中下一行的绝对路径；禁止手工搜索或猜测路径。每个阶段的 `TASK_NUMBER` 从 1 顺序递增到白名单校验块中对应的 task count。

记录脚本输出的 brief 路径。report 路径与 brief 同名：

```text
.../task-N-brief.md
.../task-N-report.md
```

### 2. 记录 BASE

派发 implementer 前：

```bash
BASE=$(git rev-parse HEAD)
```

不能在 review 时使用 `HEAD~1` 代替 BASE，因为一个 task 可能产生多个提交。

### 3. 派发 fresh implementer

implementer Prompt 只包含：

- 一句话说明本 task 在当前阶段的作用。
- “先读 brief，它是完整且逐字约束的 requirements”。
- brief 路径。
- 本 task 需要的 AGENTS/CLAUDE 路径。
- 早期 task 已建立、但 brief 无法知道的窄接口。
- report 路径和返回契约。
- 要求使用 `superpowers:test-driven-development`。
- 明确允许编辑的文件范围。
- 禁止顺手修无关问题、禁止改 plan/spec、禁止扩大兼容范围。

implementer 必须返回四种状态之一：

```text
DONE
DONE_WITH_CONCERNS
NEEDS_CONTEXT
BLOCKED
```

并只在回复中给出：

```text
status
commit(s)
one-line test summary
concerns
report file path
```

完整实现细节和测试输出写 report，不粘贴到主会话。

### 4. 处理 implementer 状态

- DONE：进入 review。
- DONE_WITH_CONCERNS：主控先读 concerns；正确性/范围疑虑必须先解决。
- NEEDS_CONTEXT：补充缺失上下文，再派发同一 task。
- BLOCKED：
  1. 缺上下文 → 补上下文；
  2. reasoning 不足 → 升级模型；
  3. task 太大 → 只在不改变 plan 验收的前提下拆成更小子任务；
  4. plan 错误/需要产品选择 → 汇报用户。

不得让同一模型无变化地反复重试。

### 5. 生成 review package

implementer 完成后：

```bash
"$SDD_SKILL_DIR/scripts/review-package" "$BASE" HEAD
```

使用脚本打印出的唯一 diff package 路径。

### 6. 派发 task reviewer

禁止使用 Claude Code 的 Agent/Task 工具创建 reviewer。必须在当前仓库通过 `codex-plugin-cc` 调用 Codex：

```text
/codex:adversarial-review --wait --base <TASK_BASE> --scope branch <FOCUS>
```

其中 `<TASK_BASE>` 是该 task 开始前的 commit；`<FOCUS>` 必须明确要求 Codex reviewer 读取三个文件：

```text
task brief
implementer report
review package
```

同时在 `<FOCUS>` 中给出对应 plan 路径，并要求读取该 plan 的 Global Constraints 原文。不要使用 `/codex:review` 代替：它不支持附加本流程所需的 task/spec 审查说明。

`--wait` 是强制项，确保 Codex 返回结论前主控不会进入下一 task。如果因运行环境限制必须使用 `--background`，则必须记录 job ID，并用 `/codex:status <job-id> --wait` 等待完成、再用 `/codex:result <job-id>` 取得完整结果；结果未取回前 task 保持 in-progress。

传给 Codex reviewer 的 `<FOCUS>` 不得出现“不要报告某问题”“最多算 Minor”“plan 已经决定所以忽略”等预判语言。

reviewer 必须输出：

```text
Spec compliance: ✅ / ❌
Code quality: Approved / Findings
Findings:
- severity
- file:line
- evidence
- violated requirement
- recommended correction
Cannot verify from diff:
- ...
```

保留并记录插件返回的完整 Codex 输出，不得由 Claude 改写、压缩或冒充 Codex 结论。如果输出缺少上述两个 verdict，review 尚未完成：补全 review context 后重新调用 Codex，不得由 Claude 自行补判。

### 7. 修复与 re-review

- Critical / Important：派发一个 fix subagent 处理本轮完整 findings。
- fix subagent 在同一 report 追加：修改、测试文件、命令、输出。
- 主控确认 report 里三项齐全后，使用相同 `<TASK_BASE>`、更新后的 review package 和相同审查焦点，再次通过 `/codex:adversarial-review` 调用 Codex re-review。
- 重复直到 spec ✅ 且 quality approved。
- Minor 写入 ledger，交 plan-level/final reviewer；不能静默丢弃。

### 8. 标记完成

只有 review clean 后才更新 todo 和 ledger，再进入 Task N+1。

## 七、模型路由

如果 Claude Code 的 Agent/Task 工具支持显式 `model`，每次派发 implementer/fix subagent 时都必须填写，不要继承主会话默认模型。

按运行环境实际可用模型映射：

- 机械、单文件、完整规格：快速模型。
- 常规多文件实现：标准工程模型。
- 并发、协议、Worktree 拆分、日志隐私、复杂 Debug：最强可用模型。
- task、plan-level 与 whole-program reviewer：不通过 Claude Agent/Task 派发，统一由 `codex-plugin-cc` 调用 Codex；不得套用 Claude 模型路由或用 Claude 模型替代。

如果运行环境不支持项目 AGENTS.md 中的 GPT 型号名称，使用 Claude Code 工具实际支持的等价档位，例如 Haiku/Sonnet/Opus，并在 ledger 记录一次映射。不得伪造模型参数。

本项目建议：

- Vitest 机械迁移：快速/标准。
- Workbench controller：最强。
- P2P/error/request ID：最强。
- Remote runtime：标准或最强。
- Inbox backend aggregation：最强；UI wiring：标准。
- CI/YAML：标准。
- Logs/sanitizer/doctor：最强。
- Documentation calibration：标准。
- 所有阶段 final review：通过 `codex-plugin-cc` 调用 Codex。

## 八、严格执行顺序

禁止更改下面的 plan 顺序，禁止并行执行两个 plan。

### Phase 01 — Vitest Migration and Frontend CI

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-vitest-frontend-ci.md
```

共 8 个 Task，严格执行 Task 1→8。

为什么第一：

- 后续所有前端新测试与 Workbench characterization 都依赖统一 runner。
- 必须先消除浮动 `npx --yes tsx` 与 CI 无单测的问题。

阶段门禁：

- 47 个现有 unit test 与 plan 清单一一对应。
- 最终没有 legacy runner、手工执行列表、`process.exit`。
- `npm ci && npm test` 自动发现全部单测。
- unit 与 E2E 是独立 CI job。
- 执行 plan Completion Contract 的全部命令。
- plan-level final reviewer clean 后才合并 integration branch。

### Phase 02 — Workbench Characterization and Controller Extraction

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-workbench-controller-extraction.md
```

共 9 个 Task，严格执行 Task 1→9。

为什么第二：

- 先用 Vitest 建 characterization。
- 必须在 Inbox/deep-link 再次修改 Workbench 前完成行为保持型拆分。

阶段门禁：

- Project、Terminal、Worktree/Git、Files、Automation、Prompt/Search overlays 均有独立 controller。
- xterm DOM、buffer/replay、tmux focus、dirty/baseHash、stale guards 行为不变。
- `Workbench.tsx` 格式化后不超过 1,200 行。
- 全部 Workbench characterization、unit、lint、build 通过。
- 不混入 Inbox 功能或视觉变化。

### Phase 03 — P2P Protocol Metadata and Error Envelope

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-p2p-protocol-metadata-errors.md
```

共 9 个 Task，严格执行 Task 1→9。

为什么第三：

- Remote Runtime 与 Mobile Attention capability gate 都依赖 protocol v1。
- error envelope/request ID 必须先于新增 P2P route。

阶段门禁：

- health 是完整 capability 权威源；mDNS 只是最多 220 UTF-8 bytes 的提示。
- 只声明当前 build 已真实注册 route 的 capability。
- v0 legacy 契约继续可读，仅维护一代滚动兼容。
- status/code/request ID/retryable/details 映射完整。
- Tauri IPC 的既有 AppError 外形不被破坏。
- route idempotency inventory 完整，unsafe writes 无 transport retry。
- route inventory checker、Rust tests、fmt、clippy 全部通过。

### Phase 04 — Remote Orchestrator Runtime Snapshot

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-remote-orchestrator-runtime-snapshot.md
```

共 8 个 Task，严格执行 Task 1→8。

为什么第四：

- 它是 protocol v1 的第一个真实 capability consumer。
- 先解决 Orchestrator 共享文件，再实现 Inbox，避免两个阶段并发冲突。

阶段门禁：

- 固定 route：`POST /api/orchestrator/runtime-snapshot`。
- 请求体严格为 `{"project_id":"..."}`。
- route 只接受 owning device local project，拒绝 remote recursion。
- live/offline/unsupported/unavailable 独立建模。
- desktop/mobile cache 分离、仅内存、仅展示。
- 不允许本机 telemetry 冒充远端数据。
- Rust、Vitest、lint、build 与文档契约通过。

### Phase 05 — Global Inbox

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-global-inbox.md
```

共 10 个 Task，严格执行 Task 1→10。

为什么第五：

- 依赖 Vitest、已拆分 Workbench、P2P capability 和稳定 Orchestrator runtime 文件结构。
- 必须先完成 outbox Retry/Discard 真实闭环，再把 failed outbox 接入 Inbox。

阶段门禁：

- 不新增 Inbox table、read/dismiss/snooze/history。
- 仅包含 Human Review、Blocked、failed outbox、项目存在时的 tmux blocking dependency。
- pending/sending/mirrored/discarded、普通设备离线和已解决任务不显示。
- remote live/cached、`cachedAt`、4 并发上限、整次失败规则正确。
- 桌面独立页面，Home 后固定入口；移动端 Projects 仍默认、Attention 为第二项。
- 两端 badge/count/group/order 完全一致，100+ 显示 99+。
- 条目只导航，不在 Inbox 内执行副作用。
- 已解决 target、初次失败、refresh stale、空态、light/dark、keyboard 都有测试。
- Rust、Vitest、Playwright、lint、build 和项目记忆通过。

### Phase 06 — macOS / Windows Smoke CI

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-cross-platform-smoke-ci.md
```

共 6 个 Task，严格执行 Task 1→6。

为什么第六：

- 在协议、runtime、Inbox 的主要平台敏感代码稳定后建立跨平台门禁。
- Logs/Doctor 会复用该 workflow。

阶段门禁：

- backend start→health→status→stop、duplicate start、stale control、native PTY echo/exit、platform lifecycle、`cargo check --bins`。
- PR relevant-path 与 daily schedule 均存在。
- 无 `continue-on-error`，有 timeout、cleanup、failure artifacts。
- WSL/tmux、GUI/WebView、权限弹窗、多机 mDNS 明确 NOT VERIFIED。
- 本地只能验证当前平台；macOS/Windows hosted 结果必须来自真实 workflow run。
- 如需 push/dispatch 才能取得证据，停止并向用户申请这项外部授权，不得伪造通过。

### Phase 07 — Backend Rotating Logs and Doctor

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-backend-logs-doctor.md
```

共 8 个 Task，严格执行 Task 1→8。

为什么第七：

- Doctor 以已经验证的 rotation/sanitizer 为前置。
- Cross-platform workflow 已可承载 Windows rename/permissions/JSON smoke。

阶段门禁：

- current log 最大 5 MiB，最多 3 个历史文件。
- Unix 目录 0700、文件 0600。
- stderr/file 同用 sanitizer；禁止 Prompt、正文、env、token、Authorization、凭据。
- `doctor` / `doctor --json` schema 稳定，stdout JSON 纯净。
- home 替换为 `<HOME>`，不输出项目名。
- healthy/degraded/unhealthy 与 exit 0/1/2 正确，正常 stopped 是 info。
- rotation、privacy、doctor fixtures、macOS/Windows smoke 通过。

### Phase 08 — README and Layered Documentation Calibration

Plan：

```text
/Users/hans/web_project/cc-partner/docs/superpowers/plans/2026-07-11-documentation-calibration.md
```

共 7 个 Task，严格执行 Task 1→7。

为什么最后：

- 它只能描述已经落地并验证的事实。
- 不允许提前把未完成 capability、doctor 或 hosted smoke 写成已支持。

阶段门禁：

- README 以 local-first Workbench→Mobile→Orchestrator→headless backend→辅助能力排序。
- IPC/P2P、62116 首选+递增、release 三 job、平台限制准确。
- PRD 对 Inbox/runtime/outbox/doctor 无相互矛盾陈述。
- 根 AGENTS 与 web/src-tauri 指令分层正确。
- docs-only CI guard 能检查链接、anchor、fence 和 stale claims。
- 文档中的所有命令有静态或实际运行证据。

## 九、Debug、范围与安全纪律

遇到测试失败或意外行为：

1. 先使用 `superpowers:systematic-debugging`。
2. 建立可重复失败证据。
3. 读取实际 console/backend/test output。
4. 需要时添加最小诊断日志；定位后删除临时日志。
5. 不通过跳过测试、放宽断言、`continue-on-error` 或吞异常来“修复”。
6. 预存且无关的问题只记录，不扩大范围；同根因、阻塞验证、低风险的问题可修，但必须在 report 说明。
7. 数据库变化必须有迁移、兼容、回滚；Inbox 本身禁止新增业务表。
8. 保留用户已有改动；禁止 `git reset --hard`、`git checkout --`、`git clean -fdx`。
9. 禁止 sudo；如确实需要，说明原因与影响并等待用户。
10. 不在日志、report、Prompt 或回复里回显 token/密码/密钥。

## 十、阶段与最终 Review

### 每份 plan 的 final review

完成该 plan 全部 task 后：

1. 计算该 plan branch 的起始 BASE。
2. 运行：
   `"$SDD_SKILL_DIR/scripts/review-package" "$PLAN_BASE" HEAD`
3. 禁止派发 Claude reviewer；通过 `codex-plugin-cc` 执行：
   `/codex:adversarial-review --wait --base <PLAN_BASE> --scope branch <FOCUS>`。
   `<FOCUS>` 必须列出 Completion Contract、对应 spec、plan、ledger 和 review package 的路径，并要求 Codex 做 plan-level broad review。
4. reviewer 检查：
   - plan Completion Contract
   - 对应 spec
   - 分层指令
   - task ledger 中所有 Minor
   - 测试与验证证据
5. 有 findings 时只派发一个 final-fix subagent 处理完整 finding 列表。
6. 修复后重跑受影响测试和 plan 最终验证，再 re-review。
7. clean 后才能合并本地 integration branch。

### Whole-program final review

Phase 08 完成并合并 integration branch 后：

1. 使用 program 起始 commit 到 integration HEAD 生成 review package。
2. 禁止派发 Claude final reviewer；通过 `codex-plugin-cc` 执行：
   `/codex:adversarial-review --wait --base <PROGRAM_BASE> --scope branch <FOCUS>`。
   `<FOCUS>` 必须列出两份 spec、8 份白名单 plan、所有 Minor ledger 和 program review package，并要求 Codex 做 whole-program final review。
3. 重点检查跨阶段接口：
   - health capability 与 route 是否原子发布
   - runtime/attention 对 v0/v1 的处理
   - Workbench controller 与 Inbox deep-link 是否一致
   - remote outbox Retry/Discard 与 Inbox 自动移除
   - cross-platform workflow 与 Logs/Doctor artifacts
   - README/PRD 是否只描述已验证事实
4. 一个 fix subagent 处理完整 final findings。
5. 重新运行受影响阶段验证和必要的全量验证。
6. 使用 `superpowers:verification-before-completion` 检查新鲜输出。
7. 最后使用 `superpowers:finishing-a-development-branch`，但不自动 push/PR/merge master；向用户呈现可选集成方式。

## 十一、最终交付报告

最终只在全部可本地完成的工作完成，或遇到真实权限 blocker 时向用户报告。

报告必须包含：

```markdown
## Outcome
- completed phases
- blocked phases and exact reason

## Branches and commits
- integration branch/head
- each plan branch
- task commit ranges
- plan merge commits

## Verification evidence
- exact commands
- exit codes / test counts
- hosted workflow URLs and job conclusions（如已获授权执行）

## Reviews
- task review status（含 codex-plugin-cc/Codex job ID 或前台结果引用）
- plan-level final review status（Codex）
- whole-program final review status（Codex）
- remaining Minor findings

## Explicitly not verified
- platform/environment items that lack real evidence

## Working tree
- git status
- no uncommitted product changes

## Next authorized action
- merge / push / sequential PR choices
```

禁止使用“应该通过”“看起来完成”“大概没问题”。所有完成声明必须有当前代码上的新鲜命令输出。

## 十二、现在开始

现在执行以下动作，不要先复述整份 Prompt：

1. 读取根指令、两份 spec，并精确读取白名单第一项 Phase 01 plan；不要扫描 plans 目录。
2. 确认 Superpowers skills/scripts 可用；运行 `/codex:setup`，确认 `codex-plugin-cc`、Codex CLI 与认证可用。
3. 做一次 program/Phase 01 pre-flight conflict scan。
4. 创建 integration worktree、Phase 01 worktree 和 durable ledger。
5. 从 Phase 01 Task 1 开始 Subagent-Driven 循环。
6. 除真实 blocker 外，连续执行，不询问“是否继续”。
