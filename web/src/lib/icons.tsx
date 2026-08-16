/**
 * Icon 统一管理 - 16x16 inline SVG，stroke-based
 * 与 uiux/ 设计稿的 icon 系统保持一致
 * 添加新 icon: 在此文件新增一个函数，遵循同样的 viewBox/stroke 规范
 */

import type { SVGProps } from 'react';

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

const baseProps = (size = 16): SVGProps<SVGSVGElement> => ({
  width: size,
  height: size,
  viewBox: '0 0 16 16',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.6,
  strokeLinecap: 'round' as const,
  strokeLinejoin: 'round' as const,
});

export const SearchIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="7" cy="7" r="4.5" />
    <path d="M10.5 10.5 14 14" />
  </svg>
);

export const PlusIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 3.5v9M3.5 8h9" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   工作台项目「在新窗口打开」需要与删除/添加明显区分的窗口语义。
 *
 * Code Logic（做什么）:
 *   渲染窗口外框与顶栏的 16x16 stroke SVG。
 */
export const WindowIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2.5" y="3" width="11" height="10" rx="1.4" />
    <path d="M2.5 6h11" />
  </svg>
);

export const EditIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M11.5 2.5 13.5 4.5 5 13H3v-2L11.5 2.5Z" />
    <path d="M10 4 12 6" />
  </svg>
);

export const TrashIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 4h10M6.5 4V2.5h3V4M5 4l.5 9.5h5L11 4" />
    <path d="M7 7v4M9 7v4" />
  </svg>
);

export const CopyIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="5" y="5" width="8" height="9" rx="1.5" />
    <path d="M3 11V3.5A1.5 1.5 0 0 1 4.5 2H10" />
  </svg>
);

export const CheckIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 8.5 6.5 12 13 4.5" />
  </svg>
);

export const XIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M4 4l8 8M12 4l-8 8" />
  </svg>
);

export const SendIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2 8 14 3l-3 11-3-5-6-1Z" />
  </svg>
);

export const DownloadIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 2v9M5 8l3 3 3-3M3 14h10" />
  </svg>
);

export const UploadIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 11V2M5 5l3-3 3 3M3 14h10" />
  </svg>
);

export const PauseIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="4" y="3" width="3" height="10" rx="0.5" />
    <rect x="9" y="3" width="3" height="10" rx="0.5" />
  </svg>
);

export const PlayIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M4 3 13 8 4 13V3Z" />
  </svg>
);

export const SunIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="8" cy="8" r="3" />
    <path d="M8 1.5v1.5M8 13v1.5M14.5 8H13M3 8H1.5M12.6 3.4l-1 1M4.4 11.6l-1 1M12.6 12.6l-1-1M4.4 4.4l-1-1" />
  </svg>
);

export const MoonIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M13 9.5A5.5 5.5 0 1 1 6.5 3a4.5 4.5 0 0 0 6.5 6.5Z" />
  </svg>
);

export const HomeIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2.5 7.5 8 3l5.5 4.5V13a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V7.5Z" />
    <path d="M6 14V9.5h4V14" />
  </svg>
);

export const TransferIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 5h9l-2-2M13 11H4l2 2" />
  </svg>
);

export const PromptsIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2.5" y="2.5" width="11" height="11" rx="2" />
    <path d="M5 6h6M5 8.5h6M5 11h4" />
  </svg>
);

export const DevicesIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2" y="3" width="9" height="7" rx="1" />
    <rect x="10" y="7" width="4" height="6" rx="0.8" />
    <path d="M4 12h5" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   AppShell 侧栏 footer 需要一个单独的手机入口图标，用来打开移动端 Workbench 访问弹层。
 *
 * Code Logic（做什么）:
 *   渲染手机外框、听筒和底部指示点的 16x16 stroke SVG，并复用 baseProps 保持尺寸、描边和 viewBox 一致。
 */
export const SmartphoneIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="4.5" y="1.8" width="7" height="12.4" rx="1.4" />
    <path d="M6.7 4h2.6M8 11.8h.01" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   设置入口（侧栏 footer / Orchestrator / Mobile）需要与主题 sun/moon 明显区分的齿轮语义。
 *
 * Code Logic（做什么）:
 *   渲染经典齿轮：中心圆 + 外圈齿形轮廓，复用 baseProps 保持 16 viewBox 与描边规范。
 */
