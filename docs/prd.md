# cc-partner - 产品需求文档 (PRD)

## 1. 产品概述

cc-partner 是一款支持 Mac/Windows/Ubuntu 三端的桌面工具，设计用于 Claude Code 用户在局域网环境下的多设备协作。

### 1.1 目标用户
使用 Claude Code 进行开发的程序员，拥有多台局域网设备。

### 1.2 核心价值
- 在多台设备间快速传输文件
- 一键区域截图并粘贴到 Claude Code
- 集中管理常用 Prompt，跨设备同步
- 使用多页面速记本记录临时文本，并在局域网与 GitHub 间同步
- 在项目文件夹维度管理 Git worktree、多个普通终端 window/pane，并直接操作当前工作区文件树

## 2. 功能需求

### 2.1 局域网文件传输

**描述**：在局域网内的多个设备间互传文件。

**功能点**：
- 选择在线设备作为传输目标
- 支持任意大小文件传输
- 分块传输（1MB/块），显示传输进度
- 断点续传：传输中断后可从已完成的位置继续
- SHA256 校验确保文件完整性
- 支持拖拽文件到应用窗口发起传输
- 文件接收后保存到用户配置的目录

### 2.2 区域截图

**描述**：用户触发截图后，框选屏幕区域，截图自动保存到系统剪贴板。

**功能点**：
- 通过快捷键或托盘菜单触发
- 全屏半透明遮罩覆盖
- 鼠标拖拽框选区域，选区内显示原始画面
- 松开鼠标后编辑工具条立即出现，可进行矩形/箭头标注；底图快照加载不应阻塞工具条显示，捕获前不应出现空白/透明闪烁帧
- 确认后截取区域图片
- 自动复制到系统剪贴板
- ESC 键取消截图
- 截图后用户可直接 Ctrl+V 粘贴到 Claude Code

### 2.3 Prompt 管理

**描述**：提供文本记录管理功能，支持标签分类和筛选。

**功能点**：
- 创建 Prompt：标题 + 内容 + 标签
- 编辑已有 Prompt
- 删除 Prompt（软删除，用于同步）
- 一键复制 Prompt 内容到剪贴板
- 标签管理：添加/移除标签
- 按标签筛选 Prompt 列表
- 文本搜索（搜索标题和内容）
- 按创建时间/更新时间排序
- Prompt 优化：用户输入原始编程任务 Prompt 后，调用本机 Claude Code CLI 的 pure/headless 模式生成中文优化版与等价英文优化版
- Prompt 优化结果只用于当前页面展示和复制，不保存历史、不入库、不跨设备同步、不做缓存
- Prompt 优化输出必须以需求方视角写成可直接粘贴给 Claude Code 的委托式 Prompt，不得生成“请确认/是否需要/请指定”等继续询问用户的澄清句；原始信息不足时只能写成待补充占位或执行假设，除非原始 Prompt 明确要求文档或文件输出，否则不得新增 `docs/`、写文件、持久化等确认要求
- Prompt 优化结果区以中文/英文双卡片展示，每张卡片内部包含标题与复制操作区、分隔线和只读文本内容区
- 工作台终端界面可通过工具栏或快捷键唤出 Prompt 优化浮层；浮层只显示一个原始 Prompt 输入框，不显示优化按钮、填入终端按钮、双语结果标题/结果区或关闭按钮；优化时应以当前项目根目录作为 Claude Code 工作目录，使其可读取项目 CLAUDE.md 上下文，并只按设置页选择的中文/英文语种生成一个优化版 Prompt；默认快捷键为轻按 Control 单键，首次触发打开浮层并聚焦原始 Prompt 输入框；浮层打开后再次触发快捷键时，如果输入框为空则直接关闭浮层，如果输入框非空则自动优化；输入框内非空时按 Enter 与再次触发快捷键等价，Shift+Enter 保留换行，输入法 composing 状态下的 Enter 不提交；后端把优化后的 Prompt 边生成边流式写入当前运行中的终端，完成后自动关闭浮层；填入只插入文本，不自动追加回车或执行命令

### 2.4 设备自动发现与互联

**描述**：局域网内的 cc-partner 实例自动发现彼此并建立连接。

