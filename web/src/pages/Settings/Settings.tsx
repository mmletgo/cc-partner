/**
 * Settings 页面 - 偏好设置中心
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要集中调整设备名、接收目录、快捷键、同步策略等运行时偏好，
 *   改变会通过表单即时反映在 UI 状态中；"保存"按钮在用户主动提交时
 *   把整张配置表发送到后端持久化，区分"未保存修改"和"已保存配置"。
 *
 * Code Logic（这个页面做什么）:
 *   - 顶部 4 个 Card 区块：基本设置 / 快捷键 / 同步与存储 / 关于
 *   - 组件挂载时从后端加载配置和版本信息
 *   - Toggle 控件内联实现，避免引入额外 Switch 组件；状态切换走
 *     受控的 onClick + role="switch" + aria-checked
 *   - 底部按钮组：恢复默认走 ghost 风格重置状态、保存走 primary 风格
 *     调用后端 API 持久化
 *   - 所有用户可见文案经 i18next 翻译（settings ns + common ns）
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon, DevicesIcon, FolderIcon, KeyboardIcon, SyncIcon, InfoIcon, DownloadIcon } from '@/lib/icons';
import { configApi } from '@/api/config';
import type { VersionInfo, UpdateCheckResult, UpdateDownloadStatus } from '@/lib/types';
import styles from './Settings.module.css';

/** 单个快捷键字段定义（label/helper 在渲染时按 i18n 解析，这里只存可本地化的 id） */
interface ShortcutField {
  id: string;
  /** label/helper 的 i18n 子键，对应 shortcut.<key>.{label,helper} */
  labelKey: 'screenshot' | 'toggleWindow' | 'openSettings' | 'quickSend';
  value: string;
}

/** 同步与存储开关定义（label/helper 同样按 i18n 解析） */
interface ToggleField {
  id: string;
  /** label/helper 的 i18n 子键，对应 sync.<key>.{label,helper} */
  labelKey: 'autoSync' | 'saveHistory' | 'encryptSensitive';
  enabled: boolean;
}

/** Settings 页面整体表单状态 */
interface SettingsState {
  deviceName: string;
  receiveDir: string;
  shortcuts: ShortcutField[];
  toggles: ToggleField[];
}

/** 快捷键字段定义（值本地化，文案走 t） */
const SHORTCUT_FIELDS: ShortcutField[] = [
  { id: 'screenshot', labelKey: 'screenshot', value: 'Cmd+Shift+S' },
  { id: 'toggle-window', labelKey: 'toggleWindow', value: 'Cmd+Shift+P' },
  { id: 'open-settings', labelKey: 'openSettings', value: 'Cmd+,' },
  { id: 'quick-send', labelKey: 'quickSend', value: 'Cmd+Shift+U' },
];

/** 同步与存储开关定义（enabled 走默认值，文案走 t） */
const TOGGLE_FIELDS: ToggleField[] = [
  { id: 'auto-sync', labelKey: 'autoSync', enabled: true },
  { id: 'save-history', labelKey: 'saveHistory', enabled: true },
  { id: 'encrypt-prompts', labelKey: 'encryptSensitive', enabled: false },
];

/** 默认快捷键字段（深拷贝，避免污染常量） */
const DEFAULT_SHORTCUTS: ShortcutField[] = SHORTCUT_FIELDS.map((s) => ({ ...s }));

/** 默认同步开关（深拷贝，避免污染常量） */
const DEFAULT_TOGGLES: ToggleField[] = TOGGLE_FIELDS.map((t) => ({ ...t }));

/**
 * 生成默认状态
 *
 * @returns 仅含可本地化 id 的默认 SettingsState
 */
function createDefaultState(): SettingsState {
  return {
    deviceName: '',
    receiveDir: '',
    shortcuts: DEFAULT_SHORTCUTS.map((s) => ({ ...s })),
    toggles: DEFAULT_TOGGLES.map((t) => ({ ...t })),
  };
}

/**
 * 计算更新检查结果的提示文本
 *
 * @param updateResult 更新检查结果
 * @param checkingUpdate 是否正在检查
 * @param t i18next 翻译函数（settings ns）
 * @returns 当前应展示的提示文本
 */
