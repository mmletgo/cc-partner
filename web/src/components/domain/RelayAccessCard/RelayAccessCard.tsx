/**
 * RelayAccessCard（中转访问 / 跳板管理卡片）
 *
 * Business Logic（为什么需要这个组件）:
 *   设备 A 无法直连设备 C 时，可指定共同可达的直连设备 B 作为跳板访问 C 的远程项目。
 *   用户需要在 Settings 依赖环境页完成两件事：添加/移除信任的跳板设备（A 侧角色），
 *   以及决定本机是否允许被别人当作跳板（B 侧角色）；同时必须固定看到
 *   「流量途经跳板设备明文中转、LAN 模型无身份校验」的风险提示。
 *
 * Code Logic（这个组件做什么）:
 *   pure view：组合 Button/Card/Pill/StatusDot/StatusMessage primitives 渲染
 *   标题说明 + 风险提示、跳板添加选择器（候选经 props 注入）、已添加跳板列表
 *   （可展开影子清单）与本机角色开关；全部数据与动作经 props 传入，
 *   不 import @/api/*，保存反馈用 StatusMessage（成功 status / 失败 alert）。
 */

import { useCallback, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusDot, StatusMessage } from '@/components/primitives';
import type { RelayViaRow } from '@/lib/relayDevices';
import {
  ChevronDownIcon,
  ChevronUpIcon,
  PlusIcon,
  RefreshIcon,
  XIcon,
} from '@/lib/icons';
import styles from './RelayAccessCard.module.css';

/** 「添加跳板设备」候选设备（本机直连在线非本机设备，由 controller 过滤注入）。 */
export interface RelayAccessCandidate {
  id: string;
  name: string;
  address: string;
}

