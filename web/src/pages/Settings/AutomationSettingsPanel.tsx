/**
 * Orchestrator 自动化设置面板 - 设置页「自动化」tab 的纯渲染组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要在 Settings 中集中编辑设备级 Orchestrator 自动化策略，包括 scheduler 开关、
 *   并发上限、验证命令和 full-auto 交付开关；这些配置由 Settings 顶层负责加载和提交。
 *
 * Code Logic（这个组件做什么）:
 *   复用 Settings 通用 Card/field/toggle/footer 样式渲染受控表单；所有字段变更通过 onChange
 *   回传完整 nextForm，不直接调用后端，不持有副作用状态。
 */
import type { ChangeEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon } from '@/lib/icons';
import {
  OPENCODE_RUNTIME_BRIDGE_REL_PATH,
  agentAdapterAvailabilityTone,
  agentAdapterBlockedReason,
  effectiveOpenCodeBridgeStatus,
  isAgentAdapterEffectivelyAvailable,
} from '@/lib/agentAdapterPresentation';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import type { AutomationSettingsForm } from './automationSettingsState';
import {
  clampAutomationMaxConcurrentTasks,
  isAutomationFormDirty,
} from './automationSettingsState';
import styles from './Settings.module.css';

interface AutomationSettingsPanelProps {
  /** 当前受控表单值 */
  form: AutomationSettingsForm;
  /** 后端默认配置表单值 */
  defaults: AutomationSettingsForm;
  /** 当前表单是否相对已加载/已保存快照有修改 */
  dirty: boolean;
  /** 是否正在保存 */
  saving: boolean;
  /** 保存错误 */
  error: string | null;
  /** 最近一次保存是否成功 */
  saved: boolean;
  /** 表单变更回调 */
  onChange: (nextForm: AutomationSettingsForm) => void;
  /** 恢复默认回调 */
  onResetDefaults: () => void;
  /** 保存回调 */
  onSave: () => void;
  /** 默认配置是否可用（不可用时禁用「恢复默认」） */
  canResetDefaults?: boolean;
  /** owner adapter catalog（只读可用性，无 path/env） */
  agentAdapters?: OrchestratorAgentAdapterCatalogItem[];
  /**
   * OpenCode bridge 需要项目 preview 时，打开既有 Agent Hub 项目预览路径。
   * 不得直接 enable/overwrite。
   */
  onOpenOpenCodeBridgePreview?: () => void;
}

interface ToggleRowProps {
  label: string;
  helper: string;
  checked: boolean;
  onToggle: (next: boolean) => void;
}

/**
 * 渲染自动化布尔开关行
 *
 * Business Logic（为什么需要这个组件）:
 *   自动化策略有多个布尔开关，需要保持与同步/AI/健康 tab 一致的开关视觉和可访问性语义。
 *
 * Code Logic（这个组件做什么）:
 *   使用 button(role=switch) 承载点击交互；checked 决定 Pill tone 与图标，onToggle 回传取反值。
 */
function ToggleRow({ label, helper, checked, onToggle }: ToggleRowProps): ReactElement {
  return (
    <button
      type="button"
      className={styles.toggleRow}
      onClick={() => onToggle(!checked)}
      role="switch"
      aria-checked={checked}
      aria-label={label}
    >
      <div className={styles.toggleText}>
        <span className={styles.toggleLabel}>{label}</span>
        <span className={styles.toggleHelper}>{helper}</span>
      </div>
      <span className={styles.toggleState}>
        {checked ? (
          <Pill tone="success" dot>
            <CheckIcon size={12} />
          </Pill>
        ) : (
          <Pill tone="neutral" dot>
            <XIcon size={12} />
          </Pill>
        )}
      </span>
    </button>
  );
}

/**
 * 自动化设置面板组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Settings 自动化 tab 需要一个受控、可复用的表单视图，父组件才能统一管理加载、保存、错误和 dirty 状态。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 位于顶部；渲染一个配置 Card 和一个交付 Card，并在底部展示 dirty/saved/error 状态与操作按钮。
 *
 * @returns 自动化配置受控表单
 */
