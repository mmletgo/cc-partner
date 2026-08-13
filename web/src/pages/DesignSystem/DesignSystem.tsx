/**
 * DesignSystem 设计系统预览页
 *
 * Business Logic（为什么需要这个页面）:
 *   仅在 dev 模式下可访问，作为 cc-partner 全部 UI 组件与 design token 的"对照表"。
 *   开发者/设计师打开 /design-system 即可一眼验证：
 *     1. 颜色 / 字体 / 间距 / 圆角 / 阴影 token 是否符合预期
 *     2. 原子组件 (Button/Card/Input/Tag/Pill/StatusDot/ProgressBar) 视觉与交互
 *     3. 业务组件 (PromptCard/TransferItem/PermissionCard) 复用效果
 *     4. 25+ stroke-based icon 库是否齐备
 *   避免在主应用各页面之间反复跳转查样式，提升开发效率并降低视觉漂移。
 *
 * Code Logic（这个页面做什么）:
 *   - 不嵌入 AppShell：直接用 Window 容器 + 顶部 ThemeToggle 横向排版，
 *     保持预览页"工具感"独立外观
 *   - 11 个 section 按 (色板→字体→按钮→卡片→表单→标签→状态→业务组件→icon→布局 token) 顺序
 *     渐进展示；每个 section 用 Card 包裹，section 间距 48px
 *   - 全部颜色/间距/圆角/阴影/字号走 var(--xxx)，禁止硬编码
 *   - 所有 hooks (useState) 集中在组件顶部，所有 map 渲染在 early-return 之后
 *     （虽然本组件无 early-return，但保持风格一致以遵循父项目 hook 顺序约定）
 */

import { useState } from 'react';
import type { ReactNode } from 'react';
import { useAppVersion } from '@/hooks/useAppVersion';
import {
  Button,
  Card,
  Input,
  Tag,
  Pill,
  StatusDot,
  ProgressBar,
} from '@/components/primitives';
import {
  PromptCard,
  TransferItem,
  PermissionCard,
} from '@/components/domain';
import { ThemeToggle } from '@/components/layout';
import {
  SearchIcon,
  PlusIcon,
  EditIcon,
  TrashIcon,
  CopyIcon,
  CheckIcon,
  XIcon,
  SendIcon,
  DownloadIcon,
  UploadIcon,
  PauseIcon,
  PlayIcon,
  SunIcon,
  MoonIcon,
  HomeIcon,
  TransferIcon,
  PromptsIcon,
  DevicesIcon,
  SettingsIcon,
  SyncIcon,
  FolderIcon,
  KeyboardIcon,
  InfoIcon,
  AlertIcon,
  ArrowRightIcon,
  FilterIcon,
  MoreIcon,
} from '@/lib/icons';
import type {
  PromptCardPrompt,
  TransferItemTask,
} from '@/components/domain';
import styles from './DesignSystem.module.css';

/* ─────────────────────────────── 数据 ─────────────────────────────── */

/**
 * 色板条目：把 var(--xxx) token 字符串映射到对应 hex 字面量以便显示。
 * 浅色色值与 tokens.css 完全对应（深色模式由 CSS 变量自动响应）。
 */
interface SwatchEntry {
  token: string;
  label: string;
  hex: string;
  /** 在色板上的语义分组，便于分块呈现 */
  group: 'bg' | 'fg' | 'border' | 'accent' | 'status';
}

const SWATCHES: SwatchEntry[] = [
  { token: '--bg', label: 'bg', hex: '#f5f4ed', group: 'bg' },
  { token: '--surface', label: 'surface', hex: '#faf9f5', group: 'bg' },
  { token: '--surface-warm', label: 'surface-warm', hex: '#e8e6dc', group: 'bg' },
  { token: '--fg', label: 'fg', hex: '#141413', group: 'fg' },
  { token: '--fg-2', label: 'fg-2', hex: '#3d3d3a', group: 'fg' },
  { token: '--muted', label: 'muted', hex: '#5e5d59', group: 'fg' },
  { token: '--meta', label: 'meta', hex: '#87867f', group: 'fg' },
  { token: '--border', label: 'border', hex: '#f0eee6', group: 'border' },
  { token: '--border-soft', label: 'border-soft', hex: '#e8e6dc', group: 'border' },
  { token: '--accent', label: 'accent', hex: '#c96442', group: 'accent' },
  { token: '--accent-on', label: 'accent-on', hex: '#faf9f5', group: 'accent' },
  { token: '--accent-soft', label: 'accent-soft', hex: '~14% accent', group: 'accent' },
  { token: '--success', label: 'success', hex: '#17a34a', group: 'status' },
  { token: '--warn', label: 'warn', hex: '#eab308', group: 'status' },
  { token: '--danger', label: 'danger', hex: '#b53333', group: 'status' },
];