**功能点**：
- 启动时自动注册 mDNS 服务
- 自动发现同一局域网内的其他实例
- 在设备页显示在线数量，并把自动发现的设备合并进连接目标列表（设备名、IP、在线状态）
- 设备上线/下线实时通知
- 每个实例同时作为 HTTP 服务端和客户端

### 2.5 Prompt 跨设备同步

**描述**：Prompt 数据在所有连接的设备间自动同步。

**功能点**：
- 新设备上线时自动拉取/推送 Prompt
- 本地修改后自动同步到对端（500ms 防抖）
- 定时同步（每 30 秒）
- 向量时钟追踪版本，避免丢失更新
- 并发冲突采用 Last-Writer-Wins 策略
- 仅同步 Prompt 数据，不同步文件

### 2.6 速记本

**描述**：提供多页面自动保存文本区域，用于快速记录临时想法、片段和待办。

**功能点**：
- 支持新增、切换、重命名、删除多个 Scratchpad 页面；新增入口位于左侧页面列表末尾
- 每个页面包含标题与正文内容，空标题保存为“未命名”
- 内容权威源为本机 SQLite，页面不再依赖 localStorage
- 用户编辑正文后自动保存，切换页面、删除页面、同步前先保存当前页待写内容
- 保留复制当前页正文和清空当前页操作
- 删除页面使用软删除传播，刷新或同步后不应复活
- 支持手动触发一次同步，同时执行局域网同步与 GitHub 云端同步，并纳入全局云端同步范围
- 旧版单页速记本内容升级后保留为标题“速记本”的第一页

### 2.7 SSH 连接目标管理

**描述**：以连接目标列表集中展示局域网设备和手动目标，并提供 SSH 连接配置管理，配置跨设备同步。

**功能点**：
- 在连接目标列表中合并局域网 mDNS 自动发现的设备（IP），并支持手动添加任意 IP
- 为每个连接目标配置用户名与端口（默认 22）
- 一键复制 ssh 连接命令（端口非 22 自动加 -p，用户名为空时省略）
- 连接目标配置（host/username/port/label）基于向量时钟跨设备同步
- 提供 mac/ubuntu/windows 三端开启 SSH 服务的配置指南
- 按本机操作系统展示对应的连接端（ssh 客户端）用法

### 2.8 健康提醒

**描述**：监测久坐行为，在长时间连续工作后提醒用户休息，降低健康风险。

**功能点**：
- 久坐监测：每分钟采样前台键鼠活跃度，推进工作/休息状态机
- 工作/休息状态机：Idle / Working / Resting 相位流转，连续工作达阈值触发久坐提醒
- 多通道提醒：健康监测启用后，久坐提醒默认弹全屏遮罩（每屏一层透明置顶遮罩，按钮关闭），系统通知由通知开关单独控制
- 喝水提醒：健康监测启用后始终启动，按可配置间隔（默认 1h）独立计时，到点弹喝水全屏遮罩；「已喝水」按钮记录一次喝水入 water_records
- 免打扰时段：可配置不弹通知的时间区间（支持跨午夜）
- 手动控制：开关监测、暂停/恢复、贪睡、跳过
- 开机自启：监测启用时注册系统开机自启（macOS LaunchAgent），禁用时移除
- macOS 权限引导：首次启动引导授权 Accessibility（键鼠采样所需）
- 健康提醒页：以状态概览、今日活跃指标、app 使用时长排行图表和 24 小时活跃分布图表展示监控控制台，头部配置入口跳转设置页健康提醒 tab
- 设置页健康提醒 tab：以「健康提醒 / 免打扰 / 通知与隐私」三个分栏目 Card 展示配置表单
- 完整配置表单：健康监测总开关、工作窗口/休息时长、喝水提醒间隔、通知开关、记录窗口标题、免打扰起止 24 小时制时间选择器、数据保留天数；久坐提醒、喝水提醒和全屏遮罩不提供独立开关，均随健康监测启用

### 2.9 GitHub 周热门项目

**描述**：「Github热门」菜单页展示 GitHub 周热门项目，并可选使用本机 Claude CLI 生成中英文项目解说。