export function AutomationSettingsPanel({
  form,
  defaults,
  dirty,
  saving,
  error,
  saved,
  onChange,
  onResetDefaults,
  onSave,
  canResetDefaults = true,
  agentAdapters = [],
  onOpenOpenCodeBridgePreview,
}: AutomationSettingsPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common', 'workbench']);
  const resetDisabled =
    saving || !canResetDefaults || !isAutomationFormDirty(form, defaults);

  return (
    <>
      {agentAdapters.length > 0 ? (
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>
              {t('settings:automation.agentAdaptersTitle')}
            </h2>
          </Card.Header>
          <Card.Body padding="md">
            <p className={styles.helper}>
              {t('settings:automation.agentAdaptersHint')}
            </p>
            <ul
              className={styles.toggleList}
              aria-label={t('settings:automation.agentAdaptersAriaLabel')}
            >
              {agentAdapters.map((item) => {
                const isOpenCode = item.provider === 'openCodeVisible';
                const bridgeStatus = isOpenCode
                  ? effectiveOpenCodeBridgeStatus(item)
                  : (item.bridgeStatus ?? null);
                const blocked = agentAdapterBlockedReason(item);
                const availableTone = agentAdapterAvailabilityTone(item);
                const effectivelyAvailable = isAgentAdapterEffectivelyAvailable(item);
                const needsBridgePreview =
                  isOpenCode && bridgeStatus !== null && bridgeStatus !== 'ready';
                return (
                  <li
                    key={item.provider}
                    className={styles.toggleRow}
                    data-testid={`agent-adapter-${item.provider}`}
                    data-provider={item.provider}
                    data-bridge-status={bridgeStatus ?? undefined}
                    data-completion={item.completionContract}
                    data-effectively-available={effectivelyAvailable ? 'true' : 'false'}
                  >
                    <div className={styles.toggleText}>
                      <span className={styles.toggleLabel}>
                        {t(`settings:automation.provider.${item.provider}`, {
                          defaultValue: item.provider,
                        })}
                      </span>
                      <span className={styles.toggleHelper}>
                        {t('settings:automation.completionContract', {
                          contract: item.completionContract,
                        })}
                        {item.executable
                          ? ` · ${t('settings:automation.executable', {
                              path: item.executable,
                            })}`
                          : ''}
                        {item.version
                          ? ` · ${t('settings:automation.version', {
                              version: item.version,
                            })}`
                          : ''}
                        {item.supportEvidence
                          ? ` · ${t('settings:automation.supportEvidence', {
                              evidence: item.supportEvidence,
                            })}`
                          : ''}
                        {isOpenCode && bridgeStatus
                          ? ` · ${t(`settings:automation.bridgeStatus.${bridgeStatus}`)}`
                          : ''}
                        {isOpenCode
                          ? ` · ${t('settings:automation.bridgePath', {
                              path: OPENCODE_RUNTIME_BRIDGE_REL_PATH,
                            })}`
                          : ''}
                        {blocked
                          ? ` · ${t('settings:automation.blockedReason', {
                              reason: blocked,
                            })}`
                          : ''}
                      </span>
                      {needsBridgePreview && onOpenOpenCodeBridgePreview ? (
                        <div className={styles.field}>
                          <Button
                            variant="secondary"
                            size="sm"
                            onClick={onOpenOpenCodeBridgePreview}
                            data-testid="open-code-bridge-preview"
                          >
                            {t('workbench:openCodeBridge.openPreview')}
                          </Button>
                        </div>
                      ) : null}
                    </div>
                    <span className={styles.toggleState}>
                      {effectivelyAvailable ? (
                        <Pill tone={availableTone} dot>
                          {t('settings:automation.adapterAvailable')}
                        </Pill>
                      ) : (
                        <Pill tone={availableTone} dot>
                          {isOpenCode && bridgeStatus === 'previewRequired'
                            ? t('settings:automation.bridgeStatus.previewRequired')
                            : t('settings:automation.adapterUnavailable')}
                        </Pill>
                      )}
                    </span>
                  </li>
                );
              })}
            </ul>
          </Card.Body>
        </Card>
      ) : null}

      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings:automation.title')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          <p className={styles.helper}>{t('settings:automation.description')}</p>

          <div className={styles.toggleList}>
            <ToggleRow
              label={t('settings:automation.enabled')}
              helper={t('settings:automation.enabledHint')}
              checked={form.enabled}
              onToggle={(enabled) => onChange({ ...form, enabled })}
            />
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="settings-automation-max-concurrent">
              {t('settings:automation.maxConcurrentTasks')}
            </label>
            <Input
              id="settings-automation-max-concurrent"
              type="number"
              min={1}
              max={8}
              step={1}
              value={form.maxConcurrentTasks}
              onChange={(e: ChangeEvent<HTMLInputElement>) =>
                onChange({
                  ...form,
                  maxConcurrentTasks: clampAutomationMaxConcurrentTasks(Number(e.target.value)),
                })
              }
              mono
            />
            <p className={styles.helper}>{t('settings:automation.maxConcurrentTasksHint')}</p>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="settings-automation-verification-commands">
              {t('settings:automation.verificationCommands')}
            </label>
            <textarea
              id="settings-automation-verification-commands"
              className={styles.textarea}
              value={form.verificationCommandsText}
              onChange={(e: ChangeEvent<HTMLTextAreaElement>) =>
                onChange({ ...form, verificationCommandsText: e.target.value })
              }
              rows={6}
              spellCheck={false}
            />
            <p className={styles.helper}>{t('settings:automation.verificationCommandsHint')}</p>
          </div>
        </Card.Body>
      </Card>

      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings:automation.deliveryTitle')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          <p className={styles.helper}>{t('settings:automation.deliveryDescription')}</p>
          <div className={styles.toggleList}>
            <ToggleRow
              label={t('settings:automation.autoCommit')}
              helper={t('settings:automation.autoCommitHint')}
              checked={form.autoCommit}
              onToggle={(autoCommit) => onChange({ ...form, autoCommit })}
            />
            <ToggleRow
              label={t('settings:automation.autoPushTaskBranch')}
              helper={t('settings:automation.autoPushTaskBranchHint')}
              checked={form.autoPushTaskBranch}
              onToggle={(autoPushTaskBranch) => onChange({ ...form, autoPushTaskBranch })}
            />
            <ToggleRow
              label={t('settings:automation.autoMergeToMain')}
              helper={t('settings:automation.autoMergeToMainHint')}
              checked={form.autoMergeToMain}
              onToggle={(autoMergeToMain) => onChange({ ...form, autoMergeToMain })}
            />
            <ToggleRow
              label={t('settings:automation.autoPushMain')}
              helper={t('settings:automation.autoPushMainHint')}
              checked={form.autoPushMain}
              onToggle={(autoPushMain) => onChange({ ...form, autoPushMain })}
            />
          </div>
        </Card.Body>
      </Card>

      <Card variant="flat" padding="md">
        <Card.Header>
          <h2 className={styles.sectionTitle}>{t('settings:automation.notificationsTitle')}</h2>
        </Card.Header>
        <Card.Body padding="md">
          <p className={styles.helper}>{t('settings:automation.notificationsDescription')}</p>
          <div className={styles.toggleList}>
            <ToggleRow
              label={t('settings:automation.notifyHumanReview')}
              helper={t('settings:automation.notifyHumanReviewHint')}
              checked={form.notifyHumanReview}
              onToggle={(notifyHumanReview) => onChange({ ...form, notifyHumanReview })}
            />
            <ToggleRow
              label={t('settings:automation.notifyBlocked')}
              helper={t('settings:automation.notifyBlockedHint')}
              checked={form.notifyBlocked}
              onToggle={(notifyBlocked) => onChange({ ...form, notifyBlocked })}
            />
            <ToggleRow
              label={t('settings:automation.notifyRemoteOutboxFailed')}
              helper={t('settings:automation.notifyRemoteOutboxFailedHint')}
              checked={form.notifyRemoteOutboxFailed}
              onToggle={(notifyRemoteOutboxFailed) =>
                onChange({ ...form, notifyRemoteOutboxFailed })
              }
            />
            <ToggleRow
              label={t('settings:automation.notifyTaskDone')}
              helper={t('settings:automation.notifyTaskDoneHint')}
              checked={form.notifyTaskDone}
              onToggle={(notifyTaskDone) => onChange({ ...form, notifyTaskDone })}
            />
          </div>

          <div className={styles.footer}>
            <div className={styles.footerLeft}>
              {error ? (
                <span className={styles.updateError}>{error}</span>
              ) : dirty ? (
                <span className={styles.dirtyHint}>{t('settings:automation.dirty')}</span>
              ) : saved ? (
                <span className={styles.savedHint}>{t('settings:automation.saved')}</span>
              ) : null}
            </div>
            <div className={styles.footerActions}>
              <Button
                variant="ghost"
                size="md"
                onClick={onResetDefaults}
                disabled={resetDisabled}
                title={
                  canResetDefaults ? undefined : t('settings:resource.defaultsUnavailable')
                }
              >
                {t('settings:action.resetDefault')}
              </Button>
              <Button variant="primary" size="md" onClick={onSave} disabled={!dirty || saving}>
                {saving ? t('settings:action.applying') : t('settings:action.apply')}
              </Button>
            </div>
          </div>
        </Card.Body>
      </Card>
    </>
  );
}
