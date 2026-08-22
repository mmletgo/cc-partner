# Agent Hub 分 Agent 独立资产管理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改所有者磁盘、不 remap 到另一家 CLI 的前提下，把 Hub 能单独管的 Plugin/MCP/Skill 开关全部接到 viewing Agent。

**Architecture:** 新增 `is_file_only_viewing_toggle(target, kind, action)`：Grok Plugin 与 Grok/Gemini/Cursor/OpenCode 自身 MCP 的 Enable/Disable 视为纯配置 patch（与仓库软链同类，跳过 CLI/L3 门闩）。Scanner 堵住借用 MCP 的 owner 启停、借用 Skill 拆源链。Grok Plugin 写 `[plugins] disabled/enabled`；自身 MCP 只翻 leaf `enabled`。前端不新增 action kind，只吃 capability。

**Tech Stack:** Rust（Tauri sidecar）`portable_actions` / `portable_inventory` / `config_patch`；React capability 驱动列表。测试：`./scripts/cc-partner-cargo.sh test --locked --lib …`；前端 `cd web && npm test -- <file>`。

**Spec:** `docs/superpowers/specs/2026-08-22-agent-hub-independent-toggle-design.md`  
**调研:** `docs/development/agent-hub-per-agent-toggle-report.md`

---

## 文件地图

| 文件 | 职责 |
|------|------|
| `src-tauri/src/agent_hub/portable_actions/targets/mod.rs` | `is_file_only_viewing_toggle` + 扩 `supports_direct_local_action` |
| `src-tauri/src/agent_hub/portable_actions/models.rs` | 不把 Enable 全局 bypass CLI；planner 用新判定 |
| `src-tauri/src/agent_hub/portable_actions/planner.rs` | file-only viewing 跳过 `MUTATION_BLOCKED` / `CLI_NOT_INSTALLED` |
| `src-tauri/src/agent_hub/portable_inventory/scanner.rs` | MCP 借用禁启停卸载；Skill `can_detach` 收口 |
| `src-tauri/src/agent_hub/portable_actions/targets/opencode.rs` | Grok plugin toml + 四家 MCP enabled patch（Grok/Gemini/Cursor/OpenCode 共用此 executor） |
| `src-tauri/src/agent_hub/portable_inventory/plugin_enablement.rs` | 已有 Grok 读语义；执行器必须对齐 |
| `web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.ts` | 若前端另有 detach 泄漏，与后端一致；通常只跟 capability |
| `docs/development/adapt-new-agent.md` | 文件-only viewing 例外写入 §3.9 / 硬规则 4 |

---

### Task 1: `is_file_only_viewing_toggle` allowlist

**Files:**
- Modify: `src-tauri/src/agent_hub/portable_actions/targets/mod.rs`
- Test: 同文件 `#[cfg(test)]` 模块（若无则在文件末加 `mod tests`）

- [ ] **Step 1: 写失败测试**

在 `targets/mod.rs` 测试模块加入：

```rust
#[test]
fn file_only_viewing_toggle_is_grok_plugin_and_uncertified_native_mcp() {
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
    use crate::agent_hub::portable_inventory::PortableAssetKind;
    use super::{is_file_only_viewing_toggle, supports_direct_local_action};

    assert!(is_file_only_viewing_toggle(
        AgentTarget::Grok,
        PortableAssetKind::Plugin,
        PortableAssetActionKind::Disable,
    ));
    assert!(is_file_only_viewing_toggle(
        AgentTarget::Gemini,
        PortableAssetKind::Mcp,
        PortableAssetActionKind::Enable,
    ));
    assert!(!is_file_only_viewing_toggle(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        PortableAssetActionKind::Disable,
    ));
    assert!(!is_file_only_viewing_toggle(
        AgentTarget::Grok,
        PortableAssetKind::Mcp,
        PortableAssetActionKind::Uninstall,
    ));
    assert!(supports_direct_local_action(
        AgentTarget::Grok,
        PortableAssetKind::Plugin,
        PortableAssetActionKind::Disable,
    ));
    assert!(!supports_direct_local_action(
        AgentTarget::Grok,
        PortableAssetKind::Plugin,
        PortableAssetActionKind::Uninstall,
    ));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib file_only_viewing_toggle_is_grok_plugin -- --nocapture`  