/** 字体演示条目：title + 文案 + 字号 + 字体族 */
interface FontSpecimen {
  title: string;
  fontFamily: string;
  fontSizeToken: string;
  sample: string;
  sampleCn: string;
}

const FONT_SPECIMENS: FontSpecimen[] = [
  {
    title: 'Display',
    fontFamily: 'var(--font-display)',
    fontSizeToken: '32px (--text-3xl)',
    sample: 'The quick brown fox jumps over the lazy dog.',
    sampleCn: '设计系统预览：颜色、字体、按钮、卡片、表单。',
  },
  {
    title: 'Body',
    fontFamily: 'var(--font-body)',
    fontSizeToken: '16px (--text-lg)',
    sample: 'The quick brown fox jumps over the lazy dog.',
    sampleCn: '主体文本 16px，行高 1.6，用于段落与表单说明。',
  },
  {
    title: 'Mono',
    fontFamily: 'var(--font-mono)',
    fontSizeToken: '14px (--text-md)',
    sample: 'const x = 42;',
    sampleCn: '// 代码片段，端口号 7842',
  },
];

/** 按钮变体：与 Button 组件 1:1 映射 */
const BUTTON_VARIANTS: Array<{ variant: 'primary' | 'secondary' | 'ghost' | 'danger' | 'icon'; label: string }> = [
  { variant: 'primary', label: 'Primary' },
  { variant: 'secondary', label: 'Secondary' },
  { variant: 'ghost', label: 'Ghost' },
  { variant: 'danger', label: 'Danger' },
  { variant: 'icon', label: 'Icon' },
];

/** 按钮尺寸 */
const BUTTON_SIZES: Array<{ size: 'sm' | 'md' | 'lg'; label: string }> = [
  { size: 'sm', label: 'Small' },
  { size: 'md', label: 'Medium' },
  { size: 'lg', label: 'Large' },
];

/** Tag 颜色 */
const TAG_COLORS: Array<{ color: 'default' | 'accent' | 'success' | 'warn' | 'danger'; label: string }> = [
  { color: 'default', label: 'Default' },
  { color: 'accent', label: 'Accent' },
  { color: 'success', label: 'Success' },
  { color: 'warn', label: 'Warn' },
  { color: 'danger', label: 'Danger' },
];

/** Tag 尺寸 */
const TAG_SIZES: Array<{ size: 'sm' | 'md'; label: string }> = [
  { size: 'sm', label: 'sm' },
  { size: 'md', label: 'md' },
];

/** Pill tone 列表 */
const PILL_TONES: Array<{ tone: 'neutral' | 'success' | 'warn' | 'danger' | 'accent'; label: string }> = [
  { tone: 'neutral', label: 'Neutral' },
  { tone: 'success', label: 'Success' },
  { tone: 'warn', label: 'Warn' },
  { tone: 'danger', label: 'Danger' },
  { tone: 'accent', label: 'Accent' },
];

/** StatusDot status 列表 */
const STATUS_OPTIONS: Array<{ status: 'online' | 'offline' | 'busy' | 'away'; label: string }> = [
  { status: 'online', label: 'Online' },
  { status: 'offline', label: 'Offline' },
  { status: 'busy', label: 'Busy' },
  { status: 'away', label: 'Away' },
];

/** ProgressBar tone 列表 */
const PROGRESS_TONES: Array<{ tone: 'accent' | 'success' | 'warn' | 'danger'; label: string; value: number }> = [
  { tone: 'accent', label: 'Accent', value: 0.62 },
  { tone: 'success', label: 'Success', value: 1 },
  { tone: 'warn', label: 'Warn', value: 0.38 },
  { tone: 'danger', label: 'Danger', value: 0.15 },
];

/** ProgressBar size 列表 */
const PROGRESS_SIZES: Array<{ size: 'sm' | 'md' | 'lg'; label: string }> = [
  { size: 'sm', label: 'sm (4px)' },
  { size: 'md', label: 'md (6px)' },
  { size: 'lg', label: 'lg (8px)' },
];

