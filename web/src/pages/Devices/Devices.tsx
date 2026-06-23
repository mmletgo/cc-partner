/**
 * Devices 页面 - 局域网设备管理
 *
 * Business Logic（为什么需要这个页面）:
 *   局域网 P2P 是 cc-partner 的核心传输通道；用户需要把当前网络上
 *   通过 mDNS 自动发现的对端设备直接作为 SSH 连接目标管理，同时知道
 *   本机在网络中的标识（方便区分"自己"）。本机信息单独高亮，以便用户
 *   在多设备场景下立即定位自己。
 *
 * Code Logic（这个页面做什么）:
 *   - 页面头部展示标题 + 在线数量 Pill + 副标题 + 手动刷新按钮
 *   - 顶部 outlined Card 显示本机信息（通过 /api/health 获取，设备名 + IP + 端口 + 状态）
 *   - SSH 目标管理是设备页主操作区：局域网设备自动成为连接目标，也可手动添加 IP；
 *     行内保存 username/port，一键复制 ssh 命令，配置可跨设备同步
 *   - 启动后用 setInterval 每 5 秒拉取一次设备列表；
 *     卸载时清除 interval 防止内存泄漏
 *   - 容器居中、限宽，保证在大窗口下也保持可读
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Pill, Button, Input } from '@/components/primitives';
import { devicesApi } from '@/api/devices';
import type { HealthResponse } from '@/api/devices';
import { sshApi } from '@/api/ssh';
import {
  SyncIcon,
  AlertIcon,
  CopyIcon,
  TrashIcon,
  PlusIcon,
} from '@/lib/icons';
import type { Device, SshTarget, OsInfo } from '@/lib/types';
import styles from './Devices.module.css';

/** 页面刷新间隔（ms） */
const REFRESH_INTERVAL_MS = 5000;

/** 本机设备信息（从 health API 响应转换而来） */
interface SelfDeviceInfo {
  id: string;
  name: string;
  address: string;
  port: number;
  status: 'online' | 'offline';
}

/** SSH 目标列表渲染行：实时设备与手动目标的合并结果 */
interface SshTargetRow {
  host: string;
  username: string;
  port: number;
  label?: string;
  online: boolean;
  deviceName?: string;
}

/**
 * 按 username/port 拼接 ssh 连接命令。
 *
 * Business Logic（为什么需要）:
 *   设备页内的一键复制需要产出可直接粘贴到终端的 SSH 命令，减少用户记 IP/端口的成本。
 *
 * Code Logic（做什么）:
 *   用户名为空时使用本机默认用户名；端口为 22 时省略 `-p`；最后压缩多余空白。
 */
function buildSshCommand(host: string, username: string, port: number): string {
  const userPart = username.trim() ? `${username.trim()}@` : '';
  const portPart = port === 22 ? '' : `-p ${port} `;
  return `ssh ${portPart}${userPart}${host}`.replace(/\s+/g, ' ').trim();
}

/**
 * 将后端 health 响应（snake_case）转换为前端 SelfDeviceInfo（camelCase）
 *
 * @param resp - 后端 /api/health 返回的原始数据
 * @returns 前端使用的本机设备信息
 */
function toSelfDevice(resp: HealthResponse): SelfDeviceInfo {
  return {
    id: resp.device_id,
    name: resp.device_name,
    address: '127.0.0.1',
    port: resp.http_port,
    status: 'online',
  };
}

/**
 * Devices 页面组件
 *
 * @returns Devices 路由的根容器
 */