Expected: compile fail（函数不存在）或 `assert!(supports_direct_local_action(Grok, Plugin, Disable))` 失败。

- [ ] **Step 3: 最小实现**

在 `supports_direct_local_action` 之前加入（中文 docstring 按 AGENTS 模板）：

```rust
pub fn is_file_only_viewing_toggle(
    target: AgentTarget,
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> bool {
    if !matches!(
        action,
        PortableAssetActionKind::Enable | PortableAssetActionKind::Disable
    ) {
        return false;
    }
    match (target, kind) {
        (AgentTarget::Grok, PortableAssetKind::Plugin) => true,
        (
            AgentTarget::Grok
            | AgentTarget::Gemini
            | AgentTarget::Cursor
            | AgentTarget::OpenCode,
            PortableAssetKind::Mcp,
        ) => true,
        _ => false,
    }
}
```

`supports_direct_local_action` 在 store 分支之后、Claude/Codex 匹配之前插入：

```rust
    if is_file_only_viewing_toggle(target, kind, action) {
        return true;
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib file_only_viewing_toggle_is_grok_plugin -- --nocapture`  
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/portable_actions/targets/mod.rs
git commit -m "feat(agent-hub): allow file-only viewing toggles for Grok plugin and native MCP"
```

---

### Task 2: Planner 对 file-only viewing 跳过 CLI 门闩

**Files:**
- Modify: `src-tauri/src/agent_hub/portable_actions/planner.rs`（`apply_target_cli_gates` 附近，约 359 行）
- Test: `src-tauri/src/agent_hub/portable_actions/planner.rs` 已有测试模块

- [ ] **Step 1: 写失败测试**

找现有构造 `preview` 请求的 helper（同文件 tests）。新增：

```rust
#[test]
fn grok_plugin_disable_not_blocked_when_cli_mutation_is_blocked() {
    // 构造 Grok plugin item：actual_enabled=true, origin native or compatibility,
    // capabilities.can_disable=true（或让 scanner 路径走 supports_direct_local_action）。
    // target DTO：installed=false, mutation_capability=Blocked, reason=cli_version_unknown。
    // preview Disable。
    // assert blocking_reasons 不含 PORTABLE_ASSET_ACTION_MUTATION_BLOCKED
    // assert 不含 PORTABLE_ASSET_ACTION_CLI_NOT_INSTALLED
}

#[test]
fn claude_plugin_disable_still_blocked_when_cli_unknown() {
    // 同样 Blocked target DTO，Claude plugin Disable
    // assert blocking_reasons 含 PORTABLE_ASSET_ACTION_MUTATION_BLOCKED 或 CLI_NOT_INSTALLED
}
```

对照邻近测试抄 `item` / `target_dto` fixture，不要新造 DTO 字段。

- [ ] **Step 2: 跑测试确认失败**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_plugin_disable_not_blocked_when_cli -- --nocapture`  
Expected: FAIL（Grok 仍被 MUTATION_BLOCKED）

- [ ] **Step 3: 改 planner**

把

```rust
    let apply_target_cli_gates = !(request.action.bypasses_target_cli_gates()
        || (borrowed && target_dto.map(|t| t.target) != Some(mutation_target)));
```

改为（`mutation_target` 已是 viewing 或 owner；file-only 用 **item.target** 即 viewing）：

```rust
    let file_only = crate::agent_hub::portable_actions::targets::is_file_only_viewing_toggle(
        item.target,
        item.kind,
        request.action,
    );
    let apply_target_cli_gates = !(request.action.bypasses_target_cli_gates()
        || file_only
        || (borrowed && target_dto.map(|t| t.target) != Some(mutation_target)));
```

