/**
 * Devices 页面 - 局域网设备管理
 *
 * Business Logic（为什么需要这个页面）:
 *   局域网 P2P 是 Claude Partner 的核心传输通道；用户需要看到当前网络上
 *   通过 mDNS 自动发现的对端设备，了解每个设备的状态、地址和端口，并
 *   知道本机在网络中的标识（方便区分"自己"）。本机信息单独高亮，以便
 *   用户在多设备场景下立即定位自己。
 *
 * Code Logic（这个页面做什么）:
 *   - 页面头部展示标题 + 在线数量 Pill + 副标题 + 手动刷新按钮
 *   - 顶部 outlined Card 显示本机信息（通过 /api/health 获取，设备名 + IP + 端口 + 状态）
 *   - 主区域用 auto-fill 网格渲染 DeviceCard，加载/空/错误态分别走
 *     skeleton / empty hint / error block
 *   - 启动后用 setInterval 每 5 秒拉取一次设备列表；
 *     卸载时清除 interval 防止内存泄漏
 *   - 容器居中、限宽，保证在大窗口下也保持可读
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Pill, Button, Input } from '@/components/primitives';
import { DeviceCard } from '@/components/domain';
import { devicesApi } from '@/api/devices';
import type { HealthResponse } from '@/api/devices';
import { SyncIcon, DevicesIcon, AlertIcon } from '@/lib/icons';
import type { Device } from '@/lib/types';
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
  const { t } = useTranslation(['devices', 'common']);
  const [devices, setDevices] = useState<Device[]>([]);
  const [selfDevice, setSelfDevice] = useState<SelfDeviceInfo | null>(null);
  const [selfLoading, setSelfLoading] = useState<boolean>(true);
  const [selfError, setSelfError] = useState<string | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState<number>(0);
  const [search, setSearch] = useState<string>('');
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

  // 首次挂载时获取本机信息
  useEffect(() => {
    fetchSelfDevice();
  }, [fetchSelfDevice]);

  // 首次挂载 + tick 变化时重新拉取设备列表
  useEffect(() => {
    fetchDevices();
  }, [fetchDevices, tick]);

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

  // 派生数据：搜索过滤
  const filteredDevices = devices.filter((d) =>
    d.name.toLowerCase().includes(search.trim().toLowerCase()),
  );

  const onlineCount = devices.filter((d) => d.status === 'online').length;

  /**
   * 处理搜索输入
   *
   * @param e input change 事件
   */
  const handleSearchChange = (e: ChangeEvent<HTMLInputElement>) => {
    setSearch(e.target.value);
  };

  /**
   * 手动触发刷新：递增 tick，下一次 effect 会重新拉取
   */
  const handleRefresh = () => {
    setLoading(true);
    setTick((prev) => prev + 1);
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
          <Button
            variant="secondary"
            size="sm"
            icon={<SyncIcon />}
            onClick={handleRefresh}
            loading={loading}
          >
            {t('common:action.refresh')}
          </Button>
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

        {/* 搜索栏 */}
        <div className={styles.searchRow}>
          <Input
            type="search"
            value={search}
            onChange={handleSearchChange}
            placeholder={t('devices:searchPlaceholder')}
            icon={<DevicesIcon />}
          />
        </div>

        {/* 错误提示 */}
        {error ? (
          <div className={styles.errorBox} role="alert">
            <AlertIcon />
            <span>{error}</span>
          </div>
        ) : null}

        {/* 设备网格 */}
        {loading && devices.length === 0 ? (
          <div className={styles.grid} aria-busy="true">
            {Array.from({ length: 3 }).map((_, i) => (
              <div key={i} className={styles.skeleton} aria-hidden="true" />
            ))}
          </div>
        ) : filteredDevices.length === 0 ? (
          <div className={styles.empty}>
            <p className={styles.emptyTitle}>{t('devices:emptyTitle')}</p>
            <p className={styles.emptyHint}>{t('devices:emptyHint')}</p>
          </div>
        ) : (
          <div className={styles.grid}>
            {filteredDevices.map((device) => (
              <DeviceCard key={device.id} device={device} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

Devices.displayName = 'Devices';
