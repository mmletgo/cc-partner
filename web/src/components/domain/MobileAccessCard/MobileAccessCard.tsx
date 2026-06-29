import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getMobileAccessInfo } from '@/api/mobile';
import type { MobileAccessInfo } from '@/lib/types';
import { Button, Card } from '@/components/primitives';
import { CopyIcon, SyncIcon } from '@/lib/icons';
import { renderMobileQrSvg, selectPrimaryMobileUrl } from './mobileQr';
import styles from './MobileAccessCard.module.css';

export interface MobileAccessCardProps {
  compact?: boolean;
  className?: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动访问卡片需要展示后端或浏览器剪贴板失败原因，且兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 Error.message；空消息或其它 unknown 值回退到 String(reason)。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * MobileAccessCard（移动端访问链接与二维码卡片）
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在桌面端同时看到局域网访问链接和二维码，才能用手机浏览器打开移动 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   请求 `/api/mobile/access-info`，选择主 URL 生成二维码 SVG，并提供复制和刷新操作。
 */
export function MobileAccessCard(props: MobileAccessCardProps) {
  const { compact = false, className } = props;
  const { t } = useTranslation(['settings']);
  const [info, setInfo] = useState<MobileAccessInfo | null>(null);
  const [qrSvg, setQrSvg] = useState<string>('');
  const [loading, setLoading] = useState<boolean>(false);
  const [copying, setCopying] = useState<boolean>(false);
  const [copied, setCopied] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const primaryUrl = useMemo(() => selectPrimaryMobileUrl(info?.urls ?? []), [info]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可能在网络切换后需要重新获取局域网 URL，设置页和 Workbench 卡片都应能刷新。
   *
   * Code Logic（这个函数做什么）:
   *   调用 mobile access-info API 并写入本地状态；失败时展示错误，成功时清理旧复制状态。
   */
  const loadAccessInfo = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const nextInfo = await getMobileAccessInfo();
      setInfo(nextInfo);
      setCopied(false);
    } catch (reason) {
      setError(getErrorMessage(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  /* eslint-disable react-hooks/set-state-in-effect -- 组件挂载与 URL 变化时需要拉取/生成外部 access-info 与二维码 */
  useEffect(() => {
    void loadAccessInfo();
  }, [loadAccessInfo]);

  useEffect(() => {
    if (!primaryUrl) {
      setQrSvg('');
      return undefined;
    }
    setQrSvg('');
    let cancelled = false;
    void renderMobileQrSvg(primaryUrl)
      .then((svg) => {
        if (!cancelled) setQrSvg(svg);
      })
      .catch((reason) => {
        if (!cancelled) setError(getErrorMessage(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [primaryUrl]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端展示访问链接后，用户通常会复制到聊天或手机浏览器。
   *
   * Code Logic（这个函数做什么）:
   *   使用 Clipboard API 写入主 URL；成功后短暂切换按钮文本，失败时展示错误。
   */
  const copyPrimaryUrl = useCallback(async (): Promise<void> => {
    if (!primaryUrl) return;
    if (typeof navigator.clipboard?.writeText !== 'function') {
      setError(t('mobileAccess.copyUnavailable'));
      return;
    }
    setCopying(true);
    setError(null);
    try {
      await navigator.clipboard.writeText(primaryUrl);
      setCopied(true);
    } catch (reason) {
      setError(getErrorMessage(reason));
    } finally {
      setCopying(false);
    }
  }, [primaryUrl, t]);

  return (
    <Card className={[styles.card, compact ? styles.compact : null, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <h2 className={styles.title}>{t('mobileAccess.title')}</h2>
          <p className={styles.description}>{t('mobileAccess.description')}</p>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.warning}>{t('mobileAccess.warning')}</p>
        {error ? <p className={styles.error}>{error}</p> : null}
        {loading && !info ? (
          <p className={styles.state}>{t('mobileAccess.loading')}</p>
        ) : primaryUrl ? (
          <div className={styles.contentGrid}>
            <div className={styles.urlList}>
              {(info?.urls ?? []).map((url) => (
                <code className={styles.url} key={url}>
                  {url}
                </code>
              ))}
              <div className={styles.actions}>
                <Button
                  variant="primary"
                  size="sm"
                  icon={<CopyIcon />}
                  loading={copying}
                  onClick={() => void copyPrimaryUrl()}
                >
                  {copied ? t('mobileAccess.copied') : t('mobileAccess.copy')}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<SyncIcon />}
                  loading={loading}
                  onClick={() => void loadAccessInfo()}
                >
                  {t('mobileAccess.refresh')}
                </Button>
              </div>
            </div>
            <div
              className={styles.qr}
              aria-label={t('mobileAccess.qrLabel')}
              dangerouslySetInnerHTML={{ __html: qrSvg }}
            />
          </div>
        ) : (
          <div className={styles.urlList}>
            <p className={styles.state}>{t('mobileAccess.empty')}</p>
            <Button
              variant="secondary"
              size="sm"
              icon={<SyncIcon />}
              loading={loading}
              onClick={() => void loadAccessInfo()}
            >
              {t('mobileAccess.refresh')}
            </Button>
          </div>
        )}
      </Card.Body>
    </Card>
  );
}