**功能点**：
- 抓取 GitHub Trending Weekly 全语言项目列表，展示仓库名称、简介、语言、星标、fork 与本周新增星标
- Github热门页项目卡片在桌面宽屏使用双列瀑布流布局，奇偶排名分列以形成错落卡片流并避免按行 grid 留空，排名与标题同行展示，卡片内部按头部、简介、Claude 解说和指标区保留清晰间距，卡片自身高度不强制等高；窄屏设备自动回落为单列并保持原始榜单顺序
- 对榜单按天缓存，频繁打开 Github热门页时优先使用缓存，GitHub 刷新失败时可回退最近缓存
- 用户可配置是否启用 Claude CLI 解说、CLI 路径、模型与缓存有效期
- Claude CLI 解说失败时首页仍展示 GitHub 原始简介，并显示可诊断的失败原因
- 旧的泛化失败缓存不会永久阻挡修复后的解说生成，应用可在合理条件下重新尝试生成

### 2.10 Claude Code 资产管理

**描述**：集中查看本机 Claude Code skills、commands、plugins 与 MCP 配置，并从局域网设备选择性拉取。

**功能点**：
- 顶部按资产类型分别展示启用数量与警告数量，避免只给总数导致定位成本过高
- 页面内用「本机资产 / 局域网拉取」两个 tab 切换，默认打开本机资产
- 搜索框与类型筛选对本机资产 tab 和局域网拉取 tab 的远端资产列表均生效
- 本机资产与局域网远端资产列表在宽屏下两列显示，窄屏自动回到单列
- 局域网拉取 tab 可选择设备、加载远端资产、勾选后拉取
- 本机资产 tab 支持搜索、类型筛选、启用/关闭与卸载
- 页面不提供本机安装卡片，新增资产优先通过文件系统或局域网拉取路径完成

### 2.11 用户设置

**描述**：提供集中偏好设置入口，管理基础配置、权限、快捷键、工作台运行依赖、同步、Claude CLI/AI 能力和版本更新。

**功能点**：
- 常规设置包含设备名、文件接收目录、macOS 权限状态和截图快捷键
- 截图快捷键通过只读录制控件修改，保存时不应覆盖未修改的设备名或接收目录
- 依赖环境页签展示 Workbench tmux dependency manager 状态，支持检测、查看后端/版本/路径、查看安装命令预览、触发安装、取消安装和重新检测
- 常规 / 同步 / AI 页签的恢复默认按钮始终可点击；常规恢复为后端按当前设备环境生成的默认设备名、默认接收目录和平台默认截图快捷键，同步和 AI 分别恢复为后端定义的云端同步默认配置与 Claude CLI/AI 默认配置
- 同步、AI 和关于页签分别管理云端同步、Claude CLI/AI 能力和应用更新；AI 页签中的 CLI 路径与模型供 GitHub 项目解说和 Prompt 优化共用，启用开关与缓存时长仅作用于 GitHub 项目解说；AI 页签同时管理 Workbench Prompt 优化浮层快捷键与自动填入语言，默认轻按 Control、默认填入中文优化版；同步和 AI 的恢复默认只重置表单，仍需用户点击“应用配置”持久化

### 2.12 工作台

**描述**：以项目文件夹为中心管理 Git worktree、本机普通交互式终端、当前工作区文件夹、文件内容浏览/编辑和 Git 提交树。第一期支持本机目录及已挂载局域网目录；远端 cc-partner 设备上的原生远端项目和远端 PTY 后续扩展。

