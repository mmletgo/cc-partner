/**
 * DeviceCard 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   设备列表面板需要把局域网内通过 mDNS 发现的对端设备渲染为可识别的卡片单元，
 *   用户通过卡片快速判断设备是否在线、地址端口是什么，以及点击卡片触发进一步操作
 *   （如发起文件传输、同步 Prompt 等）。统一的卡片外观让多个设备并排时仍清晰可读。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card（elevated）渲染，左侧 StatusDot + 设备名，右侧 Pill 显示在线/离线
 *   - 副标题用等宽字体显示 "address:port"，方便技术人员对位查找
 *   - 离线的设备整体 opacity 降低 0.6，明显弱化其"可操作感"
 *   - onClick 存在时整卡可点击，键盘 Enter/Space 同样触发
 */

import { memo, useCallback } from 'react';
import type { CSSProperties, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Pill, StatusDot } from '@/components/primitives';
import { formatRelativeTime } from '@/lib/format';
import styles from './DeviceCard.module.css';

export type DeviceStatus = 'online' | 'offline';

/** 设备数据模型（与 network 模块的 mDNS 设备信息保持一致） */
export interface DeviceCardDevice {
  id: string;
  name: string;
  address: string;
  port: number;
  status: DeviceStatus;
  lastSeen?: string;
}

export interface DeviceCardProps {
  device: DeviceCardDevice;
  onClick?: () => void;
  className?: string;
  style?: CSSProperties;
}

/**
 * 渲染设备卡片
 *
 * Business Logic（为什么需要这个组件）:
 *   设备列表面板需要把局域网内通过 mDNS 发现的对端设备渲染为可识别的卡片单元，
 *   用户通过卡片快速判断设备是否在线、地址端口是什么，以及点击卡片触发进一步操作
 *   （如发起文件传输、同步 Prompt 等）。统一的卡片外观让多个设备并排时仍清晰可读。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card（elevated）渲染，左侧 StatusDot + 设备名，右侧 Pill 显示在线/离线
 *   - 副标题用等宽字体显示 "address:port"，方便技术人员对位查找
 *   - "最后活跃" 时间复用 lib/format.ts 的 formatRelativeTime（随语言切换）
 *   - 离线的设备整体 opacity 降低 0.6，明显弱化其"可操作感"
 *   - onClick 存在时整卡可点击，键盘 Enter/Space 同样触发
 *
 * @param props DeviceCardProps
 * @returns elevated 卡片，online 全亮、offline 整体半透明
 */
function DeviceCardInner({ device, onClick, className, style }: DeviceCardProps) {
  const { t } = useTranslation(['devices', 'common']);
  const isOnline = device.status === 'online';
  const clickable = Boolean(onClick);

  const handleClick = useCallback(() => {
    onClick?.();
  }, [onClick]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (!clickable) return;
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        onClick?.();
      }
    },
    [clickable, onClick],
  );

  const cardClasses = [
    styles.card,
    clickable ? styles.clickable : null,
    isOnline ? null : styles.offline,
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <Card
      variant="elevated"
      className={cardClasses}
      style={style}
      onClick={clickable ? handleClick : undefined}
      onKeyDown={clickable ? handleKeyDown : undefined}
      role={clickable ? 'button' : undefined}
      tabIndex={clickable ? 0 : undefined}
      aria-label={clickable ? t('devices:cardAria', { name: device.name }) : undefined}
    >
      <Card.Body padding="md" className={styles.body}>
        <div className={styles.row}>
          <div className={styles.left}>
            <StatusDot
              status={isOnline ? 'online' : 'offline'}
              size="sm"
              className={styles.dot}
            />
            <h4 className={styles.name}>{device.name}</h4>
          </div>
          <Pill tone={isOnline ? 'success' : 'neutral'} className={styles.statusPill}>
            {isOnline ? t('common:status.device.online') : t('common:status.device.offline')}
          </Pill>
        </div>
        <p className={styles.subtitle}>
          {device.address}:{device.port}
        </p>
        {device.lastSeen ? (
          <p className={styles.lastSeen}>
            {t('devices:lastSeenPrefix')} {formatRelativeTime(device.lastSeen)}
          </p>
        ) : null}
      </Card.Body>
    </Card>
  );
}

export const DeviceCard = memo(DeviceCardInner);
DeviceCard.displayName = 'DeviceCard';
