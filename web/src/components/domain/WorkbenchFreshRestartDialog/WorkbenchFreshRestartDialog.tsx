/**
 * WorkbenchFreshRestartDialog — 设备级「全新启动连接」确认弹窗（pure domain view）。
 *
 * Business Logic（为什么需要）:
 *   终止整台设备的工作台终端是不可逆动作，用户确认前需要看到预检影响面（会话清单、
 *   非工作台 tmux 会话数、ssh 引导通道可用性），并显式决定是否连带终止用户自己的
 *   tmux 会话；执行后如 ssh 自动引导降级，还需展示可复制的手动命令。
 *   Workbench 页面状态卡与侧栏 WorkbenchProjectRail 两个入口共用本弹窗。
 *
 * Code Logic（做什么）:
 *   props-only 消费调用方状态：previewing/preview → 确认 UI（foreign checkbox）；
 *   result → 结果反馈（成功/未重启 + skipped + 降级详情 + manual command + 复制按钮）；
 *   error → role=alert。busy 期间禁 Escape/Backdrop 关闭与确认双锁。
 *   不 import @/api/*；用户文案走 workbench:freshStart。
 */

import { useId, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '@/components/primitives/Dialog';
import { Button } from '@/components/primitives/Button';
import { StatusMessage } from '@/components/primitives/StatusMessage';
import type {
  WorkbenchFreshRestartPreview,
  WorkbenchFreshRestartResult,
} from '@/lib/types';
import styles from './WorkbenchFreshRestartDialog.module.css';

/** 调用方传入的弹窗状态切片（见 hooks/useWorkbenchFreshRestart）。 */
export interface WorkbenchFreshRestartDialogProps {
  open: boolean;
  onClose: () => void;
  previewing: boolean;
  preview: WorkbenchFreshRestartPreview | null;
  result: WorkbenchFreshRestartResult | null;
  error: string | null;
  busy: boolean;
  onConfirm: (includeForeignSessions: boolean) => void;
}

/** degraded_reason 稳定 token → i18n key 后缀的映射（闭集）。 */
const DEGRADED_KEY_SUFFIXES: ReadonlyArray<string> = [
  'platform_unsupported',
  'ssh_unavailable',
  'ssh_failed',
  'ssh_timeout',
  'user_unknown',
  'foreign_present',
  'close_failed',
  'kill_server_failed',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   降级 token 是后端稳定 code，前端必须映射到固定文案而不是透传后端中文详情做判断。
 *
 * Code Logic（这个函数做什么）:
 *   闭集内的 token 返回对应 key；未知 token 返回 'unknown'。
 */
function degradedKeySuffix(reason: string | null): string {
  if (reason && DEGRADED_KEY_SUFFIXES.includes(reason)) return reason;
  return 'unknown';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   弹窗是设备级危险动作的最终确认层。
 *
 * Code Logic（这个函数做什么）:
 *   三段渲染：预检态（loading/清单/警告/checkbox）→ 结果态（成功/降级 + 手动命令）→
 *   错误 alert；确认按钮 busy 双锁，busy 时禁 Escape/Backdrop。
 */
export function WorkbenchFreshRestartDialog(props: WorkbenchFreshRestartDialogProps): ReactElement {
  const { open, onClose, previewing, preview, result, error, busy, onConfirm } = props;
  const { t } = useTranslation(['workbench', 'common']);
  // 动态拼 key（degraded token 闭集映射）需要绕过 i18n key 联合类型（照
  // LanFirewallDependencyCard 的 translateDynamicKey 模式；suffix 已闭集校验）。
  const translateDegraded = (suffix: string): string =>
    t(`workbench:freshStart.degraded.${suffix}` as never);
  const titleId = useId();
  const [includeForeign, setIncludeForeign] = useState(false);
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');

  const sessionCount = preview?.sessions.length ?? preview?.workbenchTmuxSessionCount ?? 0;
  const foreignNames = preview?.foreignSessionNames ?? [];

  /**
   * Business Logic（为什么需要这个函数）:
   *   降级路径的手动命令必须一键可复制（参照 LanFirewallDependencyCard 的命令展示模式）。
   *
   * Code Logic（这个函数做什么）:
   *   navigator.clipboard.writeText；失败置 failed 文案（不抛出）。
   */
  async function handleCopyCommand(command: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(command);
      setCopyState('copied');
    } catch {
      setCopyState('failed');
    }
  }

  return (
    <Dialog
      open={open}
      titleId={titleId}
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
    >
      <div className={styles.dialog} data-testid="workbench-fresh-restart-dialog">
        <h2 id={titleId}>{t('workbench:freshStart.title')}</h2>
        <p className={styles.hint}>{t('workbench:freshStart.description')}</p>

        {previewing && !result ? (
          <p className={styles.statusLine}>{t('workbench:freshStart.previewLoading')}</p>
        ) : null}

        {!previewing && !preview && !result ? (
          <StatusMessage tone="danger" role="alert">
            {t('workbench:freshStart.previewFailed')}
          </StatusMessage>
        ) : null}

        {preview && !result ? (
          <>
            <p className={styles.statusLine}>
              {t('workbench:freshStart.confirmBody', { count: sessionCount })}
            </p>
            {preview.sessions.length > 0 ? (
              <div className={styles.sessions}>
                <p className={styles.sessionsTitle}>
                  {t('workbench:freshStart.sessionsTitle')}
                </p>
                <ul className={styles.sessionList}>
                  {preview.sessions.map((session) => (
                    <li key={session.sessionId} className={styles.sessionItem}>
                      <span className={styles.sessionName}>{session.name}</span>
                      <span className={styles.sessionBackend}>{session.backend}</span>
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
            {preview.foreignSessionCount > 0 ? (
              <div className={styles.foreignWarning}>
                <p>
                  {t('workbench:freshStart.foreignWarning', {
                    count: preview.foreignSessionCount,
                    names: foreignNames.join(', '),
                  })}
                </p>
                <label className={styles.foreignLabel}>
                  <input
                    type="checkbox"
                    checked={includeForeign}
                    disabled={busy}
                    onChange={(event) => setIncludeForeign(event.target.checked)}
                  />
                  {t('workbench:freshStart.includeForeignLabel')}
                </label>
              </div>
            ) : null}
            {preview.sshBootstrapAvailable === false && preview.sshBootstrapDetail ? (
              <StatusMessage tone="warn">
                {t('workbench:freshStart.sshUnavailable', {
                  detail: preview.sshBootstrapDetail,
                })}
              </StatusMessage>
            ) : null}
          </>
        ) : null}

        {result ? (
          <>
            {result.serverRestarted ? (
              <StatusMessage tone="success">
                {t('workbench:freshStart.succeeded', {
                  count: result.terminatedSessionCount,
                })}
              </StatusMessage>
            ) : (
              <StatusMessage tone="warn">
                {t('workbench:freshStart.serverNotRestarted', {
                  count: result.terminatedSessionCount,
                })}
              </StatusMessage>
            )}
            {result.skippedSessionIds.length > 0 ? (
              <p className={styles.statusLine}>
                {t('workbench:freshStart.skippedSessions', {
                  count: result.skippedSessionIds.length,
                })}
              </p>
            ) : null}
            {result.degradedDetail ? (
              <StatusMessage tone="warn">
                {translateDegraded(degradedKeySuffix(result.degradedReason))}
                ：{result.degradedDetail}
              </StatusMessage>
            ) : null}
            {result.manualCommand ? (
              <div className={styles.commandBox}>
                <p className={styles.sessionsTitle}>
                  {t('workbench:freshStart.manualCommandLabel')}
                </p>
                <div className={styles.commandRow}>
                  <code>{result.manualCommand}</code>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => void handleCopyCommand(result.manualCommand ?? '')}
                  >
                    {copyState === 'copied'
                      ? t('workbench:freshStart.copied')
                      : copyState === 'failed'
                        ? t('workbench:freshStart.copyFailed')
                        : t('workbench:freshStart.copyCommand')}
                  </Button>
                </div>
              </div>
            ) : null}
          </>
        ) : null}

        {error ? (
          <StatusMessage tone="danger" role="alert">
            {error}
          </StatusMessage>
        ) : null}

        <div className={styles.actions}>
          {result ? (
            <Button variant="ghost" onClick={onClose} disabled={busy}>
              {t('workbench:freshStart.close')}
            </Button>
          ) : (
            <>
              <Button variant="ghost" onClick={onClose} disabled={busy}>
                {t('workbench:freshStart.cancel')}
              </Button>
              <Button
                variant="danger"
                onClick={() => onConfirm(includeForeign)}
                loading={busy}
                disabled={busy || previewing}
              >
                {t('workbench:freshStart.execute')}
              </Button>
            </>
          )}
        </div>
      </div>
    </Dialog>
  );
}