**功能点**：
- 工作台布局：项目文件夹列表紧跟全局左侧栏“设置”菜单项下方，作为进入工作台的入口，不再设置独立“工作台”主导航项；主区域依次展示工作台标题、terminal sessions 标识、worktree 管理层、依赖提示槽和中心工作区；中心工作区在当前 worktree 的终端层与文件 tab 工作区之间切换，预览文件时终端可以隐藏但 xterm DOM 必须保持挂载并停止接收输入；终端工具栏按钮与文件工作区工具栏保持一致，均显示图标和文字；文件预览按钮仅在已有打开文件 tab 时启用，点击后回到当前或首个文件 tab，与文件工作区的返回终端按钮对称；右侧检查器承载当前 window 状态，并提供当前 worktree 文件夹 / Git 提交树 tab，窄宽下排到首屏终端之后
- 添加项目文件夹：用户点击项目文件夹区右上角 `+` 后直接打开系统目录选择器；选择本机/已挂载目录后立即添加项目、进入该项目并加入最近项目列表
- 项目切换：左侧栏项目文件夹列表切换当前项目，中央 worktree/window 列表和右侧文件夹按当前项目刷新；每个项目卡片右下角显示已打开 terminal window 数与 pane 总数，而不是固定“进入工作台”文案；旧项目的异步请求结果不得覆盖新项目 UI
- Worktree 管理：每个项目默认有一个主 worktree，路径为用户添加的项目根目录；项目载入时必须读取 `git worktree list --porcelain`，把磁盘上已有的 Git worktree 同步进顶部 worktree 管理层，因此用户选择项目后应立即看到主工作区和已有 worktree 工作区；顶部 worktree 管理层只负责创建功能 worktree、切换 active worktree 和移除非主 worktree。创建 worktree 时点击“新建 worktree”先展开页面内表单，分支前缀从固定类型（如 `feature` / `fix`）中选择，用户只输入后缀，确认后组合为 `<prefix>/<suffix>` 并在应用数据目录下执行 `git worktree add -b <branch>`；如果同名本地分支已存在但已合入 base/HEAD，后端应先删除旧分支再重建，未合并则返回可操作错误；不得依赖 WebView 可能不可见的浏览器阻塞弹窗。移除 worktree 前必须先关闭该 worktree 下的 terminal window。第一期不做 Git diff 面板、交互式冲突解决和 PR 创建
- Git 提交树：右侧检查器提供“项目文件夹 / Git 历史”tab；Git 历史 tab 顶部展示当前 active worktree 的 clean/dirty/conflict 状态和 Commit、Push、Merge 操作，下面按 active worktree 查询 Git DAG，绘制类似 VS Code 的多 lane 分支/合并线，展示提交摘要、短 hash、作者和相对时间，并用 ref badge 区分本地分支、远端分支和 tag；commit 点击后不弹手写输入框，后端执行 `git add -A`、读取 staged diff/stat，并在 active worktree cwd 下用 Claude Code 项目上下文模式生成 commit message 后 `git commit -m`，无可提交改动时作为 no-op 返回最新 worktree 状态；push 优先复用当前分支 upstream 执行 `git push`，没有 upstream 时只选择 `origin` 执行 `git push -u origin <branch>`；完全没有 origin/upstream，或只有 `*-upstream` 等源码上游 remote 时返回配置提示；Git 历史工具条的 Push 按钮只在后端 status 判定当前 worktree 有可用推送目标时启用，本地未发布且没有 origin/upstream 的项目必须禁用；merge 非主 worktree 时先要求源 worktree clean，有未提交改动立即返回可读错误；源 clean 后后端自动关闭该 worktree 下所有 terminal window/pane，再检查主 worktree clean 并执行 `git merge --no-ff <branch>`；主 worktree dirty 时返回可读错误；merge 冲突时后端调用本机 Claude Code CLI（设置页 AI 的 CLI 路径和模型）在主 worktree cwd 下以项目上下文 headless 模式尝试生成冲突文件完整内容，解决成功后 stage all 并完成 merge commit，仍有冲突则尽量 `git merge --abort` 后返回错误；merge 成功后删除该 worktree session 元数据、Git worktree 本身和已合并本地分支；merge 命令返回 `{ok, worktreeId, stages}`，并通过 `workbench:merge-progress` 事件按 `checkSource/closeSessions/mergeMain/resolveConflicts/cleanup` 阶段推送 `pending|running|completed|failed|skipped` 状态，事件 payload 带 `projectId` 供前端过滤当前项目；前端阶段条在成功完成后自动隐藏释放 Git 历史空间，失败时保留错误阶段；切换项目/worktree、commit、push 或 merge 后刷新，commit 失败也应刷新当前 worktree 状态，空仓库或无提交时显示空态
- tmux 依赖管理：应用启动后自动检测 Workbench 所需 tmux；macOS/Linux 使用原生 tmux，Windows 使用默认 WSL 发行版内的 tmux。状态卡应展示 checking/ready/missing/installing/unsupported/failed 状态、后端、版本、路径、安装命令预览和最近输出；缺失且存在可用安装通道时用户可确认安装，安装完成或失败后可重新检测。tmux 不可用时仍允许普通 PTY fallback，但不承诺 window/pane 语义和重启后 shell 上下文恢复
- 终端窗口与 pane：每个 worktree 优先对应一个真实 tmux session，同一 worktree 内的前端 tab 对应 tmux window，window 内分屏对应 tmux pane；不同 worktree 的 window 必须处于互相独立的 tmux session，不能在 tmux 底部 status/window 列表中互相可见。tmux session 名应由项目名、worktree 显示名和 worktree id 短尾缀派生，不加固定 `cc-partner-worktree` 前缀；缺少显示名时才回退内部 id，避免 status bar 只显示无意义 hash，同时避免清洗后重名 worktree 碰撞。运行期 attach 必须在连接当前 worktree session 后切到对应 tmux window，切换 app tab 时必须让后端执行 tmux `select-window` 切到 tab 绑定 window，用户通过 tmux 底部 status bar 或快捷键切换 window 时顶部 app tab 也必须跟随当前 worktree 的真实 tmux current window，确保 app tab 与 tmux window 一一绑定；没有 tmux、WSL 路径不可转换或后端不可用时退回单普通 PTY window。新建 window 在 active worktree 根目录启动系统 shell（用户自行运行 `claude` 或其他命令），window 元数据持久化 `worktree_id` 与 `cwd`，重新打开 cc-partner 后应恢复之前打开的 window 列表并保持其所属 worktree
- 终端上下文恢复：macOS/Linux 环境优先使用原生 `tmux` 承载真实 shell 上下文，Windows 环境优先通过默认 WSL 发行版内的 `tmux` 承载上下文（盘符项目路径转换为 `/mnt/<drive>/...`，`\\wsl$\<distro>\...` / `\\wsl.localhost\<distro>\...` 转为发行版内 Linux 路径）；应用退出时只断开 attach，重启后重新 attach 到 worktree tmux session 的原 window；没有可恢复 window 时重建 window 并保留 tab 元数据
- 终端输出：PTY 输出必须保留中文与符号等 UTF-8 文本完整性；前端 xterm 必须按 PTY/tmux 原始控制序列渲染，不能启用会改写换行的 `convertEol`；终端面板 CSS 和 xterm theme 必须从 `--terminal-*` design token 读取，并随应用浅色/深色主题切换同步更新；tmux 底部 status bar 必须由 Workbench 覆盖为无硬编码暗色的本地样式：保留 `session ›`、`#I:#W` 标签结构和选中 window 的浅蓝块，inactive window、右侧时间和整条 status 背景必须使用 terminal default 颜色，不能在浅色主题下保留深色工具条、深色 inactive 标签、暗色时间或 underline 黑线；后端启动 Workbench PTY 客户端时必须显式设置 `TERM=xterm-256color` 和真彩色环境，不能继承 `TERM=dumb`；切换 worktree 或 app tab 时必须保留当前项目下所有 terminal window 的 xterm 实例，只隐藏非 active worktree/window，不能卸载后 replay 原始终端流，inactive 常驻 xterm 即使产生 `onData` 也必须丢弃，同时必须同步底层 tmux current window；前端应轻量同步 tmux current window 到顶部 app tab，不能因为保留 tmux status bar 而让两层 window 选中态分裂；Workbench 路由切出时终端输出缓存必须继续由 AppShell 生命周期的常驻 Provider 接收，切回后 xterm 从该缓存 replay，不能把 terminal output 监听和 buffer 只放在 Workbench 页面内部导致 Claude Code/tmux TUI 丢失屏幕态；buffer 截断导致必须 replay 历史输出时，不得把 xterm 生成的设备能力响应写回 PTY，且 replay gate 释放必须延后一轮 macrotask；活跃 xterm 已写入旧 buffer 后，如果常驻 buffer 达到上限并等长滑动截断，前端必须根据旧 buffer 后缀与新 buffer 前缀的重叠追加新尾部，不能只比较长度；创建终端时应使用当前终端可见 viewport 的真实 cols/rows 作为 PTY 初始尺寸，终端运行时 resize 也必须按同一可见 viewport 计算，避免交互式程序状态栏和命令行内容出现替换字符、首屏错位或内容超出工作台可视宽度
- 终端分屏：中央终端区每次 attach 当前 window；左右/上下分屏调用 tmux `split-window -c <window cwd>` 创建真实 pane，新 pane 必须从该 window 绑定的 worktree 根目录启动而不是继承当前 pane 中用户 `cd` 后的位置；关闭 pane 多 pane 时调用 tmux `kill-pane` 关闭当前 active pane，最后一个 pane 被关闭时应关闭所属 window 并同步移除顶部 tab，不应弹“只有一个 pane”的错误
- 会话状态：右侧只展示当前 window 状态，包含设备、项目名、worktree 名、工作区路径、window 名、命令、状态、运行时长、尺寸、开始时间和退出码
- 会话操作：支持聚焦、读取当前聚焦、重命名、关闭；聚焦 window 必须同步切换底层 tmux current window，读取当前聚焦必须只在当前 worktree 的 tmux session 内把 current window id 映射回 app sessionId；关闭/应用退出清理时如果底层进程已自然退出或被系统回收，应视为清理成功，不向用户展示 No such process 类 IO 错误；关闭 tab 会删除持久化 window 并销毁对应 tmux window，应用退出只清理当前 PTY attach，不能删除可恢复 window 元数据或销毁 worktree tmux session
- Prompt 优化浮层：终端工具栏提供 Prompt 优化入口，默认也可轻按 Control 唤起；浮层悬浮在当前终端输入光标附近，只渲染一个原始 Prompt 输入框，每次从关闭态重新打开时清空输入；首次快捷键触发打开并聚焦原始 Prompt 输入框；浮层打开后再次触发快捷键时，空输入直接关闭浮层，非空输入才自动优化；输入框内非空时按 Enter 与再次触发快捷键等价，Shift+Enter 保留换行，输入法 composing 状态下的 Enter 不提交；后端按设置语言把优化后的 Prompt 边生成边流式写入当前 running active session，完成后自动关闭浮层；优化请求绑定 active worktree 根目录以加载该工作区 CLAUDE.md 上下文，但输出仍必须是需求方视角的直接委托式 Prompt，不能把项目文档规则扩展成向用户确认 `docs/` 或写文件的澄清问题；小组件不显示“优化/填入终端/中文优化版/English optimized/关闭”等按钮或双语结果区，且不请求双语优化
- 项目文件夹：右侧文件树绑定 active worktree 根目录，支持刷新、展开/收起、选中文件/文件夹、新建文件、新建文件夹、重命名、删除确认、复制相对路径，并展示名称、类型、相对路径、大小、修改时间和父目录；点击文件节点在中心文件工作区打开 tab，预览 tab 标签直接显示文件相对路径，格式化/保存/返回终端按钮与 tab 同行显示，不再渲染第二行文件头部或类型标签；支持多文件 tab 激活/关闭，全部关闭后回到终端；文件 tab 未关闭时，终端工具栏的文件预览入口位于按钮组最右侧，用户可在终端和文件工作区之间双向切换；项目或 worktree 切换时清空文件工作区，旧异步请求不得污染新上下文；重命名/删除路径后已打开 tab 必须同步新路径或关闭
- 文件内容浏览/编辑：图片只读预览；CSV 只读表格预览；SQLite 只读枚举表并预览前 100 行，不执行用户 SQL；代码、Markdown、HTML、JSON、TOML、YAML 和普通文本走文件工作区编辑，代码编辑器需要高亮插件体验；Markdown 支持源码、预览和分栏模式，预览模式可直接编辑，体验接近 Typora；HTML 支持源码、sandbox 渲染预览和分栏模式，预览可加载 active worktree 根内相对 CSS、图片、字体和媒体资源，外链、data/blob、绝对路径、根外路径和跨根 symlink 不加载，外部/内联 CSS 的相对 `url()` 按 CSS 文件自身路径解析；JSON/TOML/YAML 提供格式化按钮，保存前必须做语义校验；保存文本文件使用 baseHash 乐观锁防止覆盖外部修改
- 当前仍不做 Git diff 面板、交互式冲突解决、会话日志持久回放、远端 cc-partner 原生项目和远端 PTY