export const SettingsIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="8" cy="8" r="2.1" />
    <path d="M6.6 1.7h2.8l.35 1.45c.42.12.81.32 1.15.58l1.4-.5.99.99-.5 1.4c.26.34.46.73.58 1.15L14.3 6.6v2.8l-1.45.35c-.12.42-.32.81-.58 1.15l.5 1.4-.99.99-1.4-.5a3.6 3.6 0 0 1-1.15.58L9.4 14.3H6.6l-.35-1.45a3.6 3.6 0 0 1-1.15-.58l-1.4.5-.99-.99.5-1.4a3.6 3.6 0 0 1-.58-1.15L1.7 9.4V6.6l1.45-.35c.12-.42.32-.81.58-1.15l-.5-1.4.99-.99 1.4.5c.34-.26.73-.46 1.15-.58L6.6 1.7Z" />
  </svg>
);

export const SyncIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2 8a6 6 0 0 1 10.5-4M14 8a6 6 0 0 1-10.5 4" />
    <path d="M12.5 1.5v3h-3M3.5 14.5v-3h3" />
  </svg>
);

export const FolderIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3H6l1.5 1.5h5A1.5 1.5 0 0 1 14 6v6a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12V4.5Z" />
  </svg>
);

export const FileIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M4.5 2h5L12 4.5V13a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V3a1 1 0 0 1 .5-1Z" />
    <path d="M9.5 2v3h3" />
  </svg>
);

export const ChevronRightIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="m6 4 4 4-4 4" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   移动端项目工作台导航需要「返回项目列表」入口，与右向 chevron 对称。
 *
 * Code Logic（做什么）:
 *   渲染向左 chevron 的 16x16 stroke SVG。
 */
export const ChevronLeftIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="m10 4-4 4 4 4" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   移动端 Workbench 顶部 worktree pill 需要下拉提示图标，表明可打开快速切换 sheet。
 *
 * Code Logic（做什么）:
 *   渲染向下 chevron 的 16x16 stroke SVG，并复用 baseProps 保持尺寸、描边和 viewBox 一致。
 */
export const ChevronDownIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="m4 6 4 4 4-4" />
  </svg>
);

export const StopIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="4" y="4" width="8" height="8" rx="1.2" />
  </svg>
);

export const KeyboardIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2" y="4" width="12" height="8" rx="1.5" />
    <path d="M5 7h.01M8 7h.01M11 7h.01M5 9.5h6" />
  </svg>
);

export const InfoIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="8" cy="8" r="6" />
    <path d="M8 7v4M8 4.5h.01" />
  </svg>
);

export const AlertIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 2 14 13H2L8 2Z" />
    <path d="M8 6.5v3M8 11.5h.01" />
  </svg>
);

export const ArrowRightIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 8h10M9 4l4 4-4 4" />
  </svg>
);

export const FilterIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2 3h12L9.5 8.5V13L6.5 11.5V8.5L2 3Z" />
  </svg>
);

export const MoreIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="4" cy="8" r="1" />
    <circle cx="8" cy="8" r="1" />
    <circle cx="12" cy="8" r="1" />
  </svg>
);

export const SplitRightIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2.5" y="2.5" width="11" height="11" rx="1.5" />
    <path d="M8 2.5v11M5 8h6M9 6l2 2-2 2" />
  </svg>
);

export const SplitDownIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2.5" y="2.5" width="11" height="11" rx="1.5" />
    <path d="M2.5 8h11M8 5v6M6 9l2 2 2-2" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   移动端终端需要进入全屏的工具栏图标，帮助用户快速识别扩大终端视图的操作。
 *
 * Code Logic（做什么）:
 *   渲染四角外扩的 16x16 stroke SVG，并复用 baseProps 保持尺寸、描边和 viewBox 一致。
 */
export const MaximizeIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M6 3H3v3M10 3h3v3M6 13H3v-3M10 13h3v-3" />
    <path d="M3 3l3 3M13 3l-3 3M3 13l3-3M13 13l-3-3" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   移动端终端全屏后需要明确的退出全屏图标，避免用户被固定覆盖层困住。
 *
 * Code Logic（做什么）:
 *   渲染四角内收的 16x16 stroke SVG，并复用 baseProps 保持尺寸、描边和 viewBox 一致。
 */
export const MinimizeIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M6 3v3H3M10 3v3h3M6 13v-3H3M10 13v-3h3" />
    <path d="M6 6 3 3M10 6l3-3M6 10l-3 3M10 10l3 3" />
  </svg>
);

export const ScratchpadIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M4.5 1.5h6l3 3v9a1.5 1.5 0 0 1-1.5 1.5h-7.5A1.5 1.5 0 0 1 3.5 14V3a1.5 1.5 0 0 1 1-1.5Z" />
    <path d="M10.5 1.5v3h3" />
    <path d="M6 8h4M6 10.5h3" />
  </svg>
);

export const HistoryIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2.5 7.5a5.5 5.5 0 1 0 1.7-3.95" />
    <path d="M3 1.5v3h3" />
    <path d="M8 5v3.2l2 1.3" />
  </svg>
);

