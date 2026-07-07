/**
 * RemoteAssetPicker 业务组件 - 局域网远端资产选择器
 *
 * Business Logic（为什么需要这个组件）:
 *   用户从局域网设备拉取 Claude Code assets 时，必须能逐项选择或对当前筛选列表全选，避免全量覆盖。
 *
 * Code Logic（这个组件做什么）:
 *   接收远端 assets、已选 key 集合与筛选条件；渲染全选/清空按钮和可勾选的 ClaudeAssetRow 列表。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { CheckIcon, XIcon } from '@/lib/icons';
import type { ClaudeCodeAsset } from '@/lib/types';
import { matchesClaudeCodeAsset } from '@/lib/claudeCodeAssets';
import type { EnabledFilter, KindFilter } from '@/lib/claudeCodeAssets';
import { ClaudeAssetRow } from '../ClaudeAssetRow';
import { remoteAssetKey } from './remoteAssetKey';
import styles from './RemoteAssetPicker.module.css';

export interface RemoteAssetPickerProps {
  assets: ClaudeCodeAsset[];
  selectedKeys: Set<string>;
  kind: KindFilter;
  search: string;
  enabledFilter: EnabledFilter;
  onSelect: (asset: ClaudeCodeAsset, checked: boolean) => void;
  onSelectMany: (assets: ClaudeCodeAsset[], checked: boolean) => void;
}

/**
 * 渲染局域网远端资产选择器。
 */
export function RemoteAssetPicker({
  assets,
  selectedKeys,
  kind,
  search,
  enabledFilter,
  onSelect,
  onSelectMany,
}: RemoteAssetPickerProps) {
  const { t } = useTranslation(['claudeCodeAssets']);
  const visible = useMemo(
    () => assets.filter((asset) => matchesClaudeCodeAsset(asset, kind, search, enabledFilter)),
    [assets, kind, search, enabledFilter],
  );
  const selectableVisible = visible.filter((asset) => asset.canExport);

  return (
    <div className={styles.picker}>
      <div className={styles.toolbar}>
        <span className={styles.count}>
          {t('claudeCodeAssets:remoteVisible', {
            count: visible.length,
            selected: selectedKeys.size,
          })}
        </span>
        <div className={styles.actions}>
          <Button
            variant="ghost"
            size="sm"
            icon={<CheckIcon />}
            disabled={selectableVisible.length === 0}
            onClick={() => onSelectMany(selectableVisible, true)}
          >
            {t('claudeCodeAssets:selectVisible')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            icon={<XIcon />}
            disabled={selectedKeys.size === 0}
            onClick={() => onSelectMany(visible, false)}
          >
            {t('claudeCodeAssets:clearVisible')}
          </Button>
        </div>
      </div>
      <div className={styles.list}>
        {visible.length > 0 ? (
          visible.map((asset) => (
            <ClaudeAssetRow
              key={remoteAssetKey(asset)}
              asset={asset}
              selectable
              selected={selectedKeys.has(remoteAssetKey(asset))}
              onSelect={onSelect}
            />
          ))
        ) : (
          <div className={styles.empty}>{t('claudeCodeAssets:remoteEmpty')}</div>
        )}
      </div>
    </div>
  );
}
