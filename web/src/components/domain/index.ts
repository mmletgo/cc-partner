/**
 * Domain 业务组件统一入口（barrel）
 *
 * Business Logic（为什么需要这个入口）:
 *   页面层可按需 import 轻量业务组件，避免关心具体子路径。
 *   重型 Workbench 编辑器/预览/工作区不得从本 barrel 同步 re-export，
 *   否则 eager 消费者（如 AppShell）会把 CodeMirror/Tiptap 打入 main initial graph。
 *
 * Code Logic（这个入口做什么）:
 *   只 re-export 侧栏/卡片等轻量组件及其类型；Workbench 重模块请深路径 import：
 *   `@/components/domain/WorkbenchFileWorkspace` 等。
 */

export { PromptCard } from './PromptCard';
export type { PromptCardProps, PromptCardPrompt } from './PromptCard';

export { TransferItem } from './TransferItem';
export type {
  TransferItemProps,
  TransferItemTask,
  TransferDirection,
  TransferStatus,
} from './TransferItem';

export { PermissionCard } from './PermissionCard';
export type { PermissionCardProps } from './PermissionCard';

export { TagInput } from './TagInput';
export type { TagInputProps } from './TagInput';

export {
  VersionHistoryDrawer,
  resolveVersionCopyText,
} from './VersionHistoryDrawer';
export type {
  VersionHistoryDrawerProps,
  VersionHistoryNamespace,
} from './VersionHistoryDrawer';

export { PermissionStatusBadge } from './PermissionStatusBadge';

export { CcHistoryCard } from './CcHistoryCard';
export type { CcHistoryCardProps } from './CcHistoryCard';

export { GithubRepoCard } from './GithubRepoCard';
export type { GithubRepoCardProps } from './GithubRepoCard';

export { MobileAccessCard } from './MobileAccessCard';
export type { MobileAccessCardProps } from './MobileAccessCard';

export { AgentAssetRow } from './AgentAssetRow';
export type { AgentAssetRowProps } from './AgentAssetRow';

export { WorkbenchProjectRail } from './WorkbenchProjectRail';
export { WorkbenchDependencyCard } from './WorkbenchDependencyCard';
export { LanFirewallDependencyCard } from './LanFirewallDependencyCard';
export type { LanFirewallDependencyCardProps } from './LanFirewallDependencyCard';
export { RuntimeDiagnosticsCard } from './RuntimeDiagnosticsCard';
export { CcSwitchCliDependencyCard } from './CcSwitchCliDependencyCard';
export type { CcSwitchCliDependencyCardProps } from './CcSwitchCliDependencyCard';