export const ClaudeMdIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3.5 2h6L12.5 5v8.5a.5.5 0 0 1-.5.5h-8.5a.5.5 0 0 1-.5-.5v-11a.5.5 0 0 1 .5-.5Z" />
    <path d="M9 2v3.5h3.5" />
    <path d="M6 8h4M6 10h4M6 12h2" />
  </svg>
);

export const TerminalIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="1.5" y="2.5" width="13" height="11" rx="1.5" />
    <path d="M4 6l2.5 2L4 10" />
    <path d="M8.5 10h4" />
  </svg>
);

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 浏览器预览需要在桌面 toolbar 和移动端导航中使用统一图标。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 16x16 线性浏览器窗口图标，继承 currentColor 并支持 size 覆盖。
 */
export const BrowserIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="2.4" y="3.2" width="11.2" height="9.6" rx="1.6" />
    <path d="M2.8 6h10.4" />
    <path d="M5 4.6h.01M7 4.6h.01" strokeWidth={1.8} />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   侧栏需要为 Orchestrator 自动化入口提供统一的 16x16 图标，避免导航项只靠文字识别。
 *
 * Code Logic（做什么）:
 *   渲染三节点编排流的 stroke SVG，并复用 baseProps 保持尺寸、描边和 viewBox 一致。
 */
export const OrchestratorIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="4" cy="4" r="1.8" />
    <circle cx="12" cy="4" r="1.8" />
    <circle cx="8" cy="12" r="1.8" />
    <path d="M5.6 4h4.8M4.8 5.6 7.2 10.4M11.2 5.6 8.8 10.4" />
  </svg>
);

export const HealthIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M1.5 8h2l1.5-3.5L7.5 12 10 5l1.5 3h3" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   侧栏活动统计入口需要与健康提醒区分的图表语义。
 *
 * Code Logic（做什么）:
 *   渲染三根柱状图 stroke SVG，复用 baseProps。
 */
export const ActivityIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 12.5V8" />
    <path d="M8 12.5V3.5" />
    <path d="M13 12.5V6.5" />
  </svg>
);

export const StarIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 2.2 9.8 5.8l4 .6-2.9 2.8.7 4-3.6-1.9-3.6 1.9.7-4-2.9-2.8 4-.6L8 2.2Z" />
  </svg>
);

export const ForkIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="4.5" cy="3.5" r="1.5" />
    <circle cx="11.5" cy="3.5" r="1.5" />
    <circle cx="8" cy="12.5" r="1.5" />
    <path d="M4.5 5v1.5A1.5 1.5 0 0 0 6 8h4a1.5 1.5 0 0 0 1.5-1.5V5M8 8v3" />
  </svg>
);

export const ExternalLinkIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M6.5 3.5H4A1.5 1.5 0 0 0 2.5 5v7A1.5 1.5 0 0 0 4 13.5h7A1.5 1.5 0 0 0 12.5 12V9.5" />
    <path d="M9 2.5h4.5V7M13.5 2.5 7.5 8.5" />
  </svg>
);

export const RefreshIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M13.5 8a5.5 5.5 0 0 1-9.2 4.05" />
    <path d="M2.5 8a5.5 5.5 0 0 1 9.2-4.05" />
    <path d="M11.8 1.8v2.6H9.2M4.2 14.2v-2.6h2.6" />
  </svg>
);

export const BellIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 1.5v1" />
    <path d="M4 6a4 4 0 0 1 8 0c0 3 1.5 4.5 2 5.5H2c.5-1 2-2.5 2-5.5Z" />
    <path d="M6.8 13.2a1.3 1.3 0 0 0 2.4 0" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   移动端 Workbench 顶部按钮需要统一的菜单图标，避免组件内硬编码字符。
 *
 * Code Logic（做什么）:
 *   渲染三条横线的 16x16 stroke SVG，并继承通用 icon 尺寸与描边规范。
 */
export const MenuIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M3 4h10M3 8h10M3 12h10" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   Provider Manager 切换 provider，需要一个"交换/切换"语义的图标。
 *
 * Code Logic（做什么）:
 *   渲染上下两根带箭头的水平线（swap），16x16 stroke，继承通用 icon 规范。
 */
export const ProviderManagerIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M2.5 5.5h7" />
    <path d="M7.5 3l2 2.5-2 2.5" />
    <path d="M13.5 10.5h-7" />
    <path d="M8.5 8l-2 2.5 2 2.5" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   Agent Hub 提示词「AI 辅助修改」按钮需要统一的火花图标，避免页面内硬编码 SVG。
 *
 * Code Logic（做什么）:
 *   渲染四向火花 16x16 stroke SVG，继承通用 icon 尺寸与描边规范。
 */
