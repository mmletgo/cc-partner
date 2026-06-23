/**
 * Settings 页面 - 偏好设置中心
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要集中调整设备名、接收目录、截图快捷键、云端同步等运行时偏好，
 *   改变会通过表单即时反映在 UI 状态中；"保存"按钮在用户主动提交时
 *   把整张配置表发送到后端持久化，区分"未保存修改"和"已保存配置"。
 *
 * Code Logic（这个页面做什么）:
 *   - 子 tab：常规 / 同步 / AI / 关于，把既有 Card 按查看任务分组
 *   - Card 区块：基本设置 / 权限管理 / 截图快捷键 / 云端同步 / GitHub Trending / 关于
 *   - 组件挂载时从后端加载配置和版本信息
 *   - Toggle 控件内联实现，避免引入额外 Switch 组件；状态切换走
 *     受控的 onClick + role="switch" + aria-checked
 *   - 底部按钮组：恢复默认走 ghost 风格重置状态、保存走 primary 风格
 *     调用后端 API 持久化
 *   - 所有用户可见文案经 i18next 翻译（settings ns + common ns）
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChangeEvent, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { PermissionCard } from '@/components/domain';
import { CheckIcon, XIcon, DevicesIcon, FolderIcon, KeyboardIcon, SyncIcon, InfoIcon, DownloadIcon } from '@/lib/icons';
import { configApi } from '@/api/config';
import { githubTrendingApi } from '@/api/githubTrending';
import { usePermissions } from '@/hooks/usePermissions';
import { mapPermissions } from '@/lib/permissionEntries';
import type {
  VersionInfo,
  UpdateCheckResult,
  UpdateDownloadStatus,
  PermissionType,
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
  ClaudeCliTestResult,
  GithubTrendingConfig,
} from '@/lib/types';
import styles from './Settings.module.css';

/** 单个快捷键字段定义（label/helper 在渲染时按 i18n 解析，这里只存可本地化的 id） */
interface ShortcutField {
  id: string;
  /** label/helper 的 i18n 子键，对应 shortcut.<key>.{label,helper} */
  labelKey: 'screenshot';
  value: string;
}

/** Settings 页面整体表单状态 */
interface SettingsState {
  deviceName: string;
  receiveDir: string;
  shortcuts: ShortcutField[];
}

/** 云端同步 Card 的可编辑表单值（受控输入，与已应用配置分离） */
interface CloudSyncForm {
  repoUrl: string;
  branch: string;
  enabled: boolean;
  auto: boolean;
  intervalSecs: number;
}

/** GitHub Trending / Claude CLI 解说 Card 的受控表单值 */
interface GithubTrendingForm {
  aiEnabled: boolean;
  claudeCliPath: string;
  claudeModel: string;
  cacheTtlHours: number;
}

/** Settings 页内子 tab id */
type SettingsTabId = 'general' | 'sync' | 'ai' | 'about';

/** Settings 页内子 tab 定义 */
interface SettingsTab {
  id: SettingsTabId;
  labelKey: SettingsTabId;
}

/** Settings 页内子 tab 顺序：按用户查看任务组织，而不是按底层配置来源组织 */
const SETTINGS_TABS: SettingsTab[] = [
  { id: 'general', labelKey: 'general' },
  { id: 'sync', labelKey: 'sync' },
  { id: 'ai', labelKey: 'ai' },
  { id: 'about', labelKey: 'about' },
];

/** 快捷键字段定义（值本地化，文案走 t） */
const SHORTCUT_FIELDS: ShortcutField[] = [
  { id: 'screenshot', labelKey: 'screenshot', value: 'Cmd+Shift+S' },
];

/** 默认快捷键字段（深拷贝，避免污染常量） */
const DEFAULT_SHORTCUTS: ShortcutField[] = SHORTCUT_FIELDS.map((s) => ({ ...s }));

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

