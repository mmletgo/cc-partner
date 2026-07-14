/**
 * Settings 依赖环境面板
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在依赖环境 tab 查看/请求 macOS 权限，并查看 Workbench tmux 与局域网防火墙依赖；
 *   编排留在 controller，本组件只渲染。
 *
 * Code Logic（这个组件做什么）:
 *   渲染权限 Card（含 mapPermissions 列表）、RuntimeDiagnosticsCard、
 *   WorkbenchDependencyCard / LanFirewallDependencyCard；不直接 import @/api 或 invoke。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button } from '@/components/primitives';
import {
  LanFirewallDependencyCard,
  PermissionCard,
  RuntimeDiagnosticsCard,
  WorkbenchDependencyCard,
} from '@/components/domain';
import { mapPermissions } from '@/lib/permissionEntries';
import type { PermissionType, PermissionsStatus } from '@/lib/types';
import styles from './Settings.module.css';

/**
 * 依赖环境面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   权限状态由 usePermissions 在 controller 中轮询；panel 仅消费投影结果。
 *
 * Code Logic（这个接口做什么）:
 *   声明权限 status/loading/error/requesting 与 request/refresh 回调。
 */
export interface SettingsDependenciesPanelProps {
  permStatus: PermissionsStatus | null;
  permLoading: boolean;
  permRefreshing: boolean;
  permError: string | null;
  permRequesting: ReadonlySet<PermissionType> | Set<PermissionType>;
  onRequestAccess: (type: PermissionType) => void;
  onRefreshPermissions: () => void;
}

/**
 * 依赖环境设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   依赖环境 tab 需要独立 pure 视图，避免 Settings 巨型 JSX 继续膨胀。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation(settings/welcome) 置顶；按 loading/error/ready 三态渲染权限卡，并挂载依赖卡片。
 *
 * @param props 权限状态与动作
 * @returns 依赖环境 tab 内容
 */
export function SettingsDependenciesPanel({
  permStatus,
  permLoading,
  permRefreshing,
  permError,
  permRequesting,
  onRequestAccess,
  onRefreshPermissions,
}: SettingsDependenciesPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);
  const [tWelcome] = useTranslation('welcome');

  return (
    <>
{/* Card: 权限管理（macOS 手动授权入口） */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:permission.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    {permLoading ? (
      <p className={styles.helper}>{t('settings:permission.checking')}</p>
    ) : !permStatus ? (
      <div className={styles.permissionList}>
        <p className={styles.helper} role="alert">
          {permError
            ? t('settings:permission.loadFailed', { error: permError })
            : t('settings:permission.loadFailed', {
                error: t('settings:permission.unknownError'),
              })}
        </p>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => void onRefreshPermissions()}
          loading={permRefreshing}
          aria-busy={permRefreshing}
        >
          {t('settings:permission.recheck')}
        </Button>
      </div>
    ) : (
      <div className={styles.permissionList}>
        {permError ? (
          <p className={styles.helper} role="alert">
            {t('settings:permission.loadFailed', { error: permError })}
          </p>
        ) : null}
        {mapPermissions(permStatus, tWelcome).map((p) => (
          <PermissionCard
            key={p.id}
            icon={p.icon}
            title={p.title}
            description={p.description}
            granted={p.granted}
            requesting={permRequesting.has(p.id as PermissionType)}
            onRequestAccess={() => onRequestAccess(p.id as PermissionType)}
          />
        ))}
        <div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void onRefreshPermissions()}
            loading={permRefreshing}
            aria-busy={permRefreshing}
          >
            {t('settings:permission.recheck')}
          </Button>
        </div>
      </div>
    )}
  </Card.Body>
</Card>

<RuntimeDiagnosticsCard />
<WorkbenchDependencyCard />
<LanFirewallDependencyCard />
    </>
  );
}

SettingsDependenciesPanel.displayName = 'SettingsDependenciesPanel';