export const SparkleIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 1.5v3.2M8 11.3V14.5M1.5 8h3.2M11.3 8H14.5" />
    <path d="M3.4 3.4l2.2 2.2M10.4 10.4l2.2 2.2M12.6 3.4l-2.2 2.2M5.6 10.4l-2.2 2.2" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   侧栏 footer 游戏大厅入口由文字按钮改为图标按钮，需要游戏手柄语义的集中管理图标。
 *
 * Code Logic（做什么）:
 *   渲染 16x16 stroke 手柄：对称机身轮廓 + 左侧十字方向键 + 右侧两枚圆点按钮
 *   （path 由 24x24 gamepad 轮廓等比缩放 2/3，保证 stroke 半宽后仍在 viewBox 内）。
 */
export const GameIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M11.55 3.33H4.45a2.67 2.67 0 0 0-2.65 2.39c-.004.035-.007.067-.011.1C1.74 6.28 1.33 9.64 1.33 10.67a2 2 0 0 0 2 2c.67 0 1-.33 1.33-.67l.94-.94a1.33 1.33 0 0 1 .95-.39h1.56a1.33 1.33 0 0 1 .94.39L10 12c.33.33.67.67 1.33.67a2 2 0 0 0 2-2c0-1.03-.4-4.39-.46-4.84-.005-.03-.007-.07-.01-.1a2.67 2.67 0 0 0-1.31-2.4z" />
    <path d="M4 7.33h2.67M5.33 6v2.67" />
    <path d="M10 8h.01M12 6.67h.01" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   充电模式切换按钮需要电池语义，图标必须集中在本库。
 *
 * Code Logic（做什么）:
 *   渲染电池外框与正极的 16x16 stroke SVG。
 */
export const BatteryIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <rect x="1.5" y="4.5" width="11.5" height="7" rx="1.4" />
    <path d="M13.8 6.6v2.8" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   无限模式目标态需要 ∞ 符号，避免组件内硬编码文字。
 *
 * Code Logic（做什么）:
 *   渲染横 8 字形 16x16 stroke SVG；path 起点固定 M1.2 保证
 *   曲线 x 范围 1.2..14.8 含 stroke 半宽后仍在 viewBox 内且水平居中。
 */
export const InfinityIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M1.2 8c0-1.7 1.3-3 2.8-3 2.2 0 3.2 3 4 3s1.8-3 4-3c1.5 0 2.8 1.3 2.8 3s-1.3 3-2.8 3c-2.2 0-3.2-3-4-3s-1.8 3-4 3c-1.5 0-2.8-1.3-2.8-3z" />
  </svg>
);

/**
 * Business Logic（为什么需要）:
 *   充电/无限两段式切换器的充电档需要用电池填充比例直接表达剩余余额，
 *   让用户不点开设置也能一眼读出电量。
 *
 * Code Logic（做什么）:
 *   渲染 16x16 电池：外框 rect + 正极 stroke currentColor；
 *   内部填充条 fill=currentColor，宽度 = 8.5 * level（内腔 x 3..11.5），
 *   level 在组件内部 clamp 到 0..1，level<=0 时不渲染填充条。
 *   aria-hidden 由使用方按需透传。
 */
export const BatteryLevelIcon = ({ size, level, ...rest }: IconProps & { level: number }) => {
  const clamped = Math.max(0, Math.min(1, level));
  return (
    <svg {...baseProps(size)} {...rest}>
      <rect x="1.5" y="4.5" width="11.5" height="7" rx="1.4" />
      <path d="M13.8 6.6v2.8" />
      {clamped > 0 ? (
        <rect
          x="3"
          y="6"
          width={8.5 * clamped}
          height="4"
          rx="0.75"
          fill="currentColor"
          stroke="none"
        />
      ) : null}
    </svg>
  );
};

/**
 * TokenIcon — Token 统计侧栏入口图标
 *
 * Business Logic（为什么需要这个组件):
 *   侧栏 System 组在 ActivityStats 之后插入 Token Stats 入口；为与现有 token 计量视觉
 *   对应，使用中心圆加三道等距横线的轻量象形，避开现成货币/键盘符号以避免与
 *   ActivityIcon / ProviderManagerIcon 撞形。
 *
 * Code Logic（这个组件做什么):
 *   16×16 viewBox、currentColor stroke 与 baseProps 同源；圆心 (8,8) 半径 5.5，
 *   三条横线 y=6.5/8.5/10.5 各长 5 个单位。
 */
export const TokenIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="8" cy="8" r="5.5" />
    <path d="M5.5 6.5h5" />
    <path d="M5.5 8.5h5" />
    <path d="M5.5 10.5h5" />
  </svg>
);
