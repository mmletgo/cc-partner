# Workspace Safe Restore 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：现有持久Workbench project/worktree/session与Browser target
- 对应计划：`docs/superpowers/plans/2026-07-15-workspace-safe-restore.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

当前project、worktree、terminal元数据可持久化，tmux可以重连，但active project仅在localStorage，active worktree/session、workspace view、inspector、browser target等多为内存态。应用重启或切换project后，用户需要重新找回工作现场。

恢复如果顺便写terminal、启动Agent、创建worktree或猜测不存在的session，会造成高风险副作用。因此恢复必须先预检，只复用已有资源，并允许安全的部分恢复。

## 2. 目标

1. Desktop零配置自动保存最后工作现场；稳定selection变化500ms后合并保存。
2. 打开Workbench时先执行side-effect-free preflight，再原子应用可安全恢复部分。
3. tmux session存在时允许safe attach；不存在时跳过，不创建shell。
4. remote project layout保存在控制设备，真正session preflight/attach由owning device执行。
5. 无法完整恢复时只显示一次简短摘要，不连续弹窗。
6. 可选命名snapshot只保存结构metadata，不成为Command Recipe。

## 3. 非目标

- 不保存或恢复terminal bytes、Prompt、assistant回复、env、token、命令或Agent配置。
- 不发送terminal input、不启动/resume Agent、不创建terminal/worktree。
- 不覆盖未保存编辑器内容，不保存文件正文。
- 不自动把remote layout映射到local repo或其他device。
- 不做可执行布局配方、宏或全局Quick Open。
- Mobile v1不自动应用Desktop layout。

## 4. Layout模型

```rust
pub struct WorkspaceLayout {
    pub schema_version: u32,
    pub id: String,
    pub slot_key: String,
    pub kind: WorkspaceLayoutKind,
    pub name: Option<String>,
    pub project_id: String,
    pub active_worktree_id: Option<String>,
    pub active_session_id: Option<String>,
    pub workspace_view: WorkspaceView,
    pub inspector_tab: InspectorTab,
    pub browser_target_url: Option<String>,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
}
```

- auto slot固定`desktop:auto`；named slot为`named:<uuid>`。
- `schemaVersion=1`。
- browser target必须通过现有loopback normalization；不保存preview ID，因为preview有TTL。
- 类型中不存在command、content、prompt、env、provider字段。
- layout存控制设备本地sidecar SQLite，不进入Cloud Sync/P2P。

新增`workbench_workspace_layouts`表，`slot_key`唯一，revision用于CAS。

## 5. 自动保存

- 前端`buildWorkspaceLayoutDraft`只接收project/worktree/session/view/inspector/browser target稳定ID。
- terminal output、pane resize、timer、Agent phase、编辑器内容变化不触发保存。
- 500ms debounce合并连续selection变化。
- save使用`expectedRevision`；conflict后重新读取最新layout并从当前UI状态重算，不盲覆盖。
- mutation结果未知时通过get/revision对账，不自动重放旧draft。
- 无project时不写空layout，不删除上一次可恢复现场。

## 6. Preflight

```rust
pub struct WorkspaceRestorePlan {
    pub restore_id: String,
    pub layout_id: String,
    pub layout_revision: u64,
    pub status: RestorePlanStatus,
    pub resolved_project_id: Option<String>,
    pub resolved_worktree_id: Option<String>,
    pub resolved_session_id: Option<String>,
    pub workspace_view: WorkspaceView,
    pub inspector_tab: InspectorTab,
    pub browser_target_url: Option<String>,
    pub actions: Vec<WorkspaceRestoreAction>,
}
```

action outcome只允许：

- `select`
- `reuse`
- `safeAttach`
- `skip`

preflight必须纯读：

- 验证project/remote shortcut仍存在；
- worktree/session归属一致；
- tmux backend target仍存在；
- raw PTY exited或不存在时skip；
- browser target仍是允许的loopback URL；
- remote offline返回offline/partial并保留layout。

禁止调用现有可能restore/spawn的sessions list路径。

## 7. Safe attach与应用顺序

`safe_attach_workbench_session(sessionId)`仅允许：

- persisted backend为tmux；
-对应tmux session/window已经存在；
-复用RestoreClaimGuard只创建attach client/registry连接；
-现有registry session直接返回。

明确禁止`tmux new-session/new-window`、raw PTY fallback、terminal write、Claude/Codex resume。

前端应用顺序：

1. preflight完成前不修改UI；
2. select project；
3. select existing worktree；
4. focus existing session或safe attach；
5.恢复workspace view/inspector；
6. browser target重新走受限preview discovery/create；
7.汇总所有skip原因并显示一次notice。

应用前保存previous selection；前端异常时恢复selection。server唯一允许的副作用是idempotent attach和TTL preview，不做破坏性回滚。

## 8. Remote与兼容

- remote layout本体留在控制设备；owner只接收inner project/worktree/session ID做preflight/safe attach。
- route必须local-project-only，禁止递归remote shortcut。
- capability为`workbench.workspace-safe-restore.v1`。
- 旧peer unsupported时控制设备只恢复project selection，其他项skip并说明。
- layout schema未知版本fail-closed，不猜字段。
- rollback移除restore coordinator后不删除layout表，不影响现有project/session/browser功能。

## 9. UI

-默认启动静默恢复可安全项；完全成功不弹通知。
-partial只显示一条可关闭inline notice，例如“已恢复3项，2项已跳过”。
-notice可展开reason code，但不展示terminal内容或绝对remote path。
-命名snapshot是Workbench二级入口，仅提供保存当前结构、应用、删除；无命令编辑器。
-不会为恢复新增第八个Workbench controller；使用纯coordinator和现有controller窄bridge。

## 10. 测试与验收

1. schema/revision CAS/invalid enum/browser target validation有repo测试。
2. preflight不存在project/worktree/session、raw PTY、tmux existing/offline remote有测试。
3.静态与运行时测试证明restore路径terminal write count=0、Agent spawn count=0、worktree create count=0。
4. safe attach idempotency、claim guard cleanup和owner mapping有Rust测试。
5.自动save debounce、CAS conflict、dirty editor不覆盖和partial notice有前端测试。
6. E2E覆盖重启恢复、remote offline、stale session与完全成功静默路径。

## 11. Spec自审

- Restore只处理UI结构和已有资源引用，不执行用户命令。
- 正常恢复零配置、零弹窗；异常只给一次摘要。
- remote资源仍由owning device验证和attach。