不要把 `Enable` 放进 `bypasses_target_cli_gates()`。

- [ ] **Step 4: 跑测试**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_plugin_disable_not_blocked_when_cli claude_plugin_disable_still_blocked -- --nocapture`  
若 cargo 只接受一个 filter：跑两次。Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/portable_actions/planner.rs
git commit -m "feat(agent-hub): skip CLI mutation gates for file-only viewing toggles"
```

---

### Task 3: 借用 MCP 禁止启停卸载；借用 Skill 禁止拆源链

**Files:**
- Modify: `src-tauri/src/agent_hub/portable_inventory/scanner.rs` `item_capabilities`
- Test: 同文件 tests；以及 `src-tauri/src/agent_hub/targets/grok.rs` 里现有 Grok 借用 plugin 测试（不要改 plugin 的 `can_uninstall` 期望，除非本任务明确要改——**不要改 plugin uninstall**）

- [ ] **Step 1: 写失败测试**

在 `scanner.rs` tests：

```rust
#[test]
fn borrowed_mcp_exposes_no_owner_toggles() {
    let caps = item_capabilities(
        AgentTarget::Claude, // enablement_target owner
        AgentTarget::Claude,
        PortableAssetKind::Mcp,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        true, // borrowed
        PortableOriginKind::Compatibility,
        false,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Mcp,
    );
    assert!(!caps.can_enable);
    assert!(!caps.can_disable);
    assert!(!caps.can_uninstall);
    assert_eq!(caps.reason_code.as_deref(), Some("borrowed_runtime_origin"));
}

#[test]
fn borrowed_store_skill_via_other_path_cannot_detach() {
    let store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    let caps = item_capabilities(
        AgentTarget::Grok,
        AgentTarget::Grok,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        true,
        PortableOriginKind::Compatibility,
        false,
        &store,
        PortableAssetKind::Skill,
    );
    assert!(!caps.can_detach);
    assert!(!caps.can_attach);
}
```

若现有 `grok_borrowed_store_skill_can_detach_source_but_not_attach_or_migrate`（或同名）期望 `can_detach == true`，本任务改为 `can_detach == false`，并改测试名/注释为「不得拆源链」。

- [ ] **Step 2: 跑测试确认失败**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib borrowed_mcp_exposes_no_owner_toggles -- --nocapture`  
Expected: FAIL（当前借用 MCP 会跟 owner allowlist）

- [ ] **Step 3: 改 `item_capabilities`**

在计算 `can_toggle_enable` 之后：

```rust
    let mut can_toggle_enable = !store_kind && can_enable_mutation && enable_semantics && actual_enabled.is_some();
    let mut can_toggle_disable = !store_kind && can_disable_mutation && enable_semantics && actual_enabled.is_some();
    if borrowed && kind == PortableAssetKind::Mcp {
        can_toggle_enable = false;
        can_toggle_disable = false;
    }
```

`can_uninstall` 在 borrowed MCP 时强制 false：

```rust
        can_uninstall: !store_kind
            && can_uninstall_mutation
            && !(borrowed && kind == PortableAssetKind::Mcp)
            && supports_direct_local_action(
                uninstall_target,
                kind,
                PortableAssetActionKind::Uninstall,
            ),
```

`can_detach`：把 `|| borrowed_store_runtime` 从「允许 detach」拿掉：

```rust
        can_detach: store_write && store.store_id.is_some() && store.store_attached && !borrowed_store_runtime,