## 3. 非功能需求

### 3.1 跨平台
- 支持 macOS、Windows、Ubuntu
- 使用 Tauri 打包为各平台独立桌面应用
- 应用启动后主窗口默认进入系统全屏显示

### 3.2 性能
- 文件传输速度应充分利用局域网带宽
- UI 操作不应因网络/IO 阻塞而卡顿（异步架构）
- 截图工具条响应时间 < 200ms，截图/标注合成不应阻塞工具条出现，选区框与工具条在快照捕获开始前应保持稳定可见

### 3.3 可靠性
- 文件传输支持断点续传
- 数据库使用 SQLite，数据持久化可靠
- 设备离线后重新上线，同步应能恢复

## 4. 技术架构

### 4.1 技术栈
- 桌面宿主：Tauri 2（Rust 主进程）
- 语言：Rust（后端）+ TypeScript（前端）
- 网络：axum（HTTP 服务端，跨设备 P2P）+ reqwest（peer client）
- 发现：mdns-sd（mDNS）
- 存储：SQLite + sqlx
- 抓屏/剪贴板：xcap + arboard
- 终端：portable-pty（工作台交互式 PTY attach）+ tmux（优先承载可恢复 window/pane 上下文）
- 通信：Tauri `invoke()` IPC（本地前端 ↔ Rust）
- 打包/更新：Tauri CLI + tauri-plugin-updater

