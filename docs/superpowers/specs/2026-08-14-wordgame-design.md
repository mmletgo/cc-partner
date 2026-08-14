# 记单词 / Game Hub 设计

日期：2026-08-14  
状态：已批准，进入实现

## 1. 问题

用户在 Workbench 里长期用英语和 agent 对话，希望把这些输出里有背诵价值的词沉淀下来，用艾宾浩斯闪卡复习。入口必须轻：不占主导航，从版本号旁打开。

## 2. 范围

第一期只做游戏大厅壳 + 记单词。不包含语音、手动加词、词库同步、其它游戏、Prompt 优化器 / Orchestrator 输出。

## 3. 采集

- 只采 Workbench 本机 / 远端终端里 Claude、Codex、OpenCode 的 **assistant 正文**。
- 不读 PTY `workbench:terminal-output` 碎片。
- 在会话所属设备上解析 provider 落盘（Claude jsonl / Codex rollout / OpenCode db），按 `(device_id, provider, session_id, record_id)` 水位增量。
- 清洗后只保留「真单词」：去掉 fence/inline code、URL、路径、camelCase / snake_case、含数字、短全大写缩写；小写后 lemma 合并；停用词不入库；必须命中嵌入的英语 lemma 允许表。
- 远端 owner 只回传 `{lemma,count}[]` + 新水位，写入**玩游戏这台机器**的 SQLite。
- 启动补扫历史。第一期词库不同步、不云备份。

## 4. 入口与浮层

- 无新路由，不进主导航。卫星窗无 footer，也就没有 Game。
- AppShell footer 版本号同一行右侧文字按钮 `game`（中英都叫 game）。
- 共享 `Dialog` 两态：大厅 → 记单词。
- 大厅：Escape / 点遮罩关闭。
- 游戏中：点遮罩不退出；Escape /「返回」回大厅。

## 5. 调度与题型

七种题型（固定顺序）：

1. `enToZh` 英→中（选择）
2. `zhToEn` 中→英（填空）
3. `chooseGloss` 选义（选择）
4. `cloze` 语境填空（填空）
5. `synonym` 近义词（选择）
6. `collocation` 搭配（选择）
7. `errorCorrection` 例句改错（填空）

规则：

- 熟悉：每种 `correct_count >= 2`（共 14）。答错只清当前题型计数；若因此不再满足，取消 familiar，当日可再出。
- 生词按天到期（入库当日即可出）。熟悉后间隔 1 / 2 / 4 / 7 / 15 / 30 天；同一词同一天最多晋级一档。
- 某词某题型当天累计答对 2 次后，该题型当天不再出。生词无总次数上限。
- 一次一题。队列：到期 → `total_count DESC` → `lemma ASC` → 题型顺序。
- 连续出题不得立刻再出刚出过的「同一词同一题型」；仅当天只剩这一张可出卡时才允许重复。
- `zhToEn` 题面不得展示 lemma 标题（答案即原词）；其它题型仍可用 lemma 作标题。
- 判题在 Rust：选择精确匹配；填空 / 改错大小写不敏感，忽略首尾空白与常见标点。无语音。

## 6. 出题与预热

- 一词一次内部 Claude structured JSON（`run_structured_json` + `resolve_internal_provider_config_dir` + Settings AI 的 CLI / model），cwd 为空（`--bare`），超时 180s，schema 要求 7 题齐全。
- 启动预热词频最高的 10 个未熟悉生词。严格满 10 个生词且 7 题缓存齐才能进游戏。
- 生成失败必须重试成功，队列堵在该词；不换模型。大厅展示原因 +「重试预热」。
- 开玩后缓存始终超前 10 个未出题生词。第一期不刷新已缓存题目。

## 7. 数据

本机 SQLite（`CREATE TABLE IF NOT EXISTS`，禁止 `sqlx::migrate!`）：

- `wordgame_lemmas`
- `wordgame_type_progress`
- `wordgame_cards`
- `wordgame_ingest_cursor`
- `wordgame_preheat`

写入走 `DatabaseMaintenanceGate` / `with_shared_write_lease`。词库权威在玩游戏的这台机器上。

## 8. 失败

- CLI 未就绪 / 超时 / JSON 不合 schema：记单词禁用，文案写清原因，可重试。
- 词库 < 10 或预热堵住：同样禁用，不进空游戏。
- 远端抽取失败：本机预热照常；大厅次要提示「有一台远端未计入词频」。capability 缺失视为 unsupported，不当空成功。

## 9. 非目标

其它游戏、新路由、卫星窗 Game、语音、手动加词、词库 P2P/云同步、独立 Tauri 透明窗。
