/**
 * MobilePushRelayCard — 移动系统推送中转配置
 *
 * Business Logic（为什么需要）:
 *   PC 要把 Attention 通知发到被杀的 App，必须配置互联网中转；密钥不进安装包。
 *
 * Code Logic（做什么）:
 *   pure view：URL + token 输入、保存/刷新、风险文案。不 import @/api。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input, StatusMessage } from '@/components/primitives';
import styles from './MobilePushRelayCard.module.css';

export interface MobilePushRelayCardProps {
  relayUrl: string;
  relayToken: string;
  tokenConfigured: boolean;
  loading: boolean;
  saving: boolean;
  loadError: string | null;
  saveError: string | null;
  saveSuccess: string | null;
  onRelayUrlChange: (value: string) => void;
  onRelayTokenChange: (value: string) => void;
  onSave: () => void;
  onRefresh: () => void;
}

export function MobilePushRelayCard(props: MobilePushRelayCardProps): ReactElement {
  const { t } = useTranslation(['settings']);
  return (
    <Card variant="flat" padding="md" className={styles.card}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <div>
            <h3 className={styles.title}>{t('settings:mobilePush.title')}</h3>
            <p className={styles.subtitle}>{t('settings:mobilePush.subtitle')}</p>
          </div>
          <Button variant="ghost" size="sm" onClick={props.onRefresh} disabled={props.loading}>
            {t('settings:mobilePush.refresh')}
          </Button>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.description}>{t('settings:mobilePush.description')}</p>
        <p className={styles.risk}>{t('settings:mobilePush.riskNotice')}</p>
        {props.loadError ? (
          <StatusMessage tone="danger">{props.loadError}</StatusMessage>
        ) : null}
        {props.saveError ? (
          <StatusMessage tone="danger">{props.saveError}</StatusMessage>
        ) : null}
        {props.saveSuccess ? (
          <StatusMessage tone="success">{t('settings:mobilePush.saveSuccess')}</StatusMessage>
        ) : null}
        <Input
          value={props.relayUrl}
          onChange={(event) => props.onRelayUrlChange(event.target.value)}
          placeholder={t('settings:mobilePush.urlPlaceholder')}
          disabled={props.loading || props.saving}
        />
        <Input
          type="password"
          value={props.relayToken}
          onChange={(event) => props.onRelayTokenChange(event.target.value)}
          placeholder={
            props.tokenConfigured
              ? t('settings:mobilePush.tokenKeep')
              : t('settings:mobilePush.tokenPlaceholder')
          }
          disabled={props.loading || props.saving}
        />
        <div>
          <Button
            variant="primary"
            size="sm"
            onClick={props.onSave}
            loading={props.saving}
            disabled={props.loading}
          >
            {t('settings:mobilePush.save')}
          </Button>
        </div>
      </Card.Body>
    </Card>
  );
}
