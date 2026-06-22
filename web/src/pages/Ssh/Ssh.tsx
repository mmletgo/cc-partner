/**
 * Ssh 页面 - 局域网设备 SSH 连接目标管理 + 配置指南
 *
 * Business Logic（为什么需要这个页面）:
 *   用户希望快速连上局域网里的其他设备做 SSH 运维。本页列出 mDNS 发现的设备 IP + 手动添加的 IP，
 *   为每个目标配置用户名/端口并一键复制 ssh 命令；配置经向量时钟跨设备同步。
 *   同时给出 mac/ubuntu/windows 三端开启 SSH 服务的指南，以及按本机系统定制的连接端用法。
 *
 * Code Logic（这个页面做什么）:
 *   - 双源合并：list_devices（实时设备，address=IP）+ list_ssh_targets（持久配置，按 host 关联）
 *   - 每行用户名/端口失焦或回车即 upsert（自动保存 + 同步）
 *   - 复制按钮按 username/port 拼 ssh 命令写剪贴板
 *   - 指南区按 get_os_info 返回的 platform 渲染本机用法 + 三端并列的被连端指南
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { sshApi } from '@/api/ssh';
import { devicesApi } from '@/api/devices';
import {
  TerminalIcon,
  CopyIcon,
  TrashIcon,
  PlusIcon,
  AlertIcon,
  SyncIcon,
} from '@/lib/icons';
import type { Device, SshTarget, OsInfo } from '@/lib/types';
import styles from './Ssh.module.css';

/** 设备刷新间隔（ms） */
const REFRESH_INTERVAL_MS = 5000;

/**
 * 按 username/port 拼接 ssh 连接命令。
 *
 * Business Logic: 复制按钮需把当前目标转成可直接粘贴到终端的 ssh 命令。
 * Code Logic:
 *   - 非空用户名：拼 user@host；空用户名：仅 host（用本机默认用户名）。
 *   - 端口非 22：前缀 `-p {port}`；端口 22：省略。
 *   - 合并多余空白后返回。
 */
function buildCommand(host: string, username: string, port: number): string {
  const userPart = username.trim() ? `${username.trim()}@` : '';
  const portPart = port === 22 ? '' : `-p ${port} `;
  return `ssh ${portPart}${userPart}${host}`.replace(/\s+/g, ' ').trim();
}