/** 间距阶梯展示条目 */
const SPACE_SCALE: Array<{ token: string; px: number; label: string }> = [
  { token: '--space-1', px: 4, label: '4' },
  { token: '--space-2', px: 8, label: '8' },
  { token: '--space-3', px: 12, label: '12' },
  { token: '--space-4', px: 16, label: '16' },
  { token: '--space-5', px: 20, label: '20' },
  { token: '--space-6', px: 24, label: '24' },
  { token: '--space-8', px: 32, label: '32' },
  { token: '--space-10', px: 40, label: '40' },
  { token: '--space-12', px: 48, label: '48' },
  { token: '--space-16', px: 64, label: '64' },
];

/** 圆角展示条目 */
const RADIUS_SCALE: Array<{ token: string; radius: number; label: string }> = [
  { token: '--radius-xs', radius: 4, label: 'xs' },
  { token: '--radius-sm', radius: 6, label: 'sm' },
  { token: '--radius-md', radius: 8, label: 'md' },
  { token: '--radius-lg', radius: 12, label: 'lg' },
];

/** 阴影展示条目 */
const SHADOW_SCALE: Array<{ token: string; boxShadow: string; label: string }> = [
  { token: '--shadow-xs', boxShadow: 'var(--shadow-xs)', label: 'xs' },
  { token: '--shadow-sm', boxShadow: 'var(--shadow-sm)', label: 'sm' },
  { token: '--shadow-md', boxShadow: 'var(--shadow-md)', label: 'md' },
  { token: '--shadow-lg', boxShadow: 'var(--shadow-lg)', label: 'lg' },
];

/** Icon 库：name + 组件映射，渲染时按 entries 顺序循环 */
const ICON_LIBRARY: Array<{ name: string; component: () => ReactNode }> = [
  { name: 'SearchIcon', component: () => <SearchIcon size={24} /> },
  { name: 'PlusIcon', component: () => <PlusIcon size={24} /> },
  { name: 'EditIcon', component: () => <EditIcon size={24} /> },
  { name: 'TrashIcon', component: () => <TrashIcon size={24} /> },
  { name: 'CopyIcon', component: () => <CopyIcon size={24} /> },
  { name: 'CheckIcon', component: () => <CheckIcon size={24} /> },
  { name: 'XIcon', component: () => <XIcon size={24} /> },
  { name: 'SendIcon', component: () => <SendIcon size={24} /> },
  { name: 'DownloadIcon', component: () => <DownloadIcon size={24} /> },
  { name: 'UploadIcon', component: () => <UploadIcon size={24} /> },
  { name: 'PauseIcon', component: () => <PauseIcon size={24} /> },
  { name: 'PlayIcon', component: () => <PlayIcon size={24} /> },
  { name: 'SunIcon', component: () => <SunIcon size={24} /> },
  { name: 'MoonIcon', component: () => <MoonIcon size={24} /> },
  { name: 'HomeIcon', component: () => <HomeIcon size={24} /> },
  { name: 'TransferIcon', component: () => <TransferIcon size={24} /> },
  { name: 'PromptsIcon', component: () => <PromptsIcon size={24} /> },
  { name: 'DevicesIcon', component: () => <DevicesIcon size={24} /> },
  { name: 'SettingsIcon', component: () => <SettingsIcon size={24} /> },
  { name: 'SyncIcon', component: () => <SyncIcon size={24} /> },
  { name: 'FolderIcon', component: () => <FolderIcon size={24} /> },
  { name: 'KeyboardIcon', component: () => <KeyboardIcon size={24} /> },
  { name: 'InfoIcon', component: () => <InfoIcon size={24} /> },
  { name: 'AlertIcon', component: () => <AlertIcon size={24} /> },
  { name: 'ArrowRightIcon', component: () => <ArrowRightIcon size={24} /> },
  { name: 'FilterIcon', component: () => <FilterIcon size={24} /> },
  { name: 'MoreIcon', component: () => <MoreIcon size={24} /> },
];

/* ────────────────────────── Mock 数据 ────────────────────────── */

/** PromptCard mock：与 PromptCardPrompt 形状一致 */
const MOCK_PROMPT: PromptCardPrompt = {
  id: 'ds-1',
  title: '代码评审助手',
  content:
    '你是一名严谨的代码评审员，请重点关注可读性、边界条件与异常处理。' +
    '对每条建议给出文件路径 + 行号 + 改写示例。',
  tag: 'Review',
  updatedAt: '2026-06-09T14:32:00Z',
};

