/**
 * Domain 业务组件统一入口（barrel）
 *
 * Business Logic（为什么需要这个入口）:
 *   页面层（pages/）按需 import 业务组件时只需 `import { PromptCard, DeviceCard } from '@/components/domain'`，
 *   避免页面层关心具体子路径。
 *
 * Code Logic（这个入口做什么）:
 *   统一 re-export 业务组件及其类型。
 */

export { PromptCard } from './PromptCard';
export type { PromptCardProps, PromptCardPrompt } from './PromptCard';

export { DeviceCard } from './DeviceCard';
export type { DeviceCardProps, DeviceCardDevice, DeviceStatus } from './DeviceCard';

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

export { PermissionStatusBadge } from './PermissionStatusBadge';

export { CcHistoryCard } from './CcHistoryCard';
export type { CcHistoryCardProps } from './CcHistoryCard';

export { GithubRepoCard } from './GithubRepoCard';
export type { GithubRepoCardProps } from './GithubRepoCard';

export { ClaudeAssetRow } from './ClaudeAssetRow';
export type { ClaudeAssetRowProps } from './ClaudeAssetRow';

export { RemoteAssetPicker, remoteAssetKey } from './RemoteAssetPicker';
export type { RemoteAssetPickerProps } from './RemoteAssetPicker';

export { WorkbenchProjectRail } from './WorkbenchProjectRail';
export { WorkbenchDependencyCard } from './WorkbenchDependencyCard';
export { WorkbenchImagePreview } from './WorkbenchImagePreview';
export type { WorkbenchImagePreviewProps } from './WorkbenchImagePreview';
export { WorkbenchCsvPreview } from './WorkbenchCsvPreview';
export type { WorkbenchCsvPreviewProps } from './WorkbenchCsvPreview';
export { WorkbenchSqlitePreview } from './WorkbenchSqlitePreview';
export type { WorkbenchSqlitePreviewProps } from './WorkbenchSqlitePreview';
export { WorkbenchFileWorkspace } from './WorkbenchFileWorkspace';
export type {
  WorkbenchFileWorkspaceProps,
  WorkbenchOpenFileTab,
} from './WorkbenchFileWorkspace';