```

（自己挂了链的 viewing：`store_attached && !borrowed_store_runtime`。）

- [ ] **Step 4: 跑相关 scanner / grok 测试**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib borrowed_mcp_exposes_no_owner_toggles -- --nocapture`  
Run: `./scripts/cc-partner-cargo.sh test --locked --lib borrowed_store_skill -- --nocapture`  
Expected: PASS；修复任何仍期望借用行 `can_detach` 的测试。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/portable_inventory/scanner.rs src-tauri/src/agent_hub/targets/grok.rs
git commit -m "fix(agent-hub): hide owner MCP toggles and source-symlink detach on borrowed rows"
```

---

### Task 4: Grok Plugin 写 `config.toml` `[plugins]` 数组

**Files:**
- Modify: `src-tauri/src/agent_hub/portable_actions/targets/opencode.rs`
- Modify: `src-tauri/src/agent_hub/portable_inventory/plugin_enablement.rs`（可抽 `set` 辅助，或执行器内联 toml_edit + 现有 patcher）
- Test: `opencode.rs` tests + 可选 `plugin_enablement.rs` 纯函数测数组变换

- [ ] **Step 1: 写失败测试（纯函数 + executor）**

先抽纯函数（测起来不碰磁盘策略也行，但 apply 必须落盘）：

```rust
#[test]
fn grok_plugin_disable_appends_disabled_without_touching_claude_settings() {
    let dir = tempfile::tempdir().unwrap();
    let grok = dir.path().join(".grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(
        grok.join("config.toml"),
        r#"
[plugins]
enabled = ["native-only"]
disabled = []
"#,
    )
    .unwrap();
    // 调 set_grok_plugin_enabled(&grok.join("config.toml"), "superpowers@claude-plugins-official", false)
    // 读回：disabled 含该 id；enabled 仍只有 native-only
}

#[test]
fn grok_plugin_enable_removes_from_disabled_and_skips_whitelist_for_borrowed() {
    // disabled = ["ecc"]；Enable ecc（当借用，不要写入 enabled 白名单）
    // 结果：disabled 空；enabled 若原先没有 ecc 则仍没有
}
```

Executor 集成：`OpenCodeTargetExecutor.execute_change`，`change.target=Grok`，`kind=Plugin`，`action=Disable`，`pre_item.native_id` 设好，`ctx.action=Disable`。用真实文件系统 temp 的 GROK 配置根。需要看 `TargetActionContext` 如何注入路径——若 executor 从 `TargetEnvironment::from_process` 读 home，测试应 `execute` 里用的 roots 函数改为可读 `change.path` 旁的 config，或给 executor 测 `set_grok_plugin_enabled` 而非整颗 execute_change。

优先测 `set_grok_plugin_enabled_in_config(path, id, enabled)` 纯文件函数，execute_change 只做路由。

- [ ] **Step 2: 跑测试确认失败**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_plugin_disable_appends_disabled -- --nocapture`  
Expected: FAIL / compile fail

- [ ] **Step 3: 实现**

`set_grok_plugin_enabled_in_config`：

1. 读 toml；没有 `[plugins]` 则创建。  
2. Disable：`disabled` 数组合入 id（`plugin_cli_selector` 结果优先，否则 native_id）；从 `enabled` 移除该 id。  
3. Enable：从 `disabled` 移除；**仅当调用方传入 `native=true` 且当前 `enabled` 非空** 才 `enabled` 合入 id。Executor 用 `pre_item.origin_kind == Native && owned_by` 是 Grok 判断 native。  
4. `apply_config_patch_atomically` patch `["plugins","disabled"]` 与必要时 `["plugins","enabled"]` 为 JSON 数组。

`OpenCodeTargetExecutor::execute_change` 在 store 分支之后、fail-closed 之前：

```rust
        if matches!(
            (change.target, change.kind, ctx.action),
            (
                AgentTarget::Grok,
                PortableAssetKind::Plugin,
                PortableAssetActionKind::Enable | PortableAssetActionKind::Disable
            )
        ) {
            let config = grok_config_toml_path(pre_item, change);
            let native = pre_item.is_some_and(|i| {
                i.origin_kind == crate::agent_hub::targets::portable::PortableOriginKind::Native
            });
            return set_grok_plugin_enabled_in_config(
                &config,
                &native_id(change, pre_item),
                native,
                matches!(ctx.action, PortableAssetActionKind::Enable),
            );
        }
```