### 4.2 数据模型

#### Prompt
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| title | str | 标题 |
| content | str | 内容 |
| tags | list[str] | 标签列表 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| device_id | str | 创建设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| deleted | bool | 软删除标记 |

#### Device
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 设备唯一 ID |
| name | str | 设备名称 |
| host | str | IP 地址 |
| port | int | HTTP 端口 |
| last_seen | datetime | 最后在线时间 |
| online | bool | 是否在线 |

#### SshTarget
| 字段 | 类型 | 说明 |
|------|------|------|
| host | str | 主键（IP/hostname） |
| port | int | 端口，默认 22 |
| username | str | 用户名（空串=用本机默认用户名） |
| label | str | 备注（可选） |
| device_id | str | 最后修改设备 ID |
| vector_clock | dict[str, int] | 向量时钟 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |
| deleted | bool | 软删除标记 |

#### WorkbenchProject
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| name | str | 项目显示名 |
| kind | str | 项目类型，第一期为 local（本机/已挂载目录） |
| device_id | str | 所属设备 ID |
| device_name | str | 所属设备名称 |
| path | str | canonical 项目根路径 |
| last_opened_at | datetime | 最近打开时间 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

#### WorkbenchWorktree
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID / deterministic main id) | 主键 |
| project_id | str | 所属 WorkbenchProject |
| name | str | worktree 显示名，默认取分支名 |
| branch | str? | Git 分支名 |
| base_branch | str? | 创建 worktree 时的基准分支/引用 |
| path | str | worktree canonical 根路径 |
| is_main | bool | 是否为项目主工作区 |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