export function Ssh() {
  const { t } = useTranslation(['ssh', 'common']);
  const [devices, setDevices] = useState<Device[]>([]);
  const [targets, setTargets] = useState<SshTarget[]>([]);
  const [osInfo, setOsInfo] = useState<OsInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [copiedHost, setCopiedHost] = useState<string | null>(null);
  // 手动添加表单
  const [mHost, setMHost] = useState('');
  const [mUser, setMUser] = useState('');
  const [mPort, setMPort] = useState('22');
  const [mLabel, setMLabel] = useState('');
  // 行内编辑缓存：host -> { username, port }
  const [edits, setEdits] = useState<Record<string, { username: string; port: string }>>({});

  /** 拉取实时设备 + 持久配置 + 本机 OS（首次加载） */
  const fetchAll = useCallback(async () => {
    try {
      setError(null);
      const [devs, tgts, os] = await Promise.all([
        devicesApi.list(),
        sshApi.list(),
        sshApi.getOsInfo(),
      ]);
      setDevices(devs);
      setTargets(tgts);
      setOsInfo(os);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('ssh:fetchFailed'));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    fetchAll();
  }, [fetchAll]);

  // 定时刷新设备（配置即时性由编辑后 upsert 保证，这里主要刷新设备在线状态）
  useEffect(() => {
    const id = setInterval(() => {
      devicesApi.list().then(setDevices).catch(() => {});
    }, REFRESH_INTERVAL_MS);
    return () => clearInterval(id);
  }, []);

  /** 按 host 取已配置的目标（用于预填设备行的用户名/端口） */
  const targetByHost = new Map(targets.map((t2) => [t2.host, t2]));

  /** 合并目标列表：实时设备（预填配置）+ 已配置但当前离线的目标 */
  const allHosts = (() => {
    const seen = new Set<string>();
    const rows: {
      host: string;
      username: string;
      port: number;
      label?: string;
      online: boolean;
      deviceName?: string;
    }[] = [];
    // 实时设备
    for (const d of devices) {
      if (seen.has(d.address)) continue;
      seen.add(d.address);
      const cfg = targetByHost.get(d.address);
      rows.push({
        host: d.address,
        username: cfg?.username ?? '',
        port: cfg?.port ?? 22,
        label: cfg?.label,
        online: d.status === 'online',
        deviceName: d.name,
      });
    }
    // 已配置但当前不在设备列表（离线/手动）
    for (const tg of targets) {
      if (seen.has(tg.host)) continue;
      seen.add(tg.host);
      rows.push({
        host: tg.host,
        username: tg.username,
        port: tg.port,
        label: tg.label,
        online: false,
      });
    }
    return rows;
  })();

  /** 保存某 host 的用户名/端口（upsert，自动同步） */
  const saveTarget = useCallback(
    async (host: string, username: string, port: number, label?: string) => {
      try {
        await sshApi.upsert(host, username, port, label);
        await sshApi.list().then(setTargets);
      } catch {
        // 静默失败，不影响其他操作
      }
    },
    [],
  );

  /** 行内编辑提交（失焦/回车） */
  const commitEdit = (host: string) => {
    const ed = edits[host];
    if (!ed) return;
    const cfg = targetByHost.get(host);
    void saveTarget(host, ed.username, Number(ed.port) || 22, cfg?.label);
    setEdits((prev) => {
      const next = { ...prev };
      delete next[host];
      return next;
    });
  };

  /** 复制 ssh 命令到剪贴板 */
  const handleCopy = async (host: string, username: string, port: number) => {
    const cmd = buildCommand(host, username, port);
    try {
      await navigator.clipboard.writeText(cmd);
      setCopiedHost(host);
      setTimeout(() => setCopiedHost(null), 1500);
    } catch {
      // 剪贴板不可用时静默
    }
  };

  /** 删除目标 */
  const handleDelete = async (host: string) => {
    try {
      await sshApi.remove(host);
      await sshApi.list().then(setTargets);
    } catch {
      // 静默
    }
  };

  /** 手动添加 */
  const handleAdd = async () => {
    const host = mHost.trim();
    if (!host) return;
    await saveTarget(host, mUser.trim(), Number(mPort) || 22, mLabel.trim() || undefined);
    setMHost('');
    setMUser('');
    setMPort('22');
    setMLabel('');
  };

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        {/* 页头 */}
        <header className={styles.header}>
          <div className={styles.headerText}>
            <h1>
              <TerminalIcon />
              {t('ssh:title')}
              {osInfo ? (
                <Pill tone="neutral" className={styles.osPill}>
                  {t('ssh:localOs')}: {osInfo.platform}
                </Pill>
              ) : null}
            </h1>
            <p className={styles.lead}>{t('ssh:desc')}</p>
          </div>
        </header>

        {error ? (
          <div className={styles.errorBox} role="alert">
            <AlertIcon />
            <span>{error}</span>
            <Button variant="secondary" size="sm" onClick={fetchAll}>
              {t('ssh:retry')}
            </Button>
          </div>
        ) : null}

        {/* 连接目标区 */}
        <Card variant="outlined" padding="md">
          <h2 className={styles.sectionTitle}>{t('ssh:targetsSection')}</h2>
          {loading && allHosts.length === 0 ? (
            <div className={styles.empty}>
              <SyncIcon /> {t('ssh:loading')}
            </div>
          ) : allHosts.length === 0 ? (
            <div className={styles.empty}>
              <p className={styles.emptyTitle}>{t('ssh:emptyTitle')}</p>
              <span>{t('ssh:emptyHint')}</span>
            </div>
          ) : (
            <div className={styles.targetList}>
              {allHosts.map((row) => {
                const ed = edits[row.host];
                const username = ed?.username ?? row.username;
                const port = ed?.port ?? String(row.port);
                return (
                  <div className={styles.targetRow} key={row.host}>
                    <span className={styles.targetHost} title={row.deviceName}>
                      {row.host}
                      {!row.online ? (
                        <span className={styles.targetHostMeta}> {t('ssh:offlineSuffix')}</span>
                      ) : null}
                    </span>
                    <Input
                      value={username}
                      placeholder={t('ssh:manualUsernamePh')}
                      onChange={(e) =>
                        setEdits((p) => ({
                          ...p,
                          [row.host]: {
                            username: e.target.value,
                            port: p[row.host]?.port ?? String(row.port),
                          },
                        }))
                      }
                      onBlur={() => commitEdit(row.host)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') commitEdit(row.host);
                      }}
                    />
                    <Input
                      type="number"
                      value={port}
                      onChange={(e) =>
                        setEdits((p) => ({
                          ...p,
                          [row.host]: {
                            username: p[row.host]?.username ?? row.username,
                            port: e.target.value,
                          },
                        }))
                      }
                      onBlur={() => commitEdit(row.host)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') commitEdit(row.host);
                      }}
                    />
                    <span />
                    <div className={styles.targetActions}>
                      <Button
                        variant="secondary"
                        size="sm"
                        icon={<CopyIcon />}
                        onClick={() => handleCopy(row.host, username, Number(port) || 22)}
                      >
                        {copiedHost === row.host ? t('ssh:copied') : t('ssh:copy')}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        icon={<TrashIcon />}
                        onClick={() => handleDelete(row.host)}
                        aria-label={t('ssh:delete')}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          {/* 手动添加 */}
          <div className={styles.manualRow}>
            <Input
              value={mHost}
              placeholder={t('ssh:manualHostPh')}
              onChange={(e) => setMHost(e.target.value)}
            />
            <Input
              value={mUser}
              placeholder={t('ssh:manualUsernamePh')}
              onChange={(e) => setMUser(e.target.value)}
            />
            <Input
              type="number"
              value={mPort}
              placeholder={t('ssh:manualPortPh')}
              onChange={(e) => setMPort(e.target.value)}
            />
            <Input
              value={mLabel}
              placeholder={t('ssh:manualLabelPh')}
              onChange={(e) => setMLabel(e.target.value)}
            />
            <Button variant="primary" size="sm" icon={<PlusIcon />} onClick={handleAdd}>
              {t('ssh:add')}
            </Button>
          </div>
        </Card>

        {/* 配置指南区 */}
        <Card variant="outlined" padding="md" className={styles.guideCard}>
          <h2 className={styles.sectionTitle}>{t('ssh:guideSection')}</h2>
          {/* 本机（连接端）用法：按本机 OS 只显示对应一端 */}
          <div className={styles.guideBlock}>
            <span className={styles.guideLabel}>
              {t('ssh:guideLocalTitle')} —{' '}
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
          {/* 被连端：三端并列 */}
          <div className={styles.guideBlock}>
            <span className={styles.guideLabel}>{t('ssh:guideRemoteTitle')}</span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideMac')}：</strong>
              {t('ssh:guideRemoteMac')}
            </span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideUbuntu')}：</strong>
              {t('ssh:guideRemoteUbuntu')}
            </span>
            <span className={styles.guideText}>
              <strong>{t('ssh:guideWindows')}：</strong>
              {t('ssh:guideRemoteWindows')}
            </span>
          </div>
        </Card>
      </div>
    </div>
  );
}

Ssh.displayName = 'Ssh';