export interface RelayAccessCardProps {
  /** 可添加为跳板的直连在线设备候选；空数组时选择器只显示占位 */
  candidates: RelayAccessCandidate[];
  /** 已配置的跳板行（含影子清单与计数），由 controller 用 buildRelayViaRows 组装 */
  viaDevices: RelayViaRow[];
  /** 本机是否允许其他设备经本机中转（relay.enabled） */
  allowEnabled: boolean;
  /** 设备/配置加载中 */
  loading: boolean;
  /** 配置保存中（添加/移除/开关任一动作 pending） */
  saving: boolean;
  /** 设备或配置加载失败文案（卡片内提示 + 重试） */
  loadError: string | null;
  /** 保存失败文案（StatusMessage danger） */
  saveError: string | null;
  /** 保存成功文案（StatusMessage success） */
  saveSuccess: string | null;
  /** 选中并确认添加一台跳板设备 */
  onAddViaDevice: (deviceId: string) => void;
  /** 移除一台已配置跳板设备 */
  onRemoveViaDevice: (deviceId: string) => void;
  /** 切换本机是否允许被用作跳板（relay.enabled） */
  onToggleAllow: (enabled: boolean) => void;
  /** 重新加载设备列表与中转配置 */
  onRefresh: () => void;
  className?: string;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Settings 依赖环境页需要与防火墙/tmux 卡片同风格的「中转访问」管理面；
 *   数据获取与保存编排在 Settings controller，卡片只消费 props 保持可测 pure view。
 *
 * Code Logic（这个组件做什么）:
 *   hooks（useTranslation/useState/useCallback）全部置顶、无 early return；
 *   渲染风险提示、跳板添加行（select + 添加按钮）、跳板列表（展开态本地管理）、
 *   本机角色 switch 与保存反馈 StatusMessage。
 *
 * @param props 数据/动作/反馈 props bundle
 * @returns 中转访问管理 Card
 */
export function RelayAccessCard(props: RelayAccessCardProps): ReactElement {
  const {
    candidates,
    viaDevices,
    allowEnabled,
    loading,
    saving,
    loadError,
    saveError,
    saveSuccess,
    onAddViaDevice,
    onRemoveViaDevice,
    onToggleAllow,
    onRefresh,
    className,
  } = props;
  const { t } = useTranslation(['settings', 'common']);
  const [selectedCandidateId, setSelectedCandidateId] = useState('');
  const [expandedViaIds, setExpandedViaIds] = useState<ReadonlySet<string>>(new Set());

  /**
   * Business Logic（为什么需要这个函数）:
   *   添加动作必须基于用户当前下拉选择；未选择时按钮已禁用，这里再做一次守卫。
   *
   * Code Logic（这个函数做什么）:
   *   非空时回调 onAddViaDevice 并复位下拉选择。
   */
  const handleAdd = useCallback(() => {
    if (!selectedCandidateId) return;
    onAddViaDevice(selectedCandidateId);
    setSelectedCandidateId('');
  }, [onAddViaDevice, selectedCandidateId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   跳板行展开/收起是纯本地 UI 状态（不触发请求），需要在 Set 上不可变切换。
   *
   * Code Logic（这个函数做什么）:
   *   复制旧 Set 后增删目标 id 并回写 state。
   */
  const toggleExpanded = useCallback((deviceId: string) => {
    setExpandedViaIds((prev) => {
      const next = new Set(prev);
      if (next.has(deviceId)) next.delete(deviceId);
      else next.add(deviceId);
      return next;
    });
  }, []);

  return (
    <Card className={[styles.card, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <div>
            <h2 className={styles.title}>{t('settings:relay.title')}</h2>
            <p className={styles.subtitle}>{t('settings:relay.subtitle')}</p>
          </div>
          <Button
            variant="ghost"
            size="sm"
            icon={<RefreshIcon />}
            loading={loading}
            disabled={loading || saving}
            onClick={onRefresh}
          >
            {t('common:action.refresh')}
          </Button>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.description}>{t('settings:relay.description')}</p>
        <p className={styles.riskNotice}>{t('settings:relay.riskNotice')}</p>

        {loadError ? (
          <div className={styles.errorBox} role="alert">
            {t('settings:relay.loadFailed', { error: loadError })}
          </div>
        ) : null}

        <section className={styles.section} aria-label={t('settings:relay.viaSectionTitle')}>
          <h3 className={styles.sectionTitle}>{t('settings:relay.viaSectionTitle')}</h3>
          <div className={styles.addRow}>
            <select
              className={styles.candidateSelect}
              aria-label={t('settings:relay.addRelayLabel')}
              value={selectedCandidateId}
              disabled={saving || loading || candidates.length === 0}
              onChange={(event) => setSelectedCandidateId(event.target.value)}
            >
              <option value="">
                {candidates.length === 0
                  ? t('settings:relay.candidatesEmpty')
                  : t('settings:relay.addRelayPlaceholder')}
              </option>
              {candidates.map((candidate) => (
                <option key={candidate.id} value={candidate.id}>
                  {candidate.name}（{candidate.address}）
                </option>
              ))}
            </select>
            <Button
              variant="secondary"
              size="sm"
              icon={<PlusIcon />}
              disabled={!selectedCandidateId || saving || loading}
              onClick={handleAdd}
            >
              {t('settings:relay.addRelay')}
            </Button>
          </div>

          {viaDevices.length === 0 ? (
            <p className={styles.empty}>{t('settings:relay.emptyVia')}</p>
          ) : (
            <div className={styles.viaList}>
              {viaDevices.map((row) => {
                const expanded = expandedViaIds.has(row.deviceId);
                return (
                  <div className={styles.viaRow} key={row.deviceId} data-expanded={expanded || undefined}>
                    <div className={styles.viaMain}>
                      <button
                        type="button"
                        className={styles.viaSummary}
                        aria-expanded={expanded}
                        disabled={row.shadows.length === 0}
                        onClick={() => toggleExpanded(row.deviceId)}
                      >
                        <StatusDot status={row.status} size="sm" />
                        <span className={styles.viaName}>{row.deviceName}</span>
                        <span className={styles.viaAddress}>{row.address}</span>
                        <Pill tone="neutral">
                          {t('settings:relay.shadowCount', { count: row.shadowCount })}
                        </Pill>
                        {row.shadows.length > 0 ? (
                          expanded ? <ChevronUpIcon /> : <ChevronDownIcon />
                        ) : null}
                      </button>
                      <Button
                        variant="icon"
                        icon={<XIcon />}
                        title={t('settings:relay.removeVia', { device: row.deviceName })}
                        aria-label={t('settings:relay.removeVia', { device: row.deviceName })}
                        disabled={saving}
                        onClick={() => onRemoveViaDevice(row.deviceId)}
                      />
                    </div>
                    {expanded && row.shadows.length > 0 ? (
                      <ul className={styles.shadowList}>
                        {row.shadows.map((shadow) => (
                          <li className={styles.shadowRow} key={shadow.id}>
                            <StatusDot status={shadow.status} size="sm" />
                            <span className={styles.shadowName}>{shadow.name}</span>
                            <span className={styles.shadowStatus}>
                              {shadow.status === 'online'
                                ? t('common:status.device.online')
                                : t('common:status.device.offline')}
                            </span>
                          </li>
                        ))}
                      </ul>
                    ) : null}
                  </div>
                );
              })}
            </div>
          )}
          <p className={styles.helper}>{t('settings:relay.shadowDiscoveryHint')}</p>
        </section>

        <section className={styles.section} aria-label={t('settings:relay.allowLabel')}>
          <h3 className={styles.sectionTitle}>{t('settings:relay.allowSectionTitle')}</h3>
          <button
            type="button"
            className={styles.toggleRow}
            role="switch"
            aria-checked={allowEnabled}
            aria-label={t('settings:relay.allowLabel')}
            disabled={saving}
            onClick={() => onToggleAllow(!allowEnabled)}
          >
            <span className={styles.toggleText}>
              <span className={styles.toggleLabel}>{t('settings:relay.allowLabel')}</span>
              <span className={styles.toggleHelper}>{t('settings:relay.allowHelper')}</span>
            </span>
            <span className={styles.toggleState}>
              {allowEnabled
                ? t('settings:relay.allowStateOn')
                : t('settings:relay.allowStateOff')}
            </span>
          </button>
        </section>

        {saveSuccess ? (
          <StatusMessage tone="success">{saveSuccess}</StatusMessage>
        ) : null}
        {saveError ? <StatusMessage tone="danger">{saveError}</StatusMessage> : null}
      </Card.Body>
    </Card>
  );
}

RelayAccessCard.displayName = 'RelayAccessCard';