#### WorkbenchSession
| 字段 | 类型 | 说明 |
|------|------|------|
| id | str (UUID) | 主键 |
| project_id | str | 所属 WorkbenchProject |
| worktree_id | str? | 所属 WorkbenchWorktree；旧记录可为空并视为主工作区 |
| name | str | 终端 tab 显示名 |
| command | str | 启动 shell 命令 |
| cwd | str | terminal window 绑定的 worktree 根路径 |
| status | str | running / exited / disconnected |
| cols | int | 最近一次 PTY 列数 |
| rows | int | 最近一次 PTY 行数 |
| started_at | datetime | 首次创建时间 |
| exited_at | datetime? | 最近断开或退出时间 |
| exit_code | int? | 子进程退出码 |
| backend | str | pty / tmux |
| backend_id | str? | tmux session 名称等后端标识；tmux 模式下由项目名、worktree 显示名和短 id 尾缀派生 |
| backend_window_id | str? | tmux window id |
| created_at | datetime | 创建时间 |
| updated_at | datetime | 更新时间 |

### 4.3 网络协议

#### mDNS 服务
- 类型：`_cc-partner._tcp.local.`
- TXT 记录：device_id, device_name

#### HTTP API
| 方法 | 路径 | 说明 |
|------|------|------|
| GET | /api/health | 健康检查 |
| POST | /api/sync/pull | 拉取 Prompt（含向量时钟摘要） |
| POST | /api/sync/push | 推送 Prompt |
| POST | /api/transfer/init | 发起文件传输 |
| POST | /api/transfer/chunk/{id} | 发送文件块 |
| GET | /api/transfer/status/{id} | 查询传输状态 |
| POST | /api/ssh-target/sync/pull | 拉取 SSH 目标（含向量时钟摘要） |
| POST | /api/ssh-target/sync/push | 推送 SSH 目标 |
