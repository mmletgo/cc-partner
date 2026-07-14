/**
 * LanFirewallDependencyCard（局域网防火墙依赖状态卡）
 *
 * Business Logic（为什么需要这个组件）:
 *   局域网互联访问项目要求当前设备允许 P2P HTTP TCP 端口和 mDNS UDP 5353 入站，
 *   用户需要在 Settings 依赖环境页看到当前端口/IP、检测结果和对应系统的放行方法。
 *
 * Code Logic（这个组件做什么）:
 *   调用 check_lan_firewall_dependency 读取后端 DTO，组合 Card/Pill/Button 渲染状态、
 *   检测项、系统步骤和可复制命令；读取开放状态但不自动修改系统防火墙。
 */

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import { lanFirewallDependencyApi } from '@/api/lanFirewallDependency';
import {
  buildLanFirewallCommandPreview,
  lanFirewallStatusTone,
  platformLabelKey,
} from '@/lib/lanFirewallDependency';
import type { LanFirewallCheck, LanFirewallDependencyStatus } from '@/lib/types';
import styles from './LanFirewallDependencyCard.module.css';

export interface LanFirewallDependencyCardProps {
  className?: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检测项 id 来自后端，前端需要把它映射为当前语言的用户可见文案。
 *
 * Code Logic（这个函数做什么）:
 *   返回 settings namespace 下的检测项标题 key；未知 id 回退到通用 unknown。
 */
function checkLabelKey(id: string): string {
  if (id === 'httpListener') return 'settings:lanFirewall.checks.httpListener';
  if (id === 'lanIp') return 'settings:lanFirewall.checks.lanIp';
  if (id === 'tcpFirewall') return 'settings:lanFirewall.checks.tcpFirewall';
  if (id === 'mdnsFirewall') return 'settings:lanFirewall.checks.mdnsFirewall';
  return 'settings:lanFirewall.checks.unknown';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检测项必须直接展示通过或失败，状态标签需要稳定映射到视觉语义。
 *
 * Code Logic（这个函数做什么）:
 *   ok=true 映射 success，ok=false 映射 danger。
 */
function checkTone(check: LanFirewallCheck): 'success' | 'danger' | 'warn' {
  if (check.ok === true) return 'success';
  return 'danger';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检测项状态文本必须走 i18n，不能在 JSX 中硬编码三态文案。
 *
 * Code Logic（这个函数做什么）:
 *   防火墙项返回 open/closed；基础运行项返回 ready/failed。
 */
function checkStatusKey(check: LanFirewallCheck): string {
  const isFirewallCheck = check.id === 'tcpFirewall' || check.id === 'mdnsFirewall';
  if (isFirewallCheck) {
    return check.ok
      ? 'settings:lanFirewall.checkStatus.open'
      : 'settings:lanFirewall.checkStatus.closed';
  }
  return check.ok
    ? 'settings:lanFirewall.checkStatus.ready'
    : 'settings:lanFirewall.checkStatus.failed';
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户需要在 Settings 依赖环境页主动重检局域网防火墙依赖状态，并复制当前系统命令。
 *
 * Code Logic（这个组件做什么）:
 *   用本地 state 管理 loading/status/error；mount 时加载一次，重检按钮复用同一加载函数。
 */
export function LanFirewallDependencyCard(props: LanFirewallDependencyCardProps) {
  const { className } = props;
  const { t } = useTranslation(['settings', 'common']);
  const [status, setStatus] = useState<LanFirewallDependencyStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   后端 guidance DTO 返回完整 i18n key，前端必须仍通过 locale 渲染用户可见文案。
   *
   * Code Logic（这个函数做什么）:
   *   将后端动态 key 收敛到单个强制类型转换点；key 集合由 settings locale 文件维护。
   */
  const translateDynamicKey = useCallback((key: string): string => t(key as never) as string, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户进入依赖环境页或点击重新检测时，需要读取最新 HTTP 端口、局域网 IP 和平台指引。
   *
   * Code Logic（这个函数做什么）:
   *   调用 lanFirewallDependencyApi.check，成功写入 status，失败写入错误文案，finally 收敛 loading。
   */
  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await lanFirewallDependencyApi.check();
      setStatus(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings:lanFirewall.loadFailed'));
    } finally {
      setLoading(false);
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   后端 detail 中的端口/IP 是运行数据可直出，但不可用哨兵必须按当前语言展示。
   *
   * Code Logic（这个函数做什么）:
   *   将 not-listening/unavailable 统一映射为 i18n 不可用文案，其余 detail 原样作为诊断数据展示。
   */
  const formatCheckDetail = useCallback(
    (check: LanFirewallCheck): string => {
      if (check.detail === 'not-listening' || check.detail === 'unavailable') {
        return t('settings:lanFirewall.unavailable');
      }
      return check.detail;
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   防火墙命令需要用户手动执行，卡片应提供一键复制以减少端口抄写错误。
   *
   * Code Logic（这个函数做什么）:
   *   使用浏览器 Clipboard API 写入命令；复制失败时只在卡片内展示 i18n 错误，不执行命令。
   */
  const copyCommand = useCallback(
    async (command: string) => {
      try {
        await navigator.clipboard.writeText(command);
      } catch {
        setError(t('settings:lanFirewall.copyFailed'));
      }
    },
    [t],
  );

  useEffect(() => {
    let cancelled = false;
    void lanFirewallDependencyApi
      .check()
      .then((next) => {
        if (!cancelled) setStatus(next);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : t('settings:lanFirewall.loadFailed'));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const tone = status ? lanFirewallStatusTone(status) : 'neutral';
  const statusKey =
    tone === 'danger'
      ? 'settings:lanFirewall.status.blocked'
      : tone === 'success'
        ? 'settings:lanFirewall.status.ready'
        : 'settings:lanFirewall.status.checking';
  const commandPreview = status ? buildLanFirewallCommandPreview(status) : '';

  return (
    <Card className={[styles.card, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <div>
            <h2 className={styles.title}>{t('settings:lanFirewall.title')}</h2>
            <p className={styles.subtitle}>{t('settings:lanFirewall.subtitle')}</p>
          </div>
          <Pill tone={tone} dot>
            {loading ? t('common:loading') : t(statusKey)}
          </Pill>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.description}>{t('settings:lanFirewall.description')}</p>
        <p className={styles.notice}>{t('settings:lanFirewall.riskNotice')}</p>
        <p className={styles.notice}>{t('settings:lanFirewall.manualNotice')}</p>

        {status ? (
          <>
            <dl className={styles.metaGrid}>
              <div>
                <dt>{t('settings:lanFirewall.meta.platform')}</dt>
                <dd>{translateDynamicKey(platformLabelKey(status.platform))}</dd>
              </div>
              <div>
                <dt>{t('settings:lanFirewall.meta.lanIp')}</dt>
                <dd>{status.lanIp ?? t('settings:lanFirewall.unavailable')}</dd>
              </div>
              <div>
                <dt>{t('settings:lanFirewall.meta.httpPort')}</dt>
                <dd>{status.httpPort > 0 ? status.httpPort : t('settings:lanFirewall.unavailable')}</dd>
              </div>
              <div>
                <dt>{t('settings:lanFirewall.meta.mdnsPort')}</dt>
                <dd>{status.mdnsPort}</dd>
              </div>
              {status.appPath ? (
                <div className={styles.metaWide}>
                  <dt>{t('settings:lanFirewall.meta.appPath')}</dt>
                  <dd>{status.appPath}</dd>
                </div>
              ) : null}
            </dl>

            <div className={styles.checkList}>
              {status.checks.map((check) => (
                <div className={styles.checkRow} key={check.id}>
                  <div className={styles.checkText}>
                    <span className={styles.checkLabel}>{translateDynamicKey(checkLabelKey(check.id))}</span>
                    <span className={styles.checkDetail}>{formatCheckDetail(check)}</span>
                  </div>
                  <Pill tone={checkTone(check)}>{translateDynamicKey(checkStatusKey(check))}</Pill>
                </div>
              ))}
            </div>

            <div className={styles.guidance}>
              <h3 className={styles.sectionTitle}>{t('settings:lanFirewall.guidanceTitle')}</h3>
              <p className={styles.description}>{translateDynamicKey(status.guidance.summaryKey)}</p>
              {status.guidance.steps.length > 0 ? (
                <ol className={styles.stepList}>
                  {status.guidance.steps.map((step) => (
                    <li key={step.labelKey}>{translateDynamicKey(step.labelKey)}</li>
                  ))}
                </ol>
              ) : null}
            </div>

            {status.guidance.commands.length > 0 ? (
              <div className={styles.commandList}>
                {status.guidance.commands.map((command) => (
                  <div className={styles.commandBox} key={`${command.labelKey}:${command.command}`}>
                    <div className={styles.commandHeader}>
                      <span>{translateDynamicKey(command.labelKey)}</span>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => {
                          void copyCommand(command.command);
                        }}
                      >
                        {t('settings:lanFirewall.copyCommand')}
                      </Button>
                    </div>
                    <code>{command.command}</code>
                  </div>
                ))}
              </div>
            ) : commandPreview ? (
              <pre className={styles.outputBox}>{commandPreview}</pre>
            ) : null}
          </>
        ) : null}

        {error ? <div className={styles.errorBox}>{error}</div> : null}

        <div className={styles.actions}>
          <Button
            variant="secondary"
            size="sm"
            loading={loading}
            disabled={loading}
            onClick={() => void load()}
          >
            {t('settings:lanFirewall.recheck')}
          </Button>
        </div>
      </Card.Body>
    </Card>
  );
}