/** TransferItem mock：覆盖 transferring / completed / failed */
const MOCK_TRANSFER: TransferItemTask = {
  id: 'tr-1',
  fileName: 'design-system-mock.pdf',
  fileSize: 2.4 * 1024 * 1024,
  direction: 'send',
  status: 'transferring',
  progress: 0.62,
  peerDevice: "Hans's MacBook Pro",
  speed: 1.2 * 1024 * 1024,
};

/* ──────────────────────── 子组件（局部） ──────────────────────── */

/**
 * 通用 section header：eyebrow + 标题 + 副标题
 */
interface SectionHeaderProps {
  eyebrow: string;
  title: string;
  desc?: string;
}

function SectionHeader({ eyebrow, title, desc }: SectionHeaderProps) {
  return (
    <div className={styles.sectionHeader}>
      <div className={styles.eyebrow}>{eyebrow}</div>
      <h2 className={styles.sectionTitle}>{title}</h2>
      {desc ? <p className={styles.sectionDesc}>{desc}</p> : null}
    </div>
  );
}

/* ────────────────────────── 页面根组件 ────────────────────────── */

/**
 * DesignSystem 预览页根组件
 *
 * @returns 不嵌入 AppShell，直接渲染独立 Window 容器
 */
export function DesignSystem() {
  // hooks 全部放在顶部；本组件无 early-return，但保留风格一致
  const [inputValue, setInputValue] = useState<string>('cc-partner-7842');
  const [searchValue, setSearchValue] = useState<string>('搜索 prompt...');
  const [inputPassword, setInputPassword] = useState<string>('a-very-secret-token');
  // 应用版本号来自后端 __version__（与 AppShell / Settings 同源），用于页头与页脚展示
  const version = useAppVersion();

  /**
   * 通用 input change handler
   *
   * @param setter useState setter
   * @returns 适配 React.ChangeEvent 的回调
   */
  const makeInputHandler = (setter: (v: string) => void) =>
    (e: React.ChangeEvent<HTMLInputElement>): void => {
      setter(e.target.value);
    };

  return (
    <div className={styles.page}>
      <div className={styles.windowFrame}>
        {/* ── 顶部条（页面头部 + ThemeToggle） ── */}
        <header className={styles.pageHeader}>
          <div className={styles.headerText}>
            <h1 className={styles.title}>设计系统</h1>
            <p className={styles.subtitle}>
              cc-partner 设计系统 v{version ?? '—'} · 仅开发环境可见
            </p>
          </div>
          <ThemeToggle className={styles.themeToggle} />
        </header>

        <main className={styles.content}>
          {/* ───────── 1. 颜色色板 ───────── */}
          <section className={styles.section} aria-label="颜色色板">
            <SectionHeader
              eyebrow="01 · Palette"
              title="颜色"
              desc="15 个核心颜色 token，覆盖背景、文本、边框、强调、状态五大维度。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.swatchGrid}>
                {SWATCHES.map((s) => (
                  <div key={s.token} className={styles.swatch}>
                    <div
                      className={styles.swatchChip}
                      style={{ backgroundColor: `var(${s.token})` }}
                      data-tone={s.group}
                      aria-hidden="true"
                    />
                    <div className={styles.swatchLabel}>
                      <span className={styles.swatchToken}>{s.label}</span>
                      <span className={styles.swatchHex}>{s.hex}</span>
                    </div>
                  </div>
                ))}
              </div>
            </Card>
          </section>

          {/* ───────── 2. 字体 ───────── */}
          <section className={styles.section} aria-label="字体">
            <SectionHeader
              eyebrow="02 · Typography"
              title="字体"
              desc="三种字体族：display (serif) / body (sans) / mono。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.fontList}>
                {FONT_SPECIMENS.map((spec) => (
                  <div key={spec.title} className={styles.fontRow}>
                    <div className={styles.fontMeta}>
                      <span className={styles.fontName}>{spec.title}</span>
                      <span className={styles.fontSize}>{spec.fontSizeToken}</span>
                    </div>
                    <p
                      className={styles.fontSample}
                      style={{ fontFamily: spec.fontFamily }}
                    >
                      {spec.sample}
                    </p>
                    <p
                      className={styles.fontSampleCn}
                      style={{ fontFamily: spec.fontFamily }}
                    >
                      {spec.sampleCn}
                    </p>
                  </div>
                ))}
              </div>
            </Card>
          </section>

          {/* ───────── 3. 按钮 ───────── */}
          <section className={styles.section} aria-label="按钮">
            <SectionHeader
              eyebrow="03 · Buttons"
              title="按钮"
              desc="5 种 variant × 3 种 size，并演示 disabled / loading / iconOnly 状态。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.btnRow}>
                <div className={styles.btnRowLabel}>Variant · size=md</div>
                <div className={styles.btnRowItems}>
                  {BUTTON_VARIANTS.map((v) => (
                    <Button
                      key={v.variant}
                      variant={v.variant}
                      size="md"
                      icon={v.variant === 'icon' ? <PlusIcon /> : undefined}
                    >
                      {v.variant === 'icon' ? null : v.label}
                    </Button>
                  ))}
                </div>
              </div>

              <div className={styles.btnRow}>
                <div className={styles.btnRowLabel}>Size · variant=secondary</div>
                <div className={styles.btnRowItems}>
                  {BUTTON_SIZES.map((s) => (
                    <Button key={s.size} variant="secondary" size={s.size}>
                      {s.label}
                    </Button>
                  ))}
                </div>
              </div>

              <div className={styles.btnRow}>
                <div className={styles.btnRowLabel}>States</div>
                <div className={styles.btnRowItems}>
                  <Button variant="primary" size="md" disabled>
                    Disabled
                  </Button>
                  <Button variant="primary" size="md" loading>
                    Loading
                  </Button>
                  <Button variant="icon" size="md" icon={<PlusIcon />} aria-label="新建" />
                </div>
              </div>
            </Card>
          </section>

          {/* ───────── 4. 卡片 ───────── */}
          <section className={styles.section} aria-label="卡片">
            <SectionHeader
              eyebrow="04 · Cards"
              title="卡片"
              desc="3 种 variant：flat / elevated / outlined。Header/Body/Footer 通过 Card 复合组件拼装。"
            />
            <div className={styles.cardGrid}>
              {(['flat', 'elevated', 'outlined'] as const).map((v) => (
                <Card key={v} variant={v} padding="none" className={styles.demoCard}>
                  <Card.Header>
                    <h3 className={styles.demoCardTitle}>{v}</h3>
                    <span className={styles.demoCardBadge}>variant</span>
                  </Card.Header>
                  <Card.Body>
                    <p className={styles.demoCardBody}>
                      这是一段示例内容，演示 Card.Body 的留白和文本行高。
                    </p>
                  </Card.Body>
                  <Card.Footer className={styles.demoCardFooter}>
                    <Button variant="ghost" size="sm">
                      取消
                    </Button>
                    <Button variant="primary" size="sm">
                      确认
                    </Button>
                  </Card.Footer>
                </Card>
              ))}
            </div>
          </section>

          {/* ───────── 5. 表单 ───────── */}
          <section className={styles.section} aria-label="表单">
            <SectionHeader
              eyebrow="05 · Forms"
              title="表单"
              desc="Input 支持 text / password / search，icon / iconRight 槽位，mono 字体变体，以及 disabled 态。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.formGrid}>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Text + icon</label>
                  <Input
                    type="text"
                    value={inputValue}
                    onChange={makeInputHandler(setInputValue)}
                    placeholder="端口号..."
                    icon={<SearchIcon />}
                  />
                </div>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Password</label>
                  <Input
                    type="password"
                    value={inputPassword}
                    onChange={makeInputHandler(setInputPassword)}
                    placeholder="密码"
                  />
                </div>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Mono (code)</label>
                  <Input
                    type="text"
                    value={inputValue}
                    onChange={makeInputHandler(setInputValue)}
                    placeholder="device-token..."
                    mono
                  />
                </div>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Search + iconRight</label>
                  <Input
                    type="search"
                    value={searchValue}
                    onChange={makeInputHandler(setSearchValue)}
                    placeholder="搜索..."
                    icon={<SearchIcon />}
                    iconRight={<FilterIcon />}
                  />
                </div>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Disabled</label>
                  <Input
                    type="text"
                    value="不可编辑"
                    onChange={() => undefined}
                    disabled
                  />
                </div>
                <div className={styles.formField}>
                  <label className={styles.formLabel}>Size sm</label>
                  <Input
                    type="text"
                    value={inputValue}
                    onChange={makeInputHandler(setInputValue)}
                    size="sm"
                    placeholder="小号输入"
                  />
                </div>
              </div>
            </Card>
          </section>

          {/* ───────── 6. 标签 ───────── */}
          <section className={styles.section} aria-label="标签">
            <SectionHeader
              eyebrow="06 · Tag / Pill"
              title="标签"
              desc="Tag 用于分类；Pill 用于状态徽章，体积更紧凑。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.tagGroup}>
                <div className={styles.tagGroupLabel}>Tag · color (size=md)</div>
                <div className={styles.tagRow}>
                  {TAG_COLORS.map((t) => (
                    <Tag key={t.color} color={t.color} size="md">
                      {t.label}
                    </Tag>
                  ))}
                </div>
              </div>
              <div className={styles.tagGroup}>
                <div className={styles.tagGroupLabel}>Tag · size (color=default)</div>
                <div className={styles.tagRow}>
                  {TAG_SIZES.map((s) => (
                    <Tag key={s.size} color="default" size={s.size}>
                      size={s.label}
                    </Tag>
                  ))}
                </div>
              </div>
              <div className={styles.tagGroup}>
                <div className={styles.tagGroupLabel}>Pill · tone</div>
                <div className={styles.tagRow}>
                  {PILL_TONES.map((p) => (
                    <Pill key={p.tone} tone={p.tone} dot>
                      {p.label}
                    </Pill>
                  ))}
                </div>
              </div>
              <div className={styles.tagGroup}>
                <div className={styles.tagGroupLabel}>Pill · dot off</div>
                <div className={styles.tagRow}>
                  {PILL_TONES.map((p) => (
                    <Pill key={`${p.tone}-nodot`} tone={p.tone} dot={false}>
                      {p.label}
                    </Pill>
                  ))}
                </div>
              </div>
            </Card>
          </section>

          {/* ───────── 7. 状态 ───────── */}
          <section className={styles.section} aria-label="状态指示">
            <SectionHeader
              eyebrow="07 · Status"
              title="状态指示"
              desc="StatusDot 用于最小空间传达状态；ProgressBar 用于进度可视化。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.statusGroup}>
                <div className={styles.statusGroupLabel}>StatusDot · size=md</div>
                <div className={styles.statusRow}>
                  {STATUS_OPTIONS.map((s) => (
                    <div key={s.status} className={styles.statusItem}>
                      <StatusDot status={s.status} size="md" />
                      <span className={styles.statusItemLabel}>{s.label}</span>
                    </div>
                  ))}
                </div>
              </div>
              <div className={styles.statusGroup}>
                <div className={styles.statusGroupLabel}>StatusDot · size=sm</div>
                <div className={styles.statusRow}>
                  {STATUS_OPTIONS.map((s) => (
                    <div key={`${s.status}-sm`} className={styles.statusItem}>
                      <StatusDot status={s.status} size="sm" />
                      <span className={styles.statusItemLabel}>{s.label}</span>
                    </div>
                  ))}
                </div>
              </div>
              <div className={styles.statusGroup}>
                <div className={styles.statusGroupLabel}>ProgressBar · tone (size=md)</div>
                <div className={styles.progressList}>
                  {PROGRESS_TONES.map((p) => (
                    <ProgressBar
                      key={p.tone}
                      value={p.value}
                      tone={p.tone}
                      size="md"
                    />
                  ))}
                </div>
              </div>
              <div className={styles.statusGroup}>
                <div className={styles.statusGroupLabel}>ProgressBar · size (tone=accent)</div>
                <div className={styles.progressList}>
                  {PROGRESS_SIZES.map((s) => (
                    <ProgressBar
                      key={s.size}
                      value={0.5}
                      tone="accent"
                      size={s.size}
                    />
                  ))}
                </div>
              </div>
            </Card>
          </section>

          {/* ───────── 8. 业务组件 ───────── */}
          <section className={styles.section} aria-label="业务组件">
            <SectionHeader
              eyebrow="08 · Domain Components"
              title="业务组件"
              desc="PromptCard / TransferItem / PermissionCard 三个业务组件的最小可运行示例。"
            />
            <div className={styles.domainGrid}>
              <div className={styles.domainCol}>
                <div className={styles.domainLabel}>PromptCard</div>
                <PromptCard prompt={MOCK_PROMPT} />
              </div>
              <div className={styles.domainCol}>
                <div className={styles.domainLabel}>TransferItem · transferring</div>
                <TransferItem task={MOCK_TRANSFER} />
              </div>
              <div className={styles.domainCol}>
                <div className={styles.domainLabel}>PermissionCard</div>
                <PermissionCard
                  icon={<InfoIcon />}
                  title="屏幕录制"
                  description="用于区域截图 / 录制功能"
                  granted={true}
                />
                <PermissionCard
                  icon={<KeyboardIcon />}
                  title="辅助功能"
                  description="用于发送全局快捷键"
                  granted={false}
                />
              </div>
            </div>
          </section>

          {/* ───────── 9. Icon 库 ───────── */}
          <section className={styles.section} aria-label="Icon 库">
            <SectionHeader
              eyebrow="09 · Icons"
              title="Icon 库"
              desc="27 个 stroke-based 16x16 SVG，统一从 @/lib/icons 导出。"
            />
            <Card variant="outlined" padding="lg" className={styles.sectionCard}>
              <div className={styles.iconGrid}>
                {ICON_LIBRARY.map((entry) => (
                  <div key={entry.name} className={styles.iconCell}>
                    <div className={styles.iconGlyph}>{entry.component()}</div>
                    <span className={styles.iconName}>{entry.name}</span>
                  </div>
                ))}
              </div>
            </Card>
          </section>

          {/* ───────── 10. 布局 token ───────── */}
          <section className={styles.section} aria-label="布局 token">
            <SectionHeader
              eyebrow="10 · Layout Tokens"
              title="间距 / 圆角 / 阴影"
              desc="布局维度的 design token 阶梯，所有数值都可在 tokens.css 集中调整。"
            />
            <div className={styles.layoutGrid}>
              {/* 间距阶梯 */}
              <Card variant="outlined" padding="lg" className={styles.sectionCard}>
                <div className={styles.layoutCardTitle}>间距 · Spacing</div>
                <div className={styles.spaceScale}>
                  {SPACE_SCALE.map((s) => (
                    <div key={s.token} className={styles.spaceItem}>
                      <div
                        className={styles.spaceBlock}
                        style={{ width: `${s.px}px` }}
                        aria-hidden="true"
                      />
                      <div className={styles.spaceMeta}>
                        <span className={styles.spaceToken}>{s.token}</span>
                        <span className={styles.spaceLabel}>{s.label}px</span>
                      </div>
                    </div>
                  ))}
                </div>
              </Card>

              {/* 圆角 */}
              <Card variant="outlined" padding="lg" className={styles.sectionCard}>
                <div className={styles.layoutCardTitle}>圆角 · Radius</div>
                <div className={styles.radiusGrid}>
                  {RADIUS_SCALE.map((r) => (
                    <div key={r.token} className={styles.radiusItem}>
                      <div
                        className={styles.radiusBlock}
                        style={{ borderRadius: `var(${r.token})` }}
                        aria-hidden="true"
                      />
                      <div className={styles.radiusMeta}>
                        <span className={styles.radiusToken}>{r.token}</span>
                        <span className={styles.radiusLabel}>{r.radius}px</span>
                      </div>
                    </div>
                  ))}
                </div>
              </Card>

              {/* 阴影 */}
              <Card variant="outlined" padding="lg" className={styles.sectionCard}>
                <div className={styles.layoutCardTitle}>阴影 · Shadow</div>
                <div className={styles.shadowGrid}>
                  {SHADOW_SCALE.map((s) => (
                    <div key={s.token} className={styles.shadowItem}>
                      <div
                        className={styles.shadowBlock}
                        style={{ boxShadow: s.boxShadow }}
                        aria-hidden="true"
                      />
                      <div className={styles.shadowMeta}>
                        <span className={styles.shadowToken}>{s.token}</span>
                        <span className={styles.shadowLabel}>shadow-{s.label}</span>
                      </div>
                    </div>
                  ))}
                </div>
              </Card>
            </div>
          </section>

          {/* ───────── 11. 页脚 ───────── */}
          <footer className={styles.pageFooter}>
            <span>cc-partner Design System v{version ?? '—'}</span>
            <span>{ICON_LIBRARY.length} icons · 15 colors · 10 spacing · 4 radius · 4 shadow</span>
          </footer>
        </main>
      </div>
    </div>
  );
}

export default DesignSystem;