function buildUpdateHint(
  updateResult: UpdateCheckResult | null,
  checkingUpdate: boolean,
  t: TFunction<'settings'>,
): string {
  if (checkingUpdate) return t('about.checkingHint');
  if (!updateResult) return t('about.upToDate');
  if (updateResult.error) return updateResult.error;
  if (updateResult.hasUpdate) return t('about.newVersionFound', { version: updateResult.version });
  return t('about.upToDate');
}

/**
 * Settings 页面组件
 *
 * @returns Settings 路由的根容器
 */
export function Settings() {
  const { t } = useTranslation(['settings', 'common']);
  const [state, setState] = useState<SettingsState>(createDefaultState);
  const initialStateRef = useRef<SettingsState>(state);
  const [savedAt, setSavedAt] = useState<Date | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [versionInfo, setVersionInfo] = useState<VersionInfo | null>(null);
  const [updateResult, setUpdateResult] = useState<UpdateCheckResult | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<UpdateDownloadStatus | null>(null);
  const [installing, setInstalling] = useState(false);
  const [saving, setSaving] = useState(false);
  const [choosingDir, setChoosingDir] = useState(false);

  // 计算是否处于"未保存"状态
  const isDirty = useMemo(() => {
    return JSON.stringify(state) !== JSON.stringify(initialStateRef.current);
  }, [state]);

  // 渲染更新检查结果的提示文本
  const updateHint = useMemo(
    () => buildUpdateHint(updateResult, checkingUpdate, t),
    [updateResult, checkingUpdate, t],
  );

  /**
   * 通用字段更新：merge 浅层部分字段
   *
   * @param partial 待合并的字段
   */
  const patchState = useCallback((partial: Partial<SettingsState>) => {
    setState((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * 处理 deviceName 输入
   *
   * @param e change 事件
   */
  const handleDeviceNameChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ deviceName: e.target.value });
  };

  /**
   * 处理 receiveDir 输入
   *
   * @param e change 事件
   */
  const handleReceiveDirChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ receiveDir: e.target.value });
  };

  /**
   * 处理快捷键输入
   *
   * @param id 快捷键 id
   * @param value 新的按键字符串
   */
  const handleShortcutChange = useCallback((id: string, value: string) => {
    setState((prev) => ({
      ...prev,
      shortcuts: prev.shortcuts.map((s) => (s.id === id ? { ...s, value } : s)),
    }));
  }, []);

  /**
   * 切换某项 toggle 的开关
   *
   * @param id toggle id
   */
  const handleToggleClick = useCallback((id: string) => {
    setState((prev) => ({
      ...prev,
      toggles: prev.toggles.map((t) => (t.id === id ? { ...t, enabled: !t.enabled } : t)),
    }));
  }, []);

  /**
   * 恢复默认：重置 state 到默认值
   */
  const handleResetDefaults = () => {
    const next = createDefaultState();
    setState(next);
  };

  /**
   * 打开原生目录选择对话框，将返回路径写入 receiveDir
   */
  const handleChooseDir = async () => {
    setChoosingDir(true);
    try {
      const result = await configApi.chooseDir();
      if (result.path) {
        patchState({ receiveDir: result.path });
      }
    } catch {
      // 目录选择取消或失败时静默处理
    } finally {
      setChoosingDir(false);
    }
  };

  /**
   * 保存按钮：把当前 state 发送到后端持久化
   */
  const handleSave = async () => {
    setSaving(true);
    try {
      await configApi.update({
        deviceName: state.deviceName,
        receiveDir: state.receiveDir,
        screenshotHotkey: state.shortcuts.find((s) => s.id === 'screenshot')?.value,
      });
      initialStateRef.current = state;
      setSavedAt(new Date());
    } catch (err) {
      // 保存失败时在 UI 提示错误
      setLoadError(err instanceof Error ? err.message : t('error.saveFailed'));
    } finally {
      setSaving(false);
    }
  };

  /**
   * 检查更新按钮：调用后端 updater/check 接口
   */
  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpdateResult(null);
    setDownloadStatus(null);
    try {
      const result = await configApi.checkUpdate();
      setUpdateResult(result);
    } catch (err) {
      setUpdateResult({
        hasUpdate: false,
        error: err instanceof Error ? err.message : t('error.checkFailed'),
      });
    } finally {
      setCheckingUpdate(false);
    }
  };

  // 组件挂载时从后端加载配置和版本信息
  useEffect(() => {
    let cancelled = false;

    async function loadConfig() {
      try {
        const [config, version] = await Promise.all([
          configApi.get(),
          configApi.version(),
        ]);
        if (cancelled) return;

        const loaded: SettingsState = {
          deviceName: config.deviceName,
          receiveDir: config.receiveDir,
          shortcuts: DEFAULT_SHORTCUTS.map((s) => {
            if (s.id === 'screenshot') {
              return { ...s, value: config.screenshotHotkey || s.value };
            }
            return { ...s };
          }),
          toggles: DEFAULT_TOGGLES.map((t) => ({ ...t })),
        };
        setState(loaded);
        initialStateRef.current = loaded;
        setVersionInfo(version);
      } catch (err) {
        if (cancelled) return;
        setLoadError(err instanceof Error ? err.message : t('error.loadConfigFailed'));
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void loadConfig();
    return () => { cancelled = true; };
    // 仅在挂载时执行一次；t 在错误分支兜底，但依赖项保持挂载语义
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 下载进行中时轮询进度状态，每 800ms 一次；进入终态（completed/failed/cancelled）后停止
  useEffect(() => {
    if (downloadStatus?.status !== 'downloading') return;
    let active = true;
    const timer = window.setInterval(async () => {
      if (!active) return;
      try {
        const status = await configApi.getDownloadStatus();
        if (active) setDownloadStatus(status);
      } catch {
        // 轮询失败静默，下一轮重试
      }
    }, 800);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [downloadStatus?.status]);

  /**
   * 启动更新下载：透传检查结果的 downloadUrl/filename，立即进入 downloading 状态
   */
  const handleDownload = async () => {
    if (!updateResult?.downloadUrl || !updateResult?.filename) return;
    // 乐观进入 downloading，让进度条立即显示
    setDownloadStatus({
      status: 'downloading',
      progress: 0,
      error: '',
      filePath: '',
      url: updateResult.downloadUrl,
      filename: updateResult.filename,
      size: updateResult.size ?? 0,
    });
    try {
      await configApi.downloadUpdate(updateResult.downloadUrl, updateResult.filename);
    } catch (err) {
      setDownloadStatus({
        status: 'failed',
        progress: 0,
        error: err instanceof Error ? err.message : t('error.startDownloadFailed'),
        filePath: '',
        url: '',
        filename: '',
        size: 0,
      });
    }
  };

  /**
   * 取消正在进行的下载
   */
  const handleCancelDownload = async () => {
    try {
      await configApi.cancelDownload();
      setDownloadStatus((prev) =>
        prev ? { ...prev, status: 'cancelled' } : prev,
      );
    } catch {
      // 取消失败静默
    }
  };

  /**
   * 安装已下载的更新包并重启（进程随后退出）
   */
  const handleInstall = async () => {
    setInstalling(true);
    try {
      await configApi.installUpdate();
    } catch {
      // 安装失败静默，用户可重试
    } finally {
      setInstalling(false);
    }
  };

  // 加载状态
  if (loading) {
    return (
      <div className={styles.page}>
        <div className={styles.container}>
          <header className={styles.header}>
            <span className={styles.eyebrow}>PREFERENCES</span>
            <h1 className={styles.title}>{t('settings:title')}</h1>
            <p className={styles.lead}>{t('settings:loading')}</p>
          </header>
        </div>
      </div>
    );
  }

  // 加载失败状态
  if (loadError) {
    return (
      <div className={styles.page}>
        <div className={styles.container}>
          <header className={styles.header}>
            <span className={styles.eyebrow}>PREFERENCES</span>
            <h1 className={styles.title}>{t('settings:title')}</h1>
            <p className={styles.lead} style={{ color: 'var(--color-danger)' }}>
              {t('settings:loadFailed', { error: loadError })}
            </p>
          </header>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        {/* 页面头部 */}
        <header className={styles.header}>
          <span className={styles.eyebrow}>PREFERENCES</span>
          <h1 className={styles.title}>{t('settings:title')}</h1>
          <p className={styles.lead}>{t('settings:subtitle')}</p>
        </header>

        {/* Card 1: 基本设置 */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:basic.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-device-name">
                {t('settings:basic.deviceName')}
              </label>
              <div className={styles.inputRow}>
                <Input
                  id="settings-device-name"
                  type="text"
                  value={state.deviceName}
                  onChange={handleDeviceNameChange}
                  icon={<DevicesIcon />}
                />
              </div>
              <p className={styles.helper}>{t('settings:basic.deviceNameHelper')}</p>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-receive-dir">
                {t('settings:basic.receiveDir')}
              </label>
              <div className={styles.inputRow}>
                <Input
                  id="settings-receive-dir"
                  type="text"
                  value={state.receiveDir}
                  onChange={handleReceiveDirChange}
                  icon={<FolderIcon />}
                />
                <Button variant="secondary" size="md" onClick={handleChooseDir} disabled={choosingDir}>
                  {choosingDir ? t('settings:basic.selecting') : t('settings:basic.selectFolder')}
                </Button>
              </div>
              <p className={styles.helper}>{t('settings:basic.receiveDirHelper')}</p>
            </div>
          </Card.Body>
        </Card>

        {/* Card 2: 快捷键 */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:shortcut.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <div className={styles.shortcutList}>
              {state.shortcuts.map((s) => (
                <div key={s.id} className={styles.shortcutRow}>
                  <div className={styles.shortcutText}>
                    <span className={styles.shortcutLabel}>
                      {t(`settings:shortcut.${s.labelKey}.label`)}
                    </span>
                    <span className={styles.shortcutHelper}>
                      {t(`settings:shortcut.${s.labelKey}.helper`)}
                    </span>
                  </div>
                  <div className={styles.shortcutInput}>
                    <Input
                      type="text"
                      value={s.value}
                      onChange={(e) => handleShortcutChange(s.id, e.target.value)}
                      icon={<KeyboardIcon />}
                      mono
                    />
                  </div>
                </div>
              ))}
            </div>
          </Card.Body>
        </Card>

        {/* Card 3: 同步与存储 */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:sync.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <div className={styles.toggleList}>
              {state.toggles.map((t2) => (
                <button
                  key={t2.id}
                  type="button"
                  className={styles.toggleRow}
                  onClick={() => handleToggleClick(t2.id)}
                  role="switch"
                  aria-checked={t2.enabled}
                  aria-label={t(`settings:sync.${t2.labelKey}.label`)}
                >
                  <div className={styles.toggleText}>
                    <span className={styles.toggleLabel}>
                      {t(`settings:sync.${t2.labelKey}.label`)}
                    </span>
                    <span className={styles.toggleHelper}>
                      {t(`settings:sync.${t2.labelKey}.helper`)}
                    </span>
                  </div>
                  <span className={styles.toggleState}>
                    {t2.enabled ? (
                      <Pill tone="success" dot>
                        <CheckIcon size={12} />
                        {t('settings:sync.enabled')}
                      </Pill>
                    ) : (
                      <Pill tone="neutral" dot>
                        <XIcon size={12} />
                        {t('settings:sync.disabled')}
                      </Pill>
                    )}
                  </span>
                </button>
              ))}
            </div>
          </Card.Body>
        </Card>

        {/* Card 4: 关于 */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:about.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <dl className={styles.metaList}>
              <div className={styles.metaRow}>
                <dt className={styles.metaKey}>{t('settings:about.versionLabel')}</dt>
                <dd className={styles.metaValue}>
                  <Pill tone="accent">v{versionInfo?.version ?? '—'}</Pill>
                </dd>
              </div>
              <div className={styles.metaRow}>
                <dt className={styles.metaKey}>{t('settings:about.buildLabel')}</dt>
                <dd className={styles.metaValue}>{versionInfo?.buildDate ?? '—'}</dd>
              </div>
              <div className={styles.metaRow}>
                <dt className={styles.metaKey}>{t('settings:about.sourceLabel')}</dt>
                <dd className={styles.metaValue}>{t('settings:about.source')}</dd>
              </div>
            </dl>
            <div className={styles.aboutActions}>
              <Button
                variant="secondary"
                size="md"
                icon={<SyncIcon />}
                onClick={handleCheckUpdate}
                disabled={checkingUpdate}
              >
                {checkingUpdate ? t('settings:about.checking') : t('settings:about.checkUpdate')}
              </Button>
              <span className={styles.aboutHint}>
                <InfoIcon size={14} />
                <span>{updateHint}</span>
              </span>
            </div>

            {/* 发现新版本时展示：版本说明 + 下载/进度/安装 */}
            {updateResult?.hasUpdate ? (
              <div className={styles.updateBlock}>
                <div className={styles.metaRow}>
                  <span className={styles.metaKey}>{t('settings:about.latestVersion')}</span>
                  <Pill tone="accent">v{updateResult.version}</Pill>
                </div>
                {updateResult.body ? (
                  <p className={styles.updateBody}>{updateResult.body}</p>
                ) : null}

                {downloadStatus?.status === 'downloading' ? (
                  <div className={styles.progressRow}>
                    <div className={styles.progressBar}>
                      <div
                        className={styles.progressFill}
                        style={{ width: `${Math.round(downloadStatus.progress * 100)}%` }}
                      />
                    </div>
                    <span className={styles.progressText}>
                      {Math.round(downloadStatus.progress * 100)}%
                    </span>
                    <Button
                      variant="ghost"
                      size="sm"
                      icon={<XIcon size={14} />}
                      onClick={handleCancelDownload}
                    >
                      {t('settings:about.cancel')}
                    </Button>
                  </div>
                ) : downloadStatus?.status === 'completed' ? (
                  <div className={styles.updateActions}>
                    <Button
                      variant="primary"
                      size="sm"
                      icon={<DownloadIcon size={14} />}
                      onClick={handleInstall}
                      disabled={installing}
                    >
                      {installing ? t('settings:about.installing') : t('settings:about.installAndRestart')}
                    </Button>
                    <span className={styles.aboutHint}>{t('settings:about.downloadCompleted')}</span>
                  </div>
                ) : downloadStatus?.status === 'failed' ? (
                  <div className={styles.updateActions}>
                    <span className={styles.updateError}>
                      {downloadStatus.error || t('settings:about.downloadFailed')}
                    </span>
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<DownloadIcon size={14} />}
                      onClick={handleDownload}
                    >
                      {t('settings:about.retryDownload')}
                    </Button>
                  </div>
                ) : downloadStatus?.status === 'cancelled' ? (
                  <div className={styles.updateActions}>
                    <span className={styles.aboutHint}>{t('settings:about.downloadCancelled')}</span>
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<DownloadIcon size={14} />}
                      onClick={handleDownload}
                    >
                      {t('settings:about.redownload')}
                    </Button>
                  </div>
                ) : updateResult.downloadUrl ? (
                  <div className={styles.updateActions}>
                    <Button
                      variant="primary"
                      size="sm"
                      icon={<DownloadIcon size={14} />}
                      onClick={handleDownload}
                    >
                      {updateResult.size
                        ? t('settings:about.downloadUpdate', { size: formatSize(updateResult.size) })
                        : t('settings:about.downloadUpdate', { size: '' })}
                    </Button>
                  </div>
                ) : (
                  <span className={styles.aboutHint}>{t('settings:about.noAsset')}</span>
                )}
              </div>
            ) : null}
          </Card.Body>
        </Card>

        {/* 底部按钮组 */}
        <div className={styles.footer}>
          <div className={styles.footerLeft}>
            {isDirty ? (
              <span className={styles.dirtyHint}>{t('settings:status.dirtyHint')}</span>
            ) : savedAt ? (
              <span className={styles.savedHint}>
                {t('settings:status.savedAt', { time: formatTime(savedAt) })}
              </span>
            ) : null}
          </div>
          <div className={styles.footerActions}>
            <Button variant="ghost" onClick={handleResetDefaults} disabled={!isDirty}>
              {t('settings:action.resetDefault')}
            </Button>
            <Button variant="primary" onClick={handleSave} disabled={!isDirty || saving}>
              {saving ? t('settings:action.saving') : t('settings:action.save')}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * 把 Date 格式化为 "HH:MM:SS" 字符串
 *
 * @param d Date 实例
 * @returns 时间字符串
 */
function formatTime(d: Date): string {
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 把字节数格式化为人类可读的大小字符串（B/KB/MB/GB）
 *
 * @param bytes 字节数
 * @returns 形如 "12.3 MB" 的字符串
 */
function formatSize(bytes: number): string {
  if (!bytes) return '';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 100 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

Settings.displayName = 'Settings';
