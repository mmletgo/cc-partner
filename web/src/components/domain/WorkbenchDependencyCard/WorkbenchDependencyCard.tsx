/**
 * WorkbenchDependencyCard（tmux 依赖状态卡）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 的真实 window/pane 能力依赖 tmux，用户需要知道当前状态并能主动安装或重新检测。
 *
 * Code Logic（这个组件做什么）:
 *   读取 WorkbenchDependencyProvider 状态，渲染状态、版本/路径、安装命令预览、安装输出和操作按钮。
 */

import { useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import type { WorkbenchDependencyStatus } from '@/lib/types';
import {
  canInstallWorkbenchDependency,
  canRecheckWorkbenchDependency,
  dependencyStatusTone,
  formatInstallCommandPreview,
} from '@/lib/workbenchDependency';
import styles from './WorkbenchDependencyCard.module.css';

export interface WorkbenchDependencyCardSource {
  status: WorkbenchDependencyStatus;
  checking: boolean;
  installing: boolean;
  error: string | null;
  check: () => Promise<void>;
  install: () => Promise<void>;
  cancel: () => Promise<void>;
}

export interface WorkbenchDependencyCardProps {
  compact?: boolean;
  className?: string;
  source?: WorkbenchDependencyCardSource;
  deviceName?: string;
  remoteWriteDisabled?: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   依赖安装会执行系统命令，必须让用户先确认命令内容。
 *
 * Code Logic（这个函数做什么）:
 *   使用浏览器原生 confirm 展示命令预览；用户确认后调用 install。
 */
function useConfirmedInstall(
  commandPreview: string,
  install: () => Promise<void>,
  deviceName?: string,
) {
  const { t } = useTranslation(['workbench']);
  return useCallback(() => {
    const command = commandPreview || t('workbench:dependency.noCommand');
    const ok = window.confirm(
      deviceName
        ? t('workbench:dependency.installConfirmRemote', { device: deviceName, command })
        : t('workbench:dependency.installConfirm', { command }),
    );
    if (!ok) return;
    void install();
  }, [commandPreview, deviceName, install, t]);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户进入 Workbench 或 Settings 时需要看到 tmux 依赖是否可用以及下一步动作。
 *
 * Code Logic（这个组件做什么）:
 *   组合 Card/Pill/Button 展示共享依赖状态；操作通过 Context 调用后端 check/install/cancel。
 */
export function WorkbenchDependencyCard(props: WorkbenchDependencyCardProps) {
  const { compact = false, className, source, deviceName, remoteWriteDisabled = false } = props;
  const { t } = useTranslation(['workbench']);
  const local = useWorkbenchDependency();
  const { status, checking, installing, error, check, install, cancel } = source ?? local;
  const commandPreview = formatInstallCommandPreview(status.installCommandPreview);
  const confirmedInstall = useConfirmedInstall(commandPreview, install, deviceName);
  const tone = dependencyStatusTone(status);
  const pillTone = tone === 'warning' ? 'warn' : tone === 'neutral' ? 'neutral' : tone;
  const title = status.available
    ? t('workbench:dependency.readyTitle')
    : t('workbench:dependency.missingTitle');

  return (
    <Card className={[styles.card, compact ? styles.compact : null, className].filter(Boolean).join(' ')}>
      <Card.Header className={styles.header}>
        <div className={styles.titleGroup}>
          <h2 className={styles.title}>{title}</h2>
          <Pill tone={pillTone} dot>
            {t(`workbench:dependency.status.${status.status}`)}
          </Pill>
        </div>
      </Card.Header>
      <Card.Body className={styles.body}>
        <p className={styles.description}>
          {status.available
            ? t('workbench:dependency.readyDescription')
            : t('workbench:dependency.missingDescription')}
        </p>

        <dl className={styles.metaGrid}>
          <div>
            <dt>{t('workbench:dependency.backend')}</dt>
            <dd>{status.backend || t('workbench:emptyValue')}</dd>
          </div>
          <div>
            <dt>{t('workbench:dependency.version')}</dt>
            <dd>{status.version ?? t('workbench:emptyValue')}</dd>
          </div>
          <div>
            <dt>{t('workbench:dependency.path')}</dt>
            <dd>{status.path ?? t('workbench:emptyValue')}</dd>
          </div>
        </dl>

        {!status.available && commandPreview ? (
          <div className={styles.commandBox}>
            <span>{t('workbench:dependency.installCommand')}</span>
            <code>{commandPreview}</code>
          </div>
        ) : null}

        {(status.error || error) ? (
          <div className={styles.errorBox}>{status.error ?? error}</div>
        ) : null}

        {status.output.length > 0 ? (
          <pre className={styles.outputBox}>{status.output.slice(-6).join('\n')}</pre>
        ) : null}

        <div className={styles.actions}>
          {canInstallWorkbenchDependency(status) && !remoteWriteDisabled ? (
            <Button
              variant="primary"
              size="sm"
              loading={installing}
              disabled={checking || installing}
              onClick={confirmedInstall}
            >
              {t('workbench:dependency.install')}
            </Button>
          ) : null}
          {status.status === 'installing' ? (
            <Button variant="secondary" size="sm" onClick={() => void cancel()}>
              {t('workbench:dependency.cancel')}
            </Button>
          ) : null}
          <Button
            variant="secondary"
            size="sm"
            loading={checking}
            disabled={!canRecheckWorkbenchDependency(status)}
            onClick={() => void check()}
          >
            {t('workbench:dependency.recheck')}
          </Button>
        </div>
      </Card.Body>
    </Card>
  );
}
