# Workbench 按 Git remote 合并项目列表

## 问题

同一 Git 仓库在本机与局域网设备上各占一条工作台项目，侧栏重复、设备筛选和工作区入口分裂。用户希望刷新时按仓库合并为一项，并在工作区标题下切换设备/路径。

## 决策

- 身份：canonical Git remote fingerprint（`host/owner/repo`，SSH 与 HTTPS 相同即同一仓库，不限 GitHub；GitHub SSH-over-443 主机 `ssh.github.com` 映射为 `github.com`）。无 remote 的目录永不互并。Hub 仍用旧的 URL 规范化，互不影响。
- 存储：不合并 `workbench_projects` 行。新增可空列 `git_remote_fingerprint`。终端/占用/新窗口仍用原 `projectId`。
- 刷新按钮：扫描可达项目的 origin 并写回 fingerprint，再按该字段分组。离线或读失败保留上次 fingerprint。
- 添加本机/打开远端：顺带写入该行 fingerprint；能匹配已有组则立即并入。
- 点列表项：打开该组上次使用的成员（设备筛选开启时，取该设备上上次使用的成员）。
- 工作区标题下「设备 · 路径」在组成员 ≥2 时变为下拉，切换即 `selectProject`。
- 列表项：仓库名 + 上次路径，并提示还有其他设备。
- 设备筛选：只收窄「哪些组出现」，不把一项拆回设备行。
- 删除：弹出勾选，按成员行移除（不删磁盘）。
- Git 历史「同步」：推送当前仓库主工作区到 origin，再 pull 其他设备上同一 fingerprint 的主工作区。

## 非目标

- 不新建仓库实体/子表。
- 不在每次进入页面时全表扫 git（仅刷新按钮 + 添加/打开单行）。
- 不改 P2P 鉴权边界。