`grok_config_toml_path`：user → `TargetPathResolver::resolve_all(&from_process()).grok.config_root.join("config.toml")`；测试通过 `change.path` 若指向 config 则用之。为可测性：若 `pre_item.source_path` 在 `.grok/` 下，用该 config_root。

禁止 `Command::new("grok")`。

- [ ] **Step 4: 跑测试 + 现有 grok plugin 扫描测试**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_plugin -- --nocapture`  
Expected: PASS（含「Claude enabledPlugins=false 不得关掉 Grok」）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/portable_actions/targets/opencode.rs
git commit -m "feat(agent-hub): toggle Grok plugins via config.toml arrays"
```

---

### Task 5: 自身 MCP `enabled` 翻转到 Grok / Gemini / Cursor / OpenCode

**Files:**
- Modify: `src-tauri/src/agent_hub/portable_actions/targets/opencode.rs`
- Reuse: `src-tauri/src/agent_hub/portable_actions/targets/codex.rs` 的 `set_codex_mcp_enabled_flag` 模式；JSONC 用 `JsoncConfigPatcher`（Claude MCP 已用）

- [ ] **Step 1: 写失败测试（每家一个最小 toml/json fixture）**

```rust
#[test]
fn grok_native_mcp_disable_sets_enabled_false() {
    // ~/.grok/config.toml [mcp_servers.good-api] 无 enabled
    // Disable → enabled = false；其它键保留
}

#[test]
fn gemini_native_mcp_disable_sets_enabled_false() {
    // settings.json { "mcpServers": { "g": { "command": "x" } } }
    // Disable → "enabled": false
}

#[test]
fn borrowed_mcp_execute_stays_blocked_on_grok_executor() {
    // change.target=Grok, kind=Mcp, origin compatibility → 仍 WRITE_NOT_CERTIFIED
    // （scanner 已不亮按钮；执行器双门闩）
}
```

Cursor / OpenCode 各一条 Disable JSONC，路径分别为 `mcp.json` 与 `opencode.json` 的 `mcpServers`。