/** 云端同步表单的默认值（用于初次加载前的占位） */
const DEFAULT_CLOUD_SYNC_FORM: CloudSyncForm = {
  repoUrl: '',
  branch: '',
  enabled: false,
  auto: false,
  intervalSecs: 300,
};

/** GitHub Trending 表单默认值（加载后会被后端配置覆盖） */
const DEFAULT_GITHUB_TRENDING_FORM: GithubTrendingForm = {
  aiEnabled: true,
  claudeCliPath: 'claude',
  claudeModel: 'sonnet',
  cacheTtlHours: 24,
};

/**
 * 将后端返回的 CloudSyncConfig 映射为受控表单值
 *
 * @param config 后端云端同步配置（可能为 null）
 * @returns 表单初始值
 */
function cloudSyncConfigToForm(config: CloudSyncConfig | null): CloudSyncForm {
  if (!config) return { ...DEFAULT_CLOUD_SYNC_FORM };
  return {
    repoUrl: config.repoUrl ?? '',
    branch: config.branch ?? '',
    enabled: config.enabled,
    auto: config.auto,
    intervalSecs: config.intervalSecs,
  };
}

/**
 * 将后端返回的 GithubTrendingConfig 映射为受控表单值
 */
function githubTrendingConfigToForm(config: GithubTrendingConfig | null): GithubTrendingForm {
  if (!config) return { ...DEFAULT_GITHUB_TRENDING_FORM };
  return {
    aiEnabled: config.aiEnabled,
    claudeCliPath: config.claudeCliPath || 'claude',
    claudeModel: config.claudeModel || 'sonnet',
    cacheTtlHours: config.cacheTtlHours,
  };
}

/**
 * 把 ISO 时间字符串格式化为 "HH:MM:SS" 本地时间
 *
 * @param iso ISO 时间字符串
 * @returns 形如 "12:34:56" 的本地时间
 */
function formatIsoTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * Settings 页面组件
 *
 * @returns Settings 路由的根容器
 */
