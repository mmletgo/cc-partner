# UI 组件模块

## 概述

可复用的 PyQt6 界面组件，供面板层组合使用。

## 文件说明

- `__init__.py` → 导出 FlowLayout, TagLabel, TagWidget, PromptCard
- `tag_widget.py` → 标签输入/展示组件（FlowLayout + TagLabel + TagWidget）
- `prompt_card.py` → Prompt 卡片组件

## TagWidget (tag_widget.py)

### FlowLayout
- 自定义 QLayout：子组件从左到右排列，空间不足时自动换行
- 支持 hasHeightForWidth/heightForWidth 接口，配合 QScrollArea 正确计算滚动区域高度
- setGeometry 时自动更新父容器的 minimumHeight
- _do_layout 支持 test_only 参数用于仅计算高度不实际移动组件
- 被 TagWidget（标签展示）和 PromptPanel（卡片网格布局）共同复用

### TagLabel
- 单个标签 pill：圆角彩色背景 + 文字 + x 删除按钮
- 8 种预设颜色循环分配
- 信号：`remove_clicked(str)` 点击删除时发射标签文本

### TagWidget
- 上部 FlowLayout 展示已有标签（可删除）
- 下部 QLineEdit 输入新标签（Enter 添加）
- 标签去重
- 信号：`tags_changed(list)` 标签变更时发射

## PromptCard (prompt_card.py)

- QFrame 卡片：圆角边框 + hover 高亮，固定宽度 340px（CARD_FIXED_WIDTH 常量）
- 展示：标题、内容预览(前100字)、标签 pill 行（彩色圆角标签）、更新时间
- 操作按钮：复制、编辑、删除
- 点击卡片空白区域触发编辑
- 信号：`copy_clicked(str)`, `edit_clicked(str)`, `delete_clicked(str)`, `tag_clicked(str)` 均传 prompt_id（tag_clicked 传标签文本）

## 依赖
- `claude_partner.models.prompt.Prompt`（PromptCard 使用）
