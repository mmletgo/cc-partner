/**
 * Claude Code Assets 页面 - skills / plugins / MCP 管理
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在 cc-partner 中集中管理 Claude Code 个人级 assets，并从局域网设备选择性拉取。
 *
 * Code Logic（这个页面做什么）:
 *   - 拉取本机 assets，支持搜索、分类筛选、启停、卸载，并按类别展示启用/警告统计；
 *   - 用页内 tab 在本机资产与局域网拉取之间切换，搜索与类型筛选对两个 tab 均生效。
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { claudeCodeAssetsApi } from '@/api/claudeCodeAssets';
import { devicesApi } from '@/api/devices';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { ClaudeAssetRow, RemoteAssetPicker, remoteAssetKey } from '@/components/domain';
import { AlertIcon, DownloadIcon, SearchIcon, SyncIcon, TerminalIcon } from '@/lib/icons';
import type {
  ClaudeCodeAsset,
  ClaudeCodeAssetInstallReport,
  ClaudeCodeAssetKind,
  ClaudeCodeAssetSelector,
  Device,
} from '@/lib/types';
import styles from './ClaudeCodeAssets.module.css';

type KindFilter = ClaudeCodeAssetKind | 'all';
type AssetTab = 'remote' | 'local';

const KIND_OPTIONS: KindFilter[] = ['all', 'skill', 'command', 'plugin', 'mcp'];
const ASSET_TABS: AssetTab[] = ['local', 'remote'];

/**
 * 生成本地/远端资产 key。
 */
function assetKey(asset: ClaudeCodeAsset): string {
  return `${asset.kind}:${asset.id}`;
}

/**
 * 判断资产是否匹配筛选条件。
 */
function matchesAsset(asset: ClaudeCodeAsset, kind: KindFilter, search: string): boolean {
  const q = search.trim().toLowerCase();
  const matchesKind = kind === 'all' || asset.kind === kind;
  const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
  return matchesKind && (!q || haystack.includes(q));
}

/**
 * 把远端选中 key 转换为后端 selectors。
 */
function selectedSelectors(assets: ClaudeCodeAsset[], selectedKeys: Set<string>): ClaudeCodeAssetSelector[] {
  return assets
    .filter((asset) => selectedKeys.has(remoteAssetKey(asset)))
    .map((asset) => ({ kind: asset.kind, id: asset.id }));
}

/**
 * Claude Code Assets 页面组件。
 */
