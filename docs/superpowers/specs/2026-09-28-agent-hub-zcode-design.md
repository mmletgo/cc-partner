# Agent Hub 接入 ZCode

日期：2026-09-28。身份 wire：`zcode`。显示名：ZCode。

本轮只做 Agent Hub 管理。工作台启动、会话搜索、Prompt 历史、用量、headless 优化器缺席。

## 身份

| 决策 | 值 |
|------|----|
| `AgentId` / `AgentTarget` | `Zcode` / `zcode` |
| CLI | `zcode`。只探测这个可执行文件，不启动 `ZCode.app` |
| 配置根 | `ZCODE_DATA_BASE_DIR` 非空时为 `<base>/.zcode`，否则 `~/.zcode`。不认 `ZCODE_HOME` |
| Hub | 进入，且必须进入 `all_hub_targets()` |
| Runtime / 会话搜索 / Prompt 历史 | `None` |
| 用量 / headless | `false` |
| 无图形剪贴板贴图 | `atFileMention`。这是未识别命令的既有回退，不是 ZCode 官方贴图语法。官方入口是剪贴板 `Ctrl+V`、`/paste-image` 和 headless `--attach`。本轮不改贴图注入 |

## 指令

ZCode 只注入两份指令：用户全局 `<config_root>/AGENTS.md`，以及仓库根 `AGENTS.md`。它不读 rules 目录，也不持续读 `CLAUDE.md`。

- 公共槽继续复用仓库根 `AGENTS.md`。ZCode 适配器不得另写一份，也不得把 `CLAUDE.md` 列成 ZCode 会加载的项目文件。
- 项目级适配槽、独有槽在 Hub 可见。`renderInstruction` 保持 `blocked`。渲染不得指向仓库根 `AGENTS.md` 或 `~/.claude`。
- 用户级原生指令文件是 `<config_root>/AGENTS.md`。用户级镜像可以写这个白名单文件。
- 禁止读写 `~/.zcode/v2/**`，禁止读取或回显 provider、model、API key。

## 可移植资产

扫描可以 `readOnly`。没有 L3 验收记录，因此 `activatePackage` / `deactivatePackage` 保持 `blocked`。不得 spawn `zcode` 做写操作。store 的 attach / detach / migrate / destroy 对 ZCode 保持 blocked。确认当前版本和逃逸软链恢复沿用现有账本/复制合同，不得因为 mutation blocked 被拒绝。

| 资产 | 路径 | 写 |
|------|------|----|
| Skill native | `<config_root>/skills`、`<project>/.zcode/skills` | 本轮不写 native 根 |
| Skill 借用 | `~/.agents/skills`、`<project>/.agents/skills`。`originKind=compatibility`，`ownedBy=sharedAgents` | 不改借用目录 |
| Command native | `<config_root>/commands`、`<project>/.zcode/commands` | 本轮不写 native 根 |
| MCP native | `<config_root>/cli/config.json` 与 `<project>/.zcode/config.json` 的 `mcp.servers` | 只改这个对象，保留文件里其他键 |
| MCP 借用 | 同一 scope 的 `.agents/mcp.json` 仅在该 scope 的 `.zcode` 配置没有 `mcp.servers` 时作为 compatibility 出现 | 借用项不可启停、不可卸载 |
| Plugin | `<config_root>/cli/plugins/installed_plugins.json`。`plugins` 是数组，元素含 `id`、`installPath`、`scope`。清单优先 `.zcode-plugin/plugin.json`，其次 `.claude-plugin/plugin.json` | 启停只改 `cli/config.json` 的 `plugins.enabledPlugins`（`id@marketplace` → bool）。未登记的已安装包视为开。安装与卸载 blocked |

插件内部的 Skill / Command / MCP 只随插件整包出现，不进各自类别的独立清单。

不扫描 `~/.claude`、`~/.codex`，也不把 Claude marketplace 列成 ZCode 会加载的包。ZCode 的开关不得继承 Claude `enabledPlugins`。

## 验证

- 身份表：`zcode` 在 Hub 列表，不在 runtime / session / history 列表。
- 适配器：公共槽不写仓库 `AGENTS.md`，不写 `~/.claude`；用户指令路径是 `<config_root>/AGENTS.md`。
- 插件开关：Claude `enabledPlugins=false` 时 ZCode 仍按自己的表；自己的表为 false 时显示关。
- `ZCODE_DATA_BASE_DIR` 指向临时目录时配置根是 `<base>/.zcode`。
- 前端 `tsc` 能通过新增的 `AgentTarget` 键。定向跑身份表、ZCode 适配器和插件开关测试。