export function Devices() {
  const { t } = useTranslation(['devices', 'ssh', 'common']);
  const [devices, setDevices] = useState<Device[]>([]);
  const [selfDevice, setSelfDevice] = useState<SelfDeviceInfo | null>(null);
  const [selfLoading, setSelfLoading] = useState<boolean>(true);
  const [selfError, setSelfError] = useState<string | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [targets, setTargets] = useState<SshTarget[]>([]);
  const [osInfo, setOsInfo] = useState<OsInfo | null>(null);
  const [sshLoading, setSshLoading] = useState<boolean>(true);
  const [sshError, setSshError] = useState<string | null>(null);
  const [copiedHost, setCopiedHost] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<boolean>(false);
  const [syncMsg, setSyncMsg] = useState<string | null>(null);
  const [manualHost, setManualHost] = useState<string>('');
  const [manualUser, setManualUser] = useState<string>('');
  const [manualPort, setManualPort] = useState<string>('22');
  const [manualLabel, setManualLabel] = useState<string>('');
  const [edits, setEdits] = useState<Record<string, { username: string; port: string }>>({});
  const [tick, setTick] = useState<number>(0);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  /**
   * 从 /api/health 获取本机设备信息
   */
  const fetchSelfDevice = useCallback(async () => {
    try {
      setSelfLoading(true);
      setSelfError(null);
      const resp = await devicesApi.health();
      setSelfDevice(toSelfDevice(resp));
    } catch (err) {
      setSelfError(err instanceof Error ? err.message : t('devices:fetchSelfFailed'));
    } finally {
      setSelfLoading(false);
    }
  }, [t]);

  /**
   * 拉取设备列表；按后端契约映射为前端 Device 类型。
   * API 错误以空列表 + 错误提示形式呈现，不会阻塞 UI。
   */
  const fetchDevices = useCallback(async () => {
    try {
      const list = await devicesApi.list();
      setDevices(list);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('devices:fetchListFailed'));
    } finally {
      setLoading(false);
    }
  }, [t]);

  /**
   * 拉取 SSH 配置与本机系统信息。
   *
   * Business Logic（为什么需要）:
   *   设备页要同时管理连接目标，需把持久 SSH 目标与系统指南数据拉到同一个页面。
   *
   * Code Logic（做什么）:
   *   并发请求 list_ssh_targets 与 get_os_info；失败时仅影响 SSH 区块，不阻断设备列表。
   */
  const fetchSshConfig = useCallback(async () => {
    try {
      setSshError(null);
      const [nextTargets, nextOsInfo] = await Promise.all([
        sshApi.list(),
        sshApi.getOsInfo(),
      ]);
      setTargets(nextTargets);
      setOsInfo(nextOsInfo);
    } catch (err) {
      setSshError(err instanceof Error ? err.message : t('ssh:fetchFailed'));
    } finally {
      setSshLoading(false);
    }
  }, [t]);

  // 首次挂载时获取本机信息
  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    fetchSelfDevice();
  }, [fetchSelfDevice]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // 首次挂载时获取 SSH 目标与本机系统信息
  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    fetchSshConfig();
  }, [fetchSshConfig]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // 首次挂载 + tick 变化时重新拉取设备列表
  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    fetchDevices();
  }, [fetchDevices, tick]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // 启动 5s 定时器，定期刷新设备列表
  useEffect(() => {
    intervalRef.current = setInterval(() => {
      setTick((prev) => prev + 1);
    }, REFRESH_INTERVAL_MS);
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, []);

  const onlineCount = devices.filter((d) => d.status === 'online').length;

  const targetByHost = useMemo(() => new Map(targets.map((target) => [target.host, target])), [
    targets,
  ]);

  /**
   * 合并 SSH 连接目标。
   *
   * Business Logic（为什么需要）:
   *   局域网发现的设备应该天然出现在连接目标里；手动添加过但当前未发现的 IP 仍应保留。
   *
   * Code Logic（做什么）:
   *   先按实时设备生成在线行，再补入 targets 中剩余 host，按 host 去重。
   */
  const sshTargetRows = useMemo<SshTargetRow[]>(() => {
    const seen = new Set<string>();
    const rows: SshTargetRow[] = [];

    for (const device of devices) {
      if (seen.has(device.address)) continue;
      seen.add(device.address);
      const cfg = targetByHost.get(device.address);
      rows.push({
        host: device.address,
        username: cfg?.username ?? '',
        port: cfg?.port ?? 22,
        label: cfg?.label,
        online: device.status === 'online',
        deviceName: device.name,
      });
    }

    for (const target of targets) {
      if (seen.has(target.host)) continue;
      seen.add(target.host);
      rows.push({
        host: target.host,
        username: target.username,
        port: target.port,
        label: target.label,
        online: false,
      });
    }

    return rows;
  }, [devices, targetByHost, targets]);

  /**
   * 保存 SSH 连接目标。
   *
   * Business Logic（为什么需要）:
   *   用户在设备页编辑用户名/端口后，配置要立刻落库并参与跨设备同步。
   *
   * Code Logic（做什么）:
   *   调 upsert_ssh_target 保存，然后刷新 targets，保持列表与后端一致。
   */
  const saveTarget = useCallback(
    async (host: string, username: string, port: number, label?: string) => {
      try {
        await sshApi.upsert(host, username, port, label);
        const nextTargets = await sshApi.list();
        setTargets(nextTargets);
        setSshError(null);
      } catch (err) {
        setSshError(err instanceof Error ? err.message : t('ssh:fetchFailed'));
      }
    },
    [t],
  );

  /**
   * 手动触发刷新：递增 tick，下一次 effect 会重新拉取
   */
  const handleRefresh = () => {
    setLoading(true);
    setSshLoading(true);
    setTick((prev) => prev + 1);
    void fetchSshConfig();
  };

  /**
   * 提交某个 SSH 目标的行内编辑。
   *
   * Business Logic（为什么需要）:
   *   用户编辑 username/port 后在失焦或回车时自动保存，避免额外确认按钮增加操作成本。
   *
   * Code Logic（做什么）:
   *   从 edits 取缓存，保留原 label，端口非法时回落 22，提交后删除该行编辑缓存。
   */
  const commitSshEdit = (host: string) => {
    const edit = edits[host];
    if (!edit) return;
    const cfg = targetByHost.get(host);
    void saveTarget(host, edit.username, Number(edit.port) || 22, cfg?.label);
    setEdits((prev) => {
      const next = { ...prev };
      delete next[host];
      return next;
    });
  };

  /**
   * 复制 SSH 命令到剪贴板。
   *
   * Business Logic（为什么需要）:
   *   用户可以从设备页直接拿到终端命令，不需要手动拼 host/user/port。
   *
   * Code Logic（做什么）:
   *   使用 Clipboard API 写入命令，并短暂标记 copiedHost 展示反馈。
   */
  const handleCopySsh = async (host: string, username: string, port: number) => {
    try {
      await navigator.clipboard.writeText(buildSshCommand(host, username, port));
      setCopiedHost(host);
      window.setTimeout(() => setCopiedHost(null), 1500);
    } catch {
      // 剪贴板不可用时保持静默，避免阻断用户继续编辑。
    }
  };

  /**
   * 删除 SSH 目标。
   *
   * Business Logic（为什么需要）:
   *   手动添加或不再使用的连接目标应可从设备页移除。
   *
   * Code Logic（做什么）:
   *   调 delete_ssh_target 软删除，再刷新 targets；实时发现设备仍会保留为在线行。
   */
  const handleDeleteSshTarget = async (host: string) => {
    try {
      await sshApi.remove(host);
      const nextTargets = await sshApi.list();
      setTargets(nextTargets);
      setSshError(null);
    } catch (err) {
      setSshError(err instanceof Error ? err.message : t('ssh:fetchFailed'));
    }
  };

  /**
   * 手动添加 SSH 目标。
   *
   * Business Logic（为什么需要）:
   *   mDNS 发现不到的机器仍可通过 IP/hostname 加入连接目标列表。
   *
   * Code Logic（做什么）:
   *   host 去空后 upsert；成功后清空表单，失败由 saveTarget 写入 sshError。
   */
  const handleAddSshTarget = async () => {
    const host = manualHost.trim();
    if (!host) return;
    await saveTarget(
      host,
      manualUser.trim(),
      Number(manualPort) || 22,
      manualLabel.trim() || undefined,
    );
    setManualHost('');
    setManualUser('');
    setManualPort('22');
    setManualLabel('');
  };

  /**
   * 触发跨设备同步。
   *
   * Business Logic（为什么需要）:
   *   SSH 目标配置需要在多设备间同步，用户可在设备页主动触发一次。
   *
   * Code Logic（做什么）:
   *   复用 trigger_sync；完成后刷新 SSH targets 并展示 3 秒反馈。
   */
  const handleSyncSshConfig = async () => {
    setSyncing(true);
    setSyncMsg(null);
    try {
      const result = await sshApi.sync();
      setSyncMsg(
        result.synced > 0 ? t('ssh:synced', { count: result.synced }) : t('ssh:syncNoDevices'),
      );
      await fetchSshConfig();
    } catch {
      setSyncMsg(t('ssh:syncFailed'));
    } finally {
      setSyncing(false);
      window.setTimeout(() => setSyncMsg(null), 3000);
    }
  };

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        {/* 页面头部 */}
        <header className={styles.header}>
          <div className={styles.headerText}>
            <h1 className={styles.title}>
              {t('devices:title')}
              <Pill tone={onlineCount > 0 ? 'success' : 'neutral'} className={styles.countPill}>
                {t('devices:onlineCount', { count: onlineCount })}
              </Pill>
            </h1>
            <p className={styles.lead}>{t('devices:desc')}</p>
          </div>
          <div className={styles.headerActions}>
            <Button
              variant="secondary"
              size="sm"
              icon={<SyncIcon />}
              onClick={handleRefresh}
              loading={loading || sshLoading}
            >
              {t('common:action.refresh')}
            </Button>
          </div>
        </header>

        {/* 本机信息 */}
        <Card variant="outlined" padding="md" className={styles.selfCard}>
          {selfLoading ? (
            <div className={styles.selfRow}>
              <div className={styles.selfLabel}>
                <span className={styles.selfDot} aria-hidden="true" />
                <span>{t('devices:localInfo')}</span>
              </div>
              <div className={styles.selfMeta}>
                <span className={styles.selfName}>{t('devices:loading')}</span>
              </div>
            </div>
          ) : selfError ? (
            <div className={styles.selfRow}>
              <div className={styles.selfLabel}>
                <span className={styles.selfDot} aria-hidden="true" />
                <span>{t('devices:localInfo')}</span>
              </div>
              <div className={styles.selfMeta}>
                <span className={styles.selfName} style={{ color: 'var(--color-danger)' }}>
                  {selfError}
                </span>
                <span className={styles.selfSep}>·</span>
                <Button variant="secondary" size="sm" onClick={fetchSelfDevice}>
                  {t('common:action.retry')}
                </Button>
              </div>
            </div>
          ) : selfDevice ? (
            <div className={styles.selfRow}>
              <div className={styles.selfLabel}>
                <span className={styles.selfDot} aria-hidden="true" />
                <span>{t('devices:localInfo')}</span>
              </div>
              <div className={styles.selfMeta}>
                <span className={styles.selfName}>{selfDevice.name}</span>
                <span className={styles.selfSep}>·</span>
                <span className={styles.selfMono}>
                  {selfDevice.address}:{selfDevice.port}
                </span>
                <span className={styles.selfSep}>·</span>
                <Pill tone="accent" className={styles.selfPill}>
                  {t('common:status.device.online')}
                </Pill>
              </div>
            </div>
          ) : null}
        </Card>

        {/* 错误提示 */}
        {error ? (
          <div className={styles.errorBox} role="alert">
            <AlertIcon />
            <span>{error}</span>
          </div>
        ) : null}

        {/* SSH 连接目标 */}
        <Card variant="outlined" padding="md" className={styles.sshCard}>
          <div className={styles.sectionHeader}>
            <div className={styles.sectionTitleGroup}>
              <h2 className={styles.sectionTitle}>{t('ssh:targetsSection')}</h2>
              {osInfo ? (
                <Pill tone="neutral" className={styles.osPill}>
                  {t('ssh:localOs')}: {osInfo.platform}
                </Pill>
              ) : null}
            </div>
            <div className={styles.sectionActions}>
              {syncMsg ? <span className={styles.syncMsg}>{syncMsg}</span> : null}
              <Button
                variant="secondary"
                size="sm"
                icon={<SyncIcon />}
                onClick={handleSyncSshConfig}
                loading={syncing}
              >
                {syncing ? t('ssh:syncing') : t('ssh:syncConfig')}
              </Button>
            </div>
          </div>

          {sshError ? (
            <div className={styles.errorBox} role="alert">
              <AlertIcon />
              <span>{sshError}</span>
              <Button variant="secondary" size="sm" onClick={fetchSshConfig}>
                {t('ssh:retry')}
              </Button>
            </div>
          ) : null}

          {(sshLoading || loading) && sshTargetRows.length === 0 ? (
            <div className={styles.empty}>
              <SyncIcon /> {t('ssh:loading')}
            </div>
          ) : sshTargetRows.length === 0 ? (
            <div className={styles.empty}>
              <p className={styles.emptyTitle}>{t('ssh:emptyTitle')}</p>
              <span className={styles.emptyHint}>{t('ssh:emptyHint')}</span>
            </div>
          ) : (
            <div className={styles.targetList}>
              {sshTargetRows.map((row) => {
                const edit = edits[row.host];
                const username = edit?.username ?? row.username;
                const port = edit?.port ?? String(row.port);
                return (
                  <div className={styles.targetRow} key={row.host}>
                    <span className={styles.targetHost} title={row.deviceName ?? row.label}>
                      {row.host}
                      {!row.online ? (
                        <span className={styles.targetHostMeta}> {t('ssh:offlineSuffix')}</span>
                      ) : null}
                    </span>
                    <Input
                      size="sm"
                      value={username}
                      placeholder={t('ssh:manualUsernamePh')}
                      onChange={(e) =>
                        setEdits((prev) => ({
                          ...prev,
                          [row.host]: {
                            username: e.target.value,
                            port: prev[row.host]?.port ?? String(row.port),
                          },
                        }))
                      }
                      onBlur={() => commitSshEdit(row.host)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') commitSshEdit(row.host);
                      }}
                    />
                    <Input
                      type="number"
                      size="sm"
                      mono
                      value={port}
                      onChange={(e) =>
                        setEdits((prev) => ({
                          ...prev,
                          [row.host]: {
                            username: prev[row.host]?.username ?? row.username,
                            port: e.target.value,
                          },
                        }))
                      }
                      onBlur={() => commitSshEdit(row.host)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') commitSshEdit(row.host);
                      }}
                    />
                    <span className={styles.targetLabel}>{row.label ?? row.deviceName ?? ''}</span>
                    <div className={styles.targetActions}>
                      <Button
                        variant="secondary"
                        size="sm"
                        icon={<CopyIcon />}
                        onClick={() => handleCopySsh(row.host, username, Number(port) || 22)}
                      >
                        {copiedHost === row.host ? t('ssh:copied') : t('ssh:copy')}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        icon={<TrashIcon />}
                        onClick={() => handleDeleteSshTarget(row.host)}
                        aria-label={t('ssh:delete')}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          <div className={styles.manualRow}>
            <Input
              size="sm"
              value={manualHost}
              placeholder={t('ssh:manualHostPh')}
              onChange={(e) => setManualHost(e.target.value)}
            />
            <Input
              size="sm"
              value={manualUser}
              placeholder={t('ssh:manualUsernamePh')}
              onChange={(e) => setManualUser(e.target.value)}
            />
            <Input
              type="number"
              size="sm"
              mono
              value={manualPort}
              placeholder={t('ssh:manualPortPh')}
              onChange={(e) => setManualPort(e.target.value)}
            />
            <Input
              size="sm"
              value={manualLabel}
              placeholder={t('ssh:manualLabelPh')}
              onChange={(e) => setManualLabel(e.target.value)}
            />
            <Button variant="primary" size="sm" icon={<PlusIcon />} onClick={handleAddSshTarget}>
              {t('ssh:add')}
            </Button>
          </div>
        </Card>

        {/* SSH 配置指南 */}
        <Card variant="outlined" padding="md" className={styles.guideCard}>
          <h2 className={styles.sectionTitle}>{t('ssh:guideSection')}</h2>
          <div className={styles.guideBlock}>
            <span className={styles.guideLabel}>
              {t('ssh:guideLocalTitle')} ·{' '}
              {osInfo?.platform === 'mac'
                ? t('ssh:guideMac')
                : osInfo?.platform === 'windows'
                  ? t('ssh:guideWindows')
                  : t('ssh:guideUbuntu')}
            </span>
            <span className={styles.guideText}>
              {osInfo?.platform === 'mac'
                ? t('ssh:guideLocalMac')
                : osInfo?.platform === 'windows'
                  ? t('ssh:guideLocalWindows')
                  : t('ssh:guideLocalUbuntu')}
            </span>
          </div>
          <div className={styles.guideBlock}>
            <span className={styles.guideLabel}>{t('ssh:guideRemoteTitle')}</span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideMac')}:</strong> {t('ssh:guideRemoteMac')}
            </span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideUbuntu')}:</strong> {t('ssh:guideRemoteUbuntu')}
            </span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideWindows')}:</strong> {t('ssh:guideRemoteWindows')}
            </span>
          </div>
        </Card>
      </div>
    </div>
  );
}

Devices.displayName = 'Devices';
