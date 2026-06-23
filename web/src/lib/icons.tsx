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

export const SettingsIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <circle cx="8" cy="8" r="2" />
    <path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8 3.4 3.4" />
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

export const HealthIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M1.5 8h2l1.5-3.5L7.5 12 10 5l1.5 3h3" />
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