export function Settings() {
  const { t } = useTranslation(['settings', 'common']);
  const [state, setState] = useState<SettingsState>(createDefaultState);
  // 最近一次"已保存/已加载"的配置快照，用于检测是否处于未保存状态
  const [initialState, setInitialState] = useState<SettingsState>(createDefaultState);
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
  const [activeTab, setActiveTab] = useState<SettingsTabId>('general');

  // 云端同步（GitHub 私有仓库）独立操作块：表单值 / 已应用配置 / 上次同步结果 / 测试结果 / 各动作 loading
  const [cloudSyncForm, setCloudSyncForm] = useState<CloudSyncForm>({ ...DEFAULT_CLOUD_SYNC_FORM });
  const [cloudSync, setCloudSync] = useState<CloudSyncConfig | null>(null);
  const [syncResult, setSyncResult] = useState<CloudSyncResult | null>(null);
  const [testResult, setTestResult] = useState<TestCloudSyncResult | null>(null);
  const [cloudSyncError, setCloudSyncError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [applying, setApplying] = useState(false);
  const [syncing, setSyncing] = useState(false);

  // GitHub Trending / Claude CLI 解说配置：独立于底部统一 Save，即时应用。
  const [githubTrendingForm, setGithubTrendingForm] = useState<GithubTrendingForm>({
    ...DEFAULT_GITHUB_TRENDING_FORM,
  });
  const [githubTrendingConfig, setGithubTrendingConfig] = useState<GithubTrendingConfig | null>(null);
  const [claudeCliTest, setClaudeCliTest] = useState<ClaudeCliTestResult | null>(null);
  const [githubTrendingError, setGithubTrendingError] = useState<string | null>(null);
  const [testingClaudeCli, setTestingClaudeCli] = useState(false);
  const [applyingGithubTrending, setApplyingGithubTrending] = useState(false);

  // macOS 权限状态（设置页手动授权入口，持续轮询以反映用户在系统设置的变更）
  const [tWelcome] = useTranslation('welcome');
  const { status: permStatus, loading: permLoading, refresh: refreshPermissions } = usePermissions();

  /**
   * 页内 tab 键盘切换：支持左右方向键 / Home / End，保持 tablist 可访问性
   *
   * @param e 当前 tab button 的键盘事件
   * @param currentIndex 当前 tab 在 SETTINGS_TABS 中的索引
   */
  const handleTabKeyDown = useCallback((e: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
    let nextIndex: number | null = null;
    if (e.key === 'ArrowRight') {
      nextIndex = (currentIndex + 1) % SETTINGS_TABS.length;
    } else if (e.key === 'ArrowLeft') {
      nextIndex = (currentIndex - 1 + SETTINGS_TABS.length) % SETTINGS_TABS.length;
    } else if (e.key === 'Home') {
      nextIndex = 0;
    } else if (e.key === 'End') {
      nextIndex = SETTINGS_TABS.length - 1;
    }

    if (nextIndex === null) return;
    e.preventDefault();
    const nextTab = SETTINGS_TABS[nextIndex];
    setActiveTab(nextTab.id);
    window.requestAnimationFrame(() => {
      document.getElementById(`settings-tab-${nextTab.id}`)?.focus();
    });
  }, []);

  /**
   * 单项权限「去设置」：请求该项权限（默认弹框 + 开面板）后刷新状态
   *
   * @param type 权限类型 screenCapture / inputMonitoring
   */
  const handleRequestAccess = useCallback(
    async (type: PermissionType) => {
      try {
        await configApi.requestPermission(type);
        await refreshPermissions();
      } catch {
        // 请求失败静默，轮询会持续反映真实状态
      }
    },
    [refreshPermissions],
  );

  // 计算是否处于"未保存"状态：当前 state 与最近一次已保存/已加载的快照是否一致
  const isDirty = useMemo(() => {
    return JSON.stringify(state) !== JSON.stringify(initialState);
  }, [state, initialState]);

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
      // 保存成功后，把已保存快照更新为当前 state，使 isDirty 归零
      setInitialState(state);
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
        const [config, version, cloudSyncConfig, githubTrendingLoaded] = await Promise.all([
          configApi.get(),
          configApi.version(),
          configApi.getCloudSyncConfig(),
          githubTrendingApi.getConfig(),
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
        };
        setState(loaded);
        // 把已加载配置作为"未保存"比较的基准快照
        setInitialState(loaded);
        setVersionInfo(version);
        // 云端同步：初始化已应用配置与受控表单值
        setCloudSync(cloudSyncConfig);
        setCloudSyncForm(cloudSyncConfigToForm(cloudSyncConfig));
        // GitHub Trending：初始化已应用配置与受控表单值
        setGithubTrendingConfig(githubTrendingLoaded);
        setGithubTrendingForm(githubTrendingConfigToForm(githubTrendingLoaded));
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

  /**
   * 更新云端同步表单的某个字段（浅合并）
   *
   * @param partial 待合并的字段
   */
  const patchCloudSyncForm = useCallback((partial: Partial<CloudSyncForm>) => {
    setCloudSyncForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * 云端同步「测试连接」：探测 git 可用性与仓库默认分支
   */
  const handleTestCloudSync = async () => {
    setTesting(true);
    setCloudSyncError(null);
    setTestResult(null);
    try {
      const result = await configApi.testCloudSync();
      setTestResult(result);
    } catch (err) {
      setTestResult({
        ok: false,
        gitVersion: null,
        defaultBranch: null,
        error: err instanceof Error ? err.message : t('cloudSync.testFailed', { error: '' }).trim(),
      });
    } finally {
      setTesting(false);
    }
  };

  /**
   * 云端同步「应用配置」：把当前表单值提交到后端，并用返回值刷新已应用配置
   */
  const handleApplyCloudSync = async () => {
    setApplying(true);
    setCloudSyncError(null);
    try {
      const updated = await configApi.updateCloudSyncConfig({
        repoUrl: cloudSyncForm.repoUrl.trim() || null,
        enabled: cloudSyncForm.enabled,
        auto: cloudSyncForm.auto,
        intervalSecs: cloudSyncForm.intervalSecs,
        branch: cloudSyncForm.branch.trim() || null,
      });
      setCloudSync(updated);
      setCloudSyncForm(cloudSyncConfigToForm(updated));
    } catch (err) {
      setCloudSyncError(err instanceof Error ? err.message : t('cloudSync.applyFailed'));
    } finally {
      setApplying(false);
    }
  };

  /**
   * 云端同步「立即同步」：触发一次 pull + push，展示结果
   */
  const handleSyncNow = async () => {
    setSyncing(true);
    setCloudSyncError(null);
    try {
      const result = await configApi.triggerCloudSync();
      setSyncResult(result);
    } catch (err) {
      setSyncResult({
        ok: false,
        pulled: 0,
        pushed: 0,
        note: err instanceof Error ? err.message : t('cloudSync.syncFailed', { time: '', note: '' }),
        syncedAt: new Date().toISOString(),
      });
    } finally {
      setSyncing(false);
    }
  };

  /**
   * 更新 GitHub Trending 表单字段
   */
  const patchGithubTrendingForm = useCallback((partial: Partial<GithubTrendingForm>) => {
    setGithubTrendingForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * GitHub Trending「应用配置」：保存 Claude CLI 路径、模型与缓存设置
   */
  const handleApplyGithubTrending = async () => {
    setApplyingGithubTrending(true);
    setGithubTrendingError(null);
    try {
      const updated = await githubTrendingApi.updateConfig({
        aiEnabled: githubTrendingForm.aiEnabled,
        claudeCliPath: githubTrendingForm.claudeCliPath.trim() || 'claude',
        claudeModel: githubTrendingForm.claudeModel.trim() || 'sonnet',
        cacheTtlHours: githubTrendingForm.cacheTtlHours,
      });
      setGithubTrendingConfig(updated);
      setGithubTrendingForm(githubTrendingConfigToForm(updated));
    } catch (err) {
      setGithubTrendingError(err instanceof Error ? err.message : t('githubTrending.applyFailed'));
    } finally {
      setApplyingGithubTrending(false);
    }
  };

  /**
   * GitHub Trending「测试 Claude CLI」：只跑 --version，不触发 AI 生成
   */
  const handleTestClaudeCli = async () => {
    setTestingClaudeCli(true);
    setGithubTrendingError(null);
    setClaudeCliTest(null);
    try {
      const result = await githubTrendingApi.testClaudeCli(githubTrendingForm.claudeCliPath);
      setClaudeCliTest(result);
    } catch (err) {
      setClaudeCliTest({
        ok: false,
        version: null,
        error: err instanceof Error ? err.message : t('githubTrending.testFailed', { error: '' }).trim(),
      });
    } finally {
      setTestingClaudeCli(false);
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
            <p className={`${styles.lead} ${styles.dangerText}`}>
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

        <div className={styles.tabs} role="tablist" aria-label={t('settings:tabsLabel')}>
          {SETTINGS_TABS.map((tab, index) => (
            <button
              key={tab.id}
              id={`settings-tab-${tab.id}`}
              type="button"
              role="tab"
              aria-selected={activeTab === tab.id}
              aria-controls={`settings-panel-${tab.id}`}
              tabIndex={activeTab === tab.id ? 0 : -1}
              className={activeTab === tab.id ? styles.tabActive : styles.tab}
              onClick={() => setActiveTab(tab.id)}
              onKeyDown={(e) => handleTabKeyDown(e, index)}
            >
              {t(`settings:tabs.${tab.labelKey}`)}
            </button>
          ))}
        </div>

        {activeTab === 'general' ? (
          <div
            id="settings-panel-general"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-general"
          >
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

        {/* Card: 权限管理（macOS 手动授权入口） */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:permission.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            {permLoading || !permStatus ? (
              <p className={styles.helper}>{t('settings:permission.checking')}</p>
            ) : (
              <div className={styles.permissionList}>
                {mapPermissions(permStatus, tWelcome).map((p) => (
                  <PermissionCard
                    key={p.id}
                    icon={p.icon}
                    title={p.title}
                    description={p.description}
                    granted={p.granted}
                    onRequestAccess={() => void handleRequestAccess(p.id as PermissionType)}
                  />
                ))}
              </div>
            )}
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

        {/* 底部按钮组：只保存常规 tab 的基础配置 */}
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
        ) : null}

        {activeTab === 'sync' ? (
          <div
            id="settings-panel-sync"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-sync"
          >
        {/* Card: 云端同步（GitHub 私有仓库，独立操作块，不混入底部统一 Save） */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:cloudSync.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <p className={styles.helper}>{t('settings:cloudSync.subtitle')}</p>

            {/* 仓库地址 */}
            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-cloud-repo-url">
                {t('settings:cloudSync.repoUrl.label')}
              </label>
              <Input
                id="settings-cloud-repo-url"
                type="text"
                value={cloudSyncForm.repoUrl}
                onChange={(e) => patchCloudSyncForm({ repoUrl: e.target.value })}
                mono
              />
              <p className={styles.helper}>{t('settings:cloudSync.repoUrl.helper')}</p>
            </div>

            {/* 分支 */}
            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-cloud-branch">
                {t('settings:cloudSync.branch.label')}
              </label>
              <Input
                id="settings-cloud-branch"
                type="text"
                value={cloudSyncForm.branch}
                onChange={(e) => patchCloudSyncForm({ branch: e.target.value })}
                mono
              />
              <p className={styles.helper}>{t('settings:cloudSync.branch.helper')}</p>
            </div>

            {/* 同步间隔 */}
            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-cloud-interval">
                {t('settings:cloudSync.interval.label')}
              </label>
              <Input
                id="settings-cloud-interval"
                type="number"
                value={cloudSyncForm.intervalSecs}
                onChange={(e) =>
                  patchCloudSyncForm({ intervalSecs: Number(e.target.value) || 0 })
                }
                mono
              />
              <p className={styles.helper}>{t('settings:cloudSync.interval.helper')}</p>
            </div>

            {/* 启用 / 自动定时 Toggle，复用同步与存储 Card 的视觉风格 */}
            <div className={styles.toggleList}>
              <button
                type="button"
                className={styles.toggleRow}
                onClick={() => patchCloudSyncForm({ enabled: !cloudSyncForm.enabled })}
                role="switch"
                aria-checked={cloudSyncForm.enabled}
                aria-label={t('settings:cloudSync.enabled.label')}
              >
                <div className={styles.toggleText}>
                  <span className={styles.toggleLabel}>
                    {t('settings:cloudSync.enabled.label')}
                  </span>
                  <span className={styles.toggleHelper}>
                    {t('settings:cloudSync.enabled.helper')}
                  </span>
                </div>
                <span className={styles.toggleState}>
                  {cloudSyncForm.enabled ? (
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

              <button
                type="button"
                className={styles.toggleRow}
                onClick={() => patchCloudSyncForm({ auto: !cloudSyncForm.auto })}
                role="switch"
                aria-checked={cloudSyncForm.auto}
                aria-label={t('settings:cloudSync.auto.label')}
              >
                <div className={styles.toggleText}>
                  <span className={styles.toggleLabel}>
                    {t('settings:cloudSync.auto.label')}
                  </span>
                  <span className={styles.toggleHelper}>
                    {t('settings:cloudSync.auto.helper')}
                  </span>
                </div>
                <span className={styles.toggleState}>
                  {cloudSyncForm.auto ? (
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
            </div>

            {/* 当前已应用配置快照（与表单待编辑值区分） */}
            {cloudSync ? (
              <div className={styles.metaRow}>
                <span className={styles.metaKey}>{t('settings:cloudSync.appliedConfig')}</span>
                <span className={styles.metaValue}>
                  {cloudSync.enabled ? t('settings:sync.enabled') : t('settings:sync.disabled')}
                  {' · '}
                  {cloudSync.repoUrl || '—'}
                  {cloudSync.branch ? ` · ${cloudSync.branch}` : ''}
                </span>
              </div>
            ) : null}

            {/* 操作按钮组 */}
            <div className={styles.aboutActions}>
              <Button
                variant="secondary"
                size="md"
                icon={<SyncIcon />}
                onClick={handleTestCloudSync}
                disabled={testing}
              >
                {testing ? t('settings:cloudSync.testing') : t('settings:cloudSync.testConnection')}
              </Button>
              <Button
                variant="secondary"
                size="md"
                onClick={handleApplyCloudSync}
                disabled={applying}
              >
                {applying ? t('settings:cloudSync.applying') : t('settings:cloudSync.apply')}
              </Button>
              <Button
                variant="primary"
                size="md"
                icon={<SyncIcon />}
                onClick={handleSyncNow}
                disabled={syncing}
              >
                {syncing ? t('settings:cloudSync.syncing') : t('settings:cloudSync.syncNow')}
              </Button>
            </div>

            {/* 测试结果 */}
            {testResult ? (
              <span className={`${styles.aboutHint} ${testResult.ok ? '' : styles.dangerText}`}>
                <InfoIcon size={14} />
                <span>
                  {testResult.ok
                    ? t('settings:cloudSync.testOk', {
                        gitVersion: testResult.gitVersion ?? '—',
                        branch: testResult.defaultBranch ?? '—',
                      })
                    : t('settings:cloudSync.testFailed', {
                        error: testResult.error ?? '',
                      })}
                </span>
              </span>
            ) : null}

            {/* 上次同步结果 */}
            {syncResult ? (
              <div className={styles.metaRow}>
                <span className={styles.metaKey}>{t('settings:cloudSync.lastSync')}</span>
                <span className={`${styles.metaValue} ${syncResult.ok ? '' : styles.dangerText}`}>
                  {syncResult.ok
                    ? t('settings:cloudSync.syncSuccess', {
                        time: formatIsoTime(syncResult.syncedAt),
                        pulled: syncResult.pulled,
                        pushed: syncResult.pushed,
                      })
                    : t('settings:cloudSync.syncFailed', {
                        time: formatIsoTime(syncResult.syncedAt),
                        note: syncResult.note,
                      })}
                </span>
              </div>
            ) : null}

            {/* 应用配置 / 同步失败错误提示 */}
            {cloudSyncError ? (
              <span className={styles.updateError}>{cloudSyncError}</span>
            ) : null}
          </Card.Body>
        </Card>
          </div>
        ) : null}

        {activeTab === 'ai' ? (
          <div
            id="settings-panel-ai"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-ai"
          >
        {/* Card: GitHub Trending / Claude 解说 */}
        <Card variant="flat" padding="md">
          <Card.Header>
            <h2 className={styles.sectionTitle}>{t('settings:githubTrending.title')}</h2>
          </Card.Header>
          <Card.Body padding="md">
            <p className={styles.helper}>{t('settings:githubTrending.subtitle')}</p>

            <div className={styles.toggleList}>
              <button
                type="button"
                className={styles.toggleRow}
                onClick={() =>
                  patchGithubTrendingForm({ aiEnabled: !githubTrendingForm.aiEnabled })
                }
                role="switch"
                aria-checked={githubTrendingForm.aiEnabled}
                aria-label={t('settings:githubTrending.aiEnabled.label')}
              >
                <div className={styles.toggleText}>
                  <span className={styles.toggleLabel}>
                    {t('settings:githubTrending.aiEnabled.label')}
                  </span>
                  <span className={styles.toggleHelper}>
                    {t('settings:githubTrending.aiEnabled.helper')}
                  </span>
                </div>
                <span className={styles.toggleState}>
                  {githubTrendingForm.aiEnabled ? (
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
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-github-claude-path">
                {t('settings:githubTrending.claudeCliPath.label')}
              </label>
              <Input
                id="settings-github-claude-path"
                type="text"
                value={githubTrendingForm.claudeCliPath}
                onChange={(e) => patchGithubTrendingForm({ claudeCliPath: e.target.value })}
                mono
              />
              <p className={styles.helper}>{t('settings:githubTrending.claudeCliPath.helper')}</p>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-github-claude-model">
                {t('settings:githubTrending.claudeModel.label')}
              </label>
              <Input
                id="settings-github-claude-model"
                type="text"
                value={githubTrendingForm.claudeModel}
                onChange={(e) => patchGithubTrendingForm({ claudeModel: e.target.value })}
                mono
              />
              <p className={styles.helper}>{t('settings:githubTrending.claudeModel.helper')}</p>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="settings-github-cache-ttl">
                {t('settings:githubTrending.cacheTtlHours.label')}
              </label>
              <Input
                id="settings-github-cache-ttl"
                type="number"
                value={githubTrendingForm.cacheTtlHours}
                onChange={(e) =>
                  patchGithubTrendingForm({ cacheTtlHours: Number(e.target.value) || 24 })
                }
                min={1}
                max={168}
                mono
              />
              <p className={styles.helper}>{t('settings:githubTrending.cacheTtlHours.helper')}</p>
            </div>

            {githubTrendingConfig ? (
              <div className={styles.metaRow}>
                <span className={styles.metaKey}>
                  {t('settings:githubTrending.appliedConfig')}
                </span>
                <span className={styles.metaValue}>
                  {githubTrendingConfig.aiEnabled
                    ? t('settings:sync.enabled')
                    : t('settings:sync.disabled')}
                  {' · '}
                  {githubTrendingConfig.claudeCliPath || 'claude'}
                  {' · '}
                  {githubTrendingConfig.claudeModel || 'sonnet'}
                </span>
              </div>
            ) : null}

            <div className={styles.aboutActions}>
              <Button
                variant="secondary"
                size="md"
                icon={<InfoIcon />}
                onClick={handleTestClaudeCli}
                disabled={testingClaudeCli}
              >
                {testingClaudeCli
                  ? t('settings:githubTrending.testing')
                  : t('settings:githubTrending.testCli')}
              </Button>
              <Button
                variant="primary"
                size="md"
                onClick={handleApplyGithubTrending}
                disabled={applyingGithubTrending}
              >
                {applyingGithubTrending
                  ? t('settings:githubTrending.applying')
                  : t('settings:githubTrending.apply')}
              </Button>
            </div>

            {claudeCliTest ? (
              <span className={`${styles.aboutHint} ${claudeCliTest.ok ? '' : styles.dangerText}`}>
                <InfoIcon size={14} />
                <span>
                  {claudeCliTest.ok
                    ? t('settings:githubTrending.testOk', {
                        version: claudeCliTest.version ?? '—',
                      })
                    : t('settings:githubTrending.testFailed', {
                        error: claudeCliTest.error ?? '',
                      })}
                </span>
              </span>
            ) : null}

            {githubTrendingError ? (
              <span className={styles.updateError}>{githubTrendingError}</span>
            ) : null}
          </Card.Body>
        </Card>
          </div>
        ) : null}

        {activeTab === 'about' ? (
          <div
            id="settings-panel-about"
            className={styles.tabPanel}
            role="tabpanel"
            aria-labelledby="settings-tab-about"
          >

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
                      {t('settings:about.downloadUpdate', { size: formatSize(updateResult.size ?? 0) })}
                    </Button>
                  </div>
                ) : (
                  <span className={styles.aboutHint}>{t('settings:about.noAsset')}</span>
                )}
              </div>
            ) : null}
          </Card.Body>
        </Card>
          </div>
        ) : null}
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
