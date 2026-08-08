/**
 * InternalClaudeProviderCard — Settings AI tab 的「内部 Claude provider 覆盖」卡。
 *
 * Business Logic（为什么需要这个组件）:
 *   commit/merge/prompt 优化/GitHub 解说/verifier 等内部 headless Claude 调用默认继承
 *   OS 默认 provider（`~/.claude/settings.json`，由 cc-switch 维护）。用户希望这些**内部**调用
 *   使用一个不同的 cc-switch provider，且不改写 OS 默认（不与交互式 Claude 会话争用）。
 *   本卡只持久化所选 provider **id**（不含凭据）；后端运行时从 cc-switch 读取 settings_config
 *   写入隔离 `CLAUDE_CONFIG_DIR`。
 *
 *   自包含（与 CcSwitchCliDependencyCard 同构）：自行管理状态/调用 API，
 *   SettingsAiPanel 只需放置一行，无需改 Settings controller/props（守住 panel pure ownership）。
 *
 * Code Logic（这个组件做什么）:
 *   mount 时读 internal_claude 配置 + cc-switch claude provider 列表（只读）；
 *   下拉选择 provider（或「沿用 OS 默认」）；Apply 写入 update_internal_claude_config 后重读；
 *   Recheck 重新拉取 provider 列表（用户在 cc-switch 新增 provider 后刷新）。
 */

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, StatusMessage } from '@/components/primitives';
import { internalClaudeApi } from '@/api/internalClaude';
import { providerManagerApi } from '@/api/providerManager';
import type { ProviderEntry } from '@/lib/types/providerManager';
import styles from './InternalClaudeProviderCard.module.css';

/** 从 cc-switch 全量列表中取 claude provider（隐藏 0 provider 的 app 已被后端过滤）。 */
async function loadClaudeProviders(): Promise<ProviderEntry[]> {
  const apps = await providerManagerApi.list();
  const claude = apps.find((a) => a.app === 'claude');
  return claude?.providers ?? [];
}

/**
 * Business Logic: 自包含内部 Claude provider 卡。
 * Code Logic: 本地 state 管理 providers/applied/draft/loading/saving/error；mount 加载一次。
 */
export function InternalClaudeProviderCard(): React.ReactElement {
  const { t } = useTranslation(['settings', 'common']);
  const [providers, setProviders] = useState<ProviderEntry[]>([]);
  const [appliedProviderId, setAppliedProviderId] = useState<string | null>(null);
  const [draftId, setDraftId] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savingNotice, setSavingNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setListError(null);
    try {
      const [list, cfg] = await Promise.all([
        loadClaudeProviders(),
        internalClaudeApi.getConfig(),
      ]);
      setProviders(list);
      const id = cfg.providerId ?? '';
      setAppliedProviderId(id);
      setDraftId(id);
    } catch (err) {
      setListError(
        err instanceof Error
          ? err.message
          : t('settings:internalClaude.loadFailed', { error: String(err) }),
      );
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    // Initial/config reload path; setState lives inside async load after await.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- mount load entry
    void load();
  }, [load]);

  const apply = useCallback(async () => {
    setSaving(true);
    setSaveError(null);
    setSavingNotice(null);
    try {
      const cfg = await internalClaudeApi.updateConfig({ providerId: draftId });
      setAppliedProviderId(cfg.providerId ?? '');
      setSavingNotice(t('settings:internalClaude.applied'));
    } catch (err) {
      setSaveError(
        err instanceof Error
          ? err.message
          : t('settings:internalClaude.saveFailed', { error: String(err) }),
      );
    } finally {
      setSaving(false);
    }
  }, [draftId, t]);

  const dirty = draftId !== (appliedProviderId ?? '');
  const appliedProvider = providers.find((p) => p.id === appliedProviderId);
  // 已应用但 cc-switch 列表里已不存在（被删除）。
  const appliedMissing =
    appliedProviderId !== null && appliedProviderId !== '' && !appliedProvider;

  return (
    <Card variant="flat" padding="md">
      <Card.Header>
        <h2 className={styles.title}>{t('settings:internalClaude.title')}</h2>
      </Card.Header>
      <Card.Body padding="md">
        <p className={styles.helper}>{t('settings:internalClaude.subtitle')}</p>

        <div className={styles.field}>
          <label className={styles.label} htmlFor="settings-internal-claude-provider">
            {t('settings:internalClaude.provider.label')}
          </label>
          <select
            id="settings-internal-claude-provider"
            className={styles.select}
            value={draftId}
            onChange={(e) => setDraftId(e.target.value)}
            disabled={loading || saving}
          >
            <option value="">{t('settings:internalClaude.provider.osDefault')}</option>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
                {p.category ? ` · ${p.category}` : ''}
                {p.isCurrent ? ` · ${t('settings:internalClaude.provider.ccSwitchCurrent')}` : ''}
              </option>
            ))}
            {appliedMissing ? (
              <option value={appliedProviderId ?? ''}>
                {t('settings:internalClaude.provider.missing', { id: appliedProviderId ?? '' })}
              </option>
            ) : null}
          </select>
          <p className={styles.helper}>{t('settings:internalClaude.provider.helper')}</p>
        </div>

        {appliedProviderId !== null ? (
          <div className={styles.metaRow}>
            <span className={styles.metaKey}>{t('settings:internalClaude.appliedLabel')}</span>
            <span className={styles.metaValue}>
              {appliedProvider
                ? appliedProvider.name
                : appliedProviderId === ''
                  ? t('settings:internalClaude.provider.osDefault')
                  : t('settings:internalClaude.provider.missing', { id: appliedProviderId })}
            </span>
          </div>
        ) : null}

        {appliedMissing ? (
          <StatusMessage tone="warn">{t('settings:internalClaude.provider.missingNotice')}</StatusMessage>
        ) : null}

        {listError ? (
          <StatusMessage tone="danger">
            {t('settings:internalClaude.loadFailed', { error: listError })}
          </StatusMessage>
        ) : null}

        {saveError ? (
          <StatusMessage tone="danger">
            {t('settings:internalClaude.saveFailed', { error: saveError })}
          </StatusMessage>
        ) : null}

        {savingNotice ? (
          <StatusMessage tone="success">{savingNotice}</StatusMessage>
        ) : null}

        {loading && providers.length === 0 ? (
          <p className={styles.helper}>{t('common:loading')}</p>
        ) : null}

        <div className={styles.actions}>
          <Button variant="ghost" size="md" onClick={load} disabled={loading || saving}>
            {t('settings:internalClaude.recheck')}
          </Button>
          <Button variant="primary" size="md" onClick={apply} disabled={saving || !dirty}>
            {saving ? t('settings:internalClaude.applying') : t('settings:internalClaude.apply')}
          </Button>
        </div>
      </Card.Body>
    </Card>
  );
}