export function ClaudeCodeAssets() {
  const { t } = useTranslation(['claudeCodeAssets', 'common']);
  const [assets, setAssets] = useState<ClaudeCodeAsset[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  const [remoteAssets, setRemoteAssets] = useState<ClaudeCodeAsset[]>([]);
  const [kind, setKind] = useState<KindFilter>('all');
  const [search, setSearch] = useState<string>('');
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>('');
  const [selectedRemoteKeys, setSelectedRemoteKeys] = useState<Set<string>>(new Set());
  const [overwrite, setOverwrite] = useState<boolean>(false);
  const [activeTab, setActiveTab] = useState<AssetTab>('local');
  const [loading, setLoading] = useState<boolean>(true);
  const [remoteLoading, setRemoteLoading] = useState<boolean>(false);
  const [actionKey, setActionKey] = useState<string | null>(null);
  const [pulling, setPulling] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [report, setReport] = useState<ClaudeCodeAssetInstallReport | null>(null);

  /**
   * 刷新本机 assets。
   */
  const refreshAssets = useCallback(async () => {
    try {
      setError(null);
      const list = await claudeCodeAssetsApi.list();
      setAssets(list);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('claudeCodeAssets:loadFailed'));
    } finally {
      setLoading(false);
    }
  }, [t]);

  /**
   * 刷新局域网设备列表。
   */
  const refreshDevices = useCallback(async () => {
    try {
      const list = await devicesApi.list();
      setDevices(list);
      if (!selectedDeviceId && list[0]) {
        setSelectedDeviceId(list[0].id);
      }
    } catch {
      setDevices([]);
    }
  }, [selectedDeviceId]);

  /**
   * 拉取当前选中设备的远端 inventory。
   */
  const loadRemoteAssets = useCallback(async () => {
    if (!selectedDeviceId) return;
    try {
      setRemoteLoading(true);
      setRemoteError(null);
      setSelectedRemoteKeys(new Set());
      const list = await claudeCodeAssetsApi.listRemote(selectedDeviceId);
      setRemoteAssets(list);
    } catch (err) {
      setRemoteError(err instanceof Error ? err.message : t('claudeCodeAssets:remoteLoadFailed'));
      setRemoteAssets([]);
    } finally {
      setRemoteLoading(false);
    }
  }, [selectedDeviceId, t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 页面挂载时发起数据请求 */
  useEffect(() => {
    void refreshAssets();
    void refreshDevices();
  }, [refreshAssets, refreshDevices]);
  /* eslint-enable react-hooks/set-state-in-effect */

  const filteredAssets = useMemo(
    () => assets.filter((asset) => matchesAsset(asset, kind, search)),
    [assets, kind, search],
  );

  const counts = useMemo(() => {
    return KIND_OPTIONS.filter((option): option is ClaudeCodeAssetKind => option !== 'all').map((option) => {
      const kindAssets = assets.filter((asset) => asset.kind === option);
      return {
        kind: option,
        enabled: kindAssets.filter((asset) => asset.enabled).length,
        warnings: kindAssets.filter((asset) => asset.warnings.length > 0).length,
      };
    });
  }, [assets]);

  /**
   * 处理启停操作。
   */
  const handleToggle = async (asset: ClaudeCodeAsset) => {
    const key = assetKey(asset);
    try {
      setActionKey(key);
      setReport(await claudeCodeAssetsApi.setEnabled(asset.kind, asset.id, !asset.enabled));
      await refreshAssets();
    } catch (err) {
      setError(err instanceof Error ? err.message : t('claudeCodeAssets:actionFailed'));
    } finally {
      setActionKey(null);
    }
  };

  /**
   * 处理卸载操作。
   */
  const handleRemove = async (asset: ClaudeCodeAsset) => {
    if (!window.confirm(t('claudeCodeAssets:confirmUninstall', { name: asset.name }))) return;
    const key = assetKey(asset);
    try {
      setActionKey(key);
      setReport(await claudeCodeAssetsApi.uninstall(asset.kind, asset.id));
      await refreshAssets();
    } catch (err) {
      setError(err instanceof Error ? err.message : t('claudeCodeAssets:actionFailed'));
    } finally {
      setActionKey(null);
    }
  };

  /**
   * 更新远端选择状态。
   */
  const handleRemoteSelect = (asset: ClaudeCodeAsset, checked: boolean) => {
    setSelectedRemoteKeys((prev) => {
      const next = new Set(prev);
      const key = remoteAssetKey(asset);
      if (checked) next.add(key);
      else next.delete(key);
      return next;
    });
  };

  /**
   * 批量更新远端选择状态。
   */
  const handleRemoteSelectMany = (nextAssets: ClaudeCodeAsset[], checked: boolean) => {
    setSelectedRemoteKeys((prev) => {
      const next = new Set(prev);
      for (const asset of nextAssets) {
        const key = remoteAssetKey(asset);
        if (checked) next.add(key);
        else next.delete(key);
      }
      return next;
    });
  };

  /**
   * 拉取远端已选 assets。
   */
  const handlePull = async () => {
    if (!selectedDeviceId) return;
    const items = selectedSelectors(remoteAssets, selectedRemoteKeys);
    if (items.length === 0) return;
    try {
      setPulling(true);
      setRemoteError(null);
      setReport(await claudeCodeAssetsApi.pullRemote(selectedDeviceId, items, overwrite));
      setSelectedRemoteKeys(new Set());
      await refreshAssets();
    } catch (err) {
      setRemoteError(err instanceof Error ? err.message : t('claudeCodeAssets:pullFailed'));
    } finally {
      setPulling(false);
    }
  };

  const selectedDevice = devices.find((device) => device.id === selectedDeviceId);

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.headerText}>
            <h1 className={styles.title}>
              <TerminalIcon />
              {t('claudeCodeAssets:title')}
              <Pill tone="accent">{assets.length}</Pill>
            </h1>
            <div className={styles.kindSummary} aria-label={t('claudeCodeAssets:categorySummaryLabel')}>
              {counts.map((item) => (
                <span key={item.kind} className={styles.kindSummaryItem}>
                  <span className={styles.kindName}>{t(`claudeCodeAssets:kinds.${item.kind}`)}</span>
                  <span className={[styles.metric, item.enabled > 0 ? styles.metricSuccess : styles.metricNeutral].join(' ')}>
                    {t('claudeCodeAssets:enabledShort', { count: item.enabled })}
                  </span>
                  <span className={[styles.metric, item.warnings > 0 ? styles.metricWarn : styles.metricNeutral].join(' ')}>
                    {t('claudeCodeAssets:warningShort', { count: item.warnings })}
                  </span>
                </span>
              ))}
            </div>
          </div>
          <Button variant="secondary" icon={<SyncIcon />} loading={loading} onClick={refreshAssets}>
            {t('claudeCodeAssets:refresh')}
          </Button>
        </header>

        {error ? (
          <div className={styles.errorBox}>
            <AlertIcon />
            {error}
          </div>
        ) : null}

        {report ? (
          <Card variant="outlined" padding="sm" className={styles.report}>
            <Card.Body>
              <div className={styles.reportLine}>
                <span>{report.note}</span>
                <Button variant="ghost" size="sm" onClick={() => setReport(null)}>
                  {t('claudeCodeAssets:dismiss')}
                </Button>
              </div>
              {report.items.length > 0 ? (
                <div className={styles.reportItems}>
                  {report.items.map((item) => (
                    <span key={`${item.kind}:${item.id}:${item.status}`} className={styles.reportItem}>
                      {item.name}: {item.message}
                    </span>
                  ))}
                </div>
              ) : null}
            </Card.Body>
          </Card>
        ) : null}

        <div className={styles.tabs} role="tablist" aria-label={t('claudeCodeAssets:tabsLabel')}>
          {ASSET_TABS.map((tab) => (
            <button
              key={tab}
              type="button"
              role="tab"
              aria-selected={activeTab === tab}
              className={activeTab === tab ? styles.tabActive : styles.tab}
              onClick={() => setActiveTab(tab)}
            >
              {tab === 'remote' ? t('claudeCodeAssets:remoteTitle') : t('claudeCodeAssets:localTitle')}
            </button>
          ))}
        </div>

        <Card variant="outlined" className={styles.toolsCard}>
          <Card.Body>
            <div className={styles.filters}>
              <Input
                icon={<SearchIcon />}
                value={search}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setSearch(e.currentTarget.value)}
                placeholder={t('claudeCodeAssets:searchPlaceholder')}
                aria-label={t('claudeCodeAssets:searchPlaceholder')}
              />
              <div className={styles.segmented}>
                {KIND_OPTIONS.map((option) => (
                  <button
                    key={option}
                    type="button"
                    className={option === kind ? styles.segmentActive : styles.segment}
                    onClick={() => setKind(option)}
                  >
                    {option === 'all' ? t('claudeCodeAssets:allKinds') : t(`claudeCodeAssets:kinds.${option}`)}
                  </button>
                ))}
              </div>
            </div>
          </Card.Body>
        </Card>

        {activeTab === 'remote' ? (
          <Card variant="outlined" className={styles.remotePanel}>
            <Card.Header>
              <div className={styles.sectionHeader}>
                <h2>{t('claudeCodeAssets:remoteTitle')}</h2>
                {selectedDevice ? <Pill tone="accent">{selectedDevice.name}</Pill> : null}
              </div>
            </Card.Header>
            <Card.Body>
              <div className={styles.remoteControls}>
                <label className={styles.field}>
                  <span>{t('claudeCodeAssets:deviceLabel')}</span>
                  <select value={selectedDeviceId} onChange={(e) => setSelectedDeviceId(e.currentTarget.value)}>
                    <option value="">{t('claudeCodeAssets:selectDevice')}</option>
                    {devices.map((device) => (
                      <option key={device.id} value={device.id}>
                        {device.name} · {device.address}
                      </option>
                    ))}
                  </select>
                </label>
                <Button
                  variant="secondary"
                  icon={<SyncIcon />}
                  loading={remoteLoading}
                  disabled={!selectedDeviceId}
                  onClick={loadRemoteAssets}
                >
                  {t('claudeCodeAssets:loadRemote')}
                </Button>
                <label className={styles.checkLine}>
                  <input
                    type="checkbox"
                    checked={overwrite}
                    onChange={(e) => setOverwrite(e.currentTarget.checked)}
                  />
                  {t('claudeCodeAssets:overwrite')}
                </label>
                <Button
                  variant="primary"
                  icon={<DownloadIcon />}
                  loading={pulling}
                  disabled={!selectedDeviceId || selectedRemoteKeys.size === 0}
                  onClick={handlePull}
                >
                  {t('claudeCodeAssets:pullSelected', { count: selectedRemoteKeys.size })}
                </Button>
              </div>
              {remoteError ? (
                <div className={styles.errorBox}>
                  <AlertIcon />
                  {remoteError}
                </div>
              ) : null}
              <RemoteAssetPicker
                assets={remoteAssets}
                selectedKeys={selectedRemoteKeys}
                kind={kind}
                search={search}
                onSelect={handleRemoteSelect}
                onSelectMany={handleRemoteSelectMany}
              />
            </Card.Body>
          </Card>
        ) : (
          <Card variant="flat" className={styles.panel}>
            <Card.Header>
              <div className={styles.sectionHeader}>
                <h2>{t('claudeCodeAssets:localTitle')}</h2>
                <Pill tone="neutral">{filteredAssets.length}</Pill>
              </div>
            </Card.Header>
            <Card.Body>
              <div className={styles.assetList}>
                {loading && assets.length === 0 ? (
                  <div className={styles.empty}>{t('claudeCodeAssets:loading')}</div>
                ) : filteredAssets.length > 0 ? (
                  filteredAssets.map((asset) => (
                    <ClaudeAssetRow
                      key={assetKey(asset)}
                      asset={asset}
                      busy={actionKey === assetKey(asset)}
                      onToggle={handleToggle}
                      onRemove={handleRemove}
                    />
                  ))
                ) : (
                  <div className={styles.empty}>{t('claudeCodeAssets:empty')}</div>
                )}
              </div>
            </Card.Body>
          </Card>
        )}
      </div>
    </div>
  );
}