- [ ] **Step 2: 跑测试确认失败**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_native_mcp_disable_sets_enabled_false -- --nocapture`  
Expected: FAIL

- [ ] **Step 3: 实现**

抽出：

```rust
fn set_mcp_enabled_toml(config_path, table_key /* "mcp_servers" */, id, enabled)
fn set_mcp_enabled_jsonc(config_path, object_key /* "mcpServers" */, id, enabled)
```

Toml 路径：`[table_key, id, "enabled"]`，复制 Codex `set_codex_mcp_enabled_flag`。  
JSONC 路径：`[object_key, id, "enabled"]`，`JsoncConfigPatcher`。

`execute_change`：当 `is_file_only_viewing_toggle(change.target, change.kind, ctx.action) && kind==Mcp`：

- 若 `pre_item` 是 borrowed / compatibility → 返回 `Blocked { PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED }`  
- 否则按 target 选文件：Grok `config.toml`；Gemini `settings.json`；Cursor `mcp.json`；OpenCode 已有 `scan_mcp_file` 的候选（`opencode.json` / `jsonc`，取 `pre_item.source_path`）。

Disable 不得 `value: None` 删 leaf。

- [ ] **Step 4: 跑测试**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib grok_native_mcp_disable gemini_native_mcp_disable borrowed_mcp_execute -- --nocapture`  
（filter 受限就逐个跑）Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/portable_actions/targets/opencode.rs
git commit -m "feat(agent-hub): toggle native MCP enabled flags for Grok Gemini Cursor OpenCode"
```

---

### Task 6: 前端与文档对齐（无新动作 kind）

**Files:**
- Modify: `web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.ts` 注释（借用 MCP 不再 enable/disable）
- Test: `web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts`
- Modify: `docs/development/adapt-new-agent.md` 硬规则 4 + §3.9：file-only viewing 与仓库软链同类例外
- Modify: `docs/development/agent-hub-per-agent-toggle-report.md` 文末链到本 spec（可选）

- [ ] **Step 1: 写失败测试**

若前端 `canOfferPortableDetach` 仍对 borrowed loadedViaOtherPath 为 true，加：

```ts
test('borrowed store skill loaded via other path does not offer detach', () => {
  const item = makeItem({
    kind: 'skill',
    originKind: 'compatibility',
    ownedBy: 'claude',
    target: 'grok',
    store: { storeId: 'skill:x', storeAttached: false, loadedViaOtherPath: true, loadedViaTarget: 'claude' },
    capabilities: { ...baseCapabilities, canDetach: false },
  });
  expect(canOfferPortableDetach(item)).toBe(false);
});
```

MCP 借用：`resolvePortableRowActions` 在 `canEnable/canDisable/canUninstall` 全 false 时返回 `[]`（已有逻辑）。加一条 fixture 锁定。

- [ ] **Step 2: 跑测试**

Run: `cd web && npm test -- portableInventoryPresentation.test.ts`  
Expected: 若 capability 已是 false，测试应 PASS；若前端另有覆盖，先红后改。

- [ ] **Step 3: 更新 adapt-new-agent.md**

在硬规则 4 的例外列表增加：

「Grok Plugin 的 `[plugins] enabled/disabled` 与 Grok/Gemini/Cursor/OpenCode **自身** MCP 的 `enabled` 字段是 viewing 配置 patch，不 spawn CLI，不要求 L3 activatePackage。借用 MCP 不得 Enable/Disable/Uninstall。」

§3.9 表格 Grok 行：Hub **可以**写 `disabled`/`enabled` 数组。

- [ ] **Step 4: Commit**

```bash
git add web/src/pages/AgentHub/portableAssets docs/development/adapt-new-agent.md
git commit -m "docs(agent-hub): document file-only viewing toggles; lock borrowed MCP UI"
```

---

### Task 7: 回归

- [ ] **Step 1: Rust**

Run: `./scripts/cc-partner-cargo.sh test --locked --lib portable_inventory portable_actions plugin_enablement -- --nocapture`  
若 filter 太宽导致超时，至少跑：`file_only_viewing_toggle`、`grok_plugin`、`borrowed_mcp`、`scan_only_manifest_cannot_be_promoted`。

- [ ] **Step 2: 前端**

Run: `cd web && npm test -- portableInventoryPresentation.test.ts PortableInventoryRow.test.ts usePortableInventoryController.test.ts`

- [ ] **Step 3: 手工（Dev 包）**

`./start.sh` → Agent Hub → Grok → Plugin：借用 Claude 包应有启用/禁用；点禁用后 `~/.grok/config.toml` 的 `disabled` 含该 id，`~/.claude/settings.json` 不变。Grok → MCP：自身 server 可关；列表里的 Claude MCP 无按钮。

---

## 明确不做（本计划外）

- 借用 MCP 的 Grok 侧黑名单  
- 借用 Skill 逐条 ignore  
- OpenCode/Pi 整根 env 门闩的 Hub 写入（除非后续证实有配置文件落点）  
- Cursor/Gemini/Pi 列出 Claude plugin  
- Plugin/MCP 进 portable-store  
- 把 Grok `activatePackage` 改成 support-manifest `supported`

## Spec 覆盖

| Spec 决策 | 任务 |
|-----------|------|
| `is_file_only_viewing_toggle` | Task 1 |
| planner 跳过 CLI 门闩 | Task 2 |
| 借用 MCP 无启停卸载 | Task 3 |
| 借用 Skill 不拆源链 | Task 3 |
| Grok Plugin toml | Task 4 |
| 自身 MCP enabled | Task 5 |
| 文案/手册 | Task 6 |
| 根级门闩 | **不做**（Open Questions：env-only 不写） |
| 不 remap CLI | Task 4/5 禁止 spawn |
