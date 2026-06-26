/**
 * WorkbenchFileWorkspace 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 文件查看/编辑能力需要一个独立的 tab 工作区容器，页面层只负责管理打开文件状态和回调，
 *   容器负责统一展示 tab、文件元信息、保存/格式化动作和不同文件类型的预览/编辑入口。
 *
 * Code Logic（这个组件做什么）:
 *   接收已打开文件 tabs 和 activeTabId，按 active tab 的 detectedType 分发到 Markdown、HTML、图片、
 *   CSV、SQLite 或代码编辑组件；所有内容修改、模式切换、保存、格式化和表选择都通过 props 回调上抛。
 */

import { useCallback, useMemo } from 'react';
import type { KeyboardEvent, MouseEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { WorkbenchWorkspaceNav } from '@/components/layout';
import { Button } from '@/components/primitives';
import { CheckIcon, RefreshIcon, TerminalIcon, XIcon } from '@/lib/icons';
import type { WorkbenchFileMode, WorkbenchOpenFile } from '@/lib/types';
import { WorkbenchCodeEditor } from '../WorkbenchCodeEditor';
import { WorkbenchCsvPreview } from '../WorkbenchCsvPreview';
import { WorkbenchHtmlPreview } from '../WorkbenchHtmlPreview';
import type { WorkbenchHtmlMode } from '../WorkbenchHtmlPreview';
import type { WorkbenchHtmlAssetLoader } from '../WorkbenchHtmlPreview/htmlAssets';
import { WorkbenchImagePreview } from '../WorkbenchImagePreview';
import { WorkbenchMarkdownEditor } from '../WorkbenchMarkdownEditor';
import type { WorkbenchMarkdownMode } from '../WorkbenchMarkdownEditor';
import { WorkbenchSqlitePreview } from '../WorkbenchSqlitePreview';
import styles from './WorkbenchFileWorkspace.module.css';

export interface WorkbenchOpenFileTab {
  id: string;
  path: string;
  name: string;
  opened: WorkbenchOpenFile;
  content: string;
  dirty: boolean;
  mode: WorkbenchFileMode;
}

export interface WorkbenchFileWorkspaceProps {
  tabs: WorkbenchOpenFileTab[];
  activeTabId: string | null;
  saving: boolean;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onReturnToTerminal: () => void;
  onContentChange: (id: string, value: string) => void;
  onModeChange: (id: string, mode: WorkbenchOpenFileTab['mode']) => void;
  loadHtmlAsset?: WorkbenchHtmlAssetLoader;
  onSave: (id: string) => void;
  onFormat: (id: string) => void;
  onSelectSqliteTable: (id: string, table: string) => void;
}

const MARKDOWN_MODES = new Set<WorkbenchMarkdownMode>(['wysiwyg', 'source', 'split']);
const HTML_MODES = new Set<WorkbenchHtmlMode>(['wysiwyg', 'source', 'split']);

const FILENAME_LANGUAGE_HINTS: Record<string, string> = {
  bashrc: 'shell',
  dockerfile: 'dockerfile',
  justfile: 'shell',
  makefile: 'makefile',
  npmrc: 'text',
  zshrc: 'shell',
};

/**
 * 生成 tab 与 panel 的稳定 ARIA id
 *
 * Business Logic（为什么需要这个函数）:
 *   文件 tab 的业务 id 可能来自路径、数据库或其他外部来源，包含空格、斜杠等特殊字符时仍需要生成可安全引用的
 *   DOM id，确保 tab 与 tabpanel 的可访问性关系稳定。
 *
 * Code Logic（这个函数做什么）:
 *   将 tabId 清洗为小写字母/数字/连字符/下划线片段，并追加同源字符串计算出的短 hash 降低清洗后碰撞风险；
 *   返回 tab button id 和 panel id，供 aria-controls / aria-labelledby 成对使用。
 */
function createTabAriaIds(tabId: string): { tabButtonId: string; tabPanelId: string } {
  let hash = 0;

  for (let index = 0; index < tabId.length; index += 1) {
    hash = (hash * 31 + tabId.charCodeAt(index)) >>> 0;
  }

  const sanitized = tabId
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '');
  const safePart = `${sanitized || 'tab'}-${hash.toString(36)}`;

  return {
    tabButtonId: `workbench-file-tab-${safePart}`,
    tabPanelId: `workbench-file-panel-${safePart}`,
  };
}

/**
 * 聚焦指定文件 tab 按钮
 *
 * Business Logic（为什么需要这个函数）:
 *   键盘用户在 tablist 内用方向键切换文件时，需要焦点跟随切换后的 tab，否则 roving focus 状态与视觉选中不一致。
 *
 * Code Logic（这个函数做什么）:
 *   复用 createTabAriaIds 生成安全 button id，在下一帧查找 DOM 节点并调用 focus；非浏览器环境直接跳过。
 */
function focusTabButton(tabId: string): void {
  if (typeof window === 'undefined') {
    return;
  }

  const { tabButtonId } = createTabAriaIds(tabId);
  const focusButton = () => {
    document.getElementById(tabButtonId)?.focus();
  };

  if (typeof window.requestAnimationFrame === 'function') {
    window.requestAnimationFrame(focusButton);
    return;
  }

  window.setTimeout(focusButton, 0);
}

/**
 * 约束 Markdown 编辑模式
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件 tab 的通用 mode 包含 viewer/editor，但 Markdown 编辑器只接受 wysiwyg/source/split。
 *
 * Code Logic（这个函数做什么）:
 *   检查当前 tab mode 是否属于 Markdown 编辑器支持集合；不支持时返回 wysiwyg 作为安全默认值。
 */
function coerceMarkdownMode(mode: WorkbenchOpenFileTab['mode']): WorkbenchMarkdownMode {
  return MARKDOWN_MODES.has(mode as WorkbenchMarkdownMode) ? (mode as WorkbenchMarkdownMode) : 'wysiwyg';
}

/**
 * 约束 HTML 预览模式
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench 通用文件 mode 包含 viewer/editor，但 HTML 预览只支持源码、预览和分屏三种模式。
 *
 * Code Logic（这个函数做什么）:
 *   调用 HTML 组件导出的 type guard；不支持时回退 wysiwyg，HTML 语境下该值表示渲染预览。
 */
function coerceHtmlMode(mode: WorkbenchOpenFileTab['mode']): WorkbenchHtmlMode {
  return HTML_MODES.has(mode as WorkbenchHtmlMode) ? (mode as WorkbenchHtmlMode) : 'wysiwyg';
}

/**
 * 推断代码编辑器语言
 *
 * Business Logic（为什么需要这个函数）:
 *   代码编辑器需要尽量使用具体语言/扩展名启用语法高亮；只传 detectedType=code 会丢失 TSX、
 *   JSON、TOML 等可识别文件的高亮信息。
 *
 * Code Logic（这个函数做什么）:
 *   优先从后端 metadata.name 的文件名和扩展名推断语言；对无扩展常见文件名做小范围映射，
 *   最后回退到 detectedType，让未知文件仍能以纯文本打开。
 */
function deriveEditorLanguage(opened: WorkbenchOpenFile): string {
  const normalizedName = opened.metadata.name.trim().toLowerCase();
  const leafName = normalizedName.split(/[\\/]/).pop() ?? normalizedName;
  const hiddenName = leafName.startsWith('.') ? leafName.slice(1) : leafName;
  const knownLanguage = FILENAME_LANGUAGE_HINTS[hiddenName] ?? FILENAME_LANGUAGE_HINTS[leafName];

  if (knownLanguage) {
    return knownLanguage;
  }

  if (leafName.startsWith('dockerfile')) {
    return 'dockerfile';
  }

  const extensionStart = leafName.lastIndexOf('.');

  if (extensionStart > 0 && extensionStart < leafName.length - 1) {
    return leafName.slice(extensionStart + 1);
  }

  return opened.detectedType;
}

/**
 * 渲染 Workbench 文件 tab 工作区
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在 Workbench 中打开多个文件后，需要在不离开终端上下文的前提下切换文件、关闭文件、
 *   修改可编辑内容并回到终端继续操作。
 *
 * Code Logic（这个组件做什么）:
 *   选择 activeTabId 对应 tab（缺失时回退第一个 tab），渲染横向 tab header、notice 和内容区；
 *   内容区按文件类型分发到既有 preview/editor 组件，并通过稳定回调携带当前 tab id 上抛事件。
 */
export function WorkbenchFileWorkspace(props: WorkbenchFileWorkspaceProps): ReactElement {
  const {
    tabs,
    activeTabId,
    saving,
    onActivate,
    onClose,
    onReturnToTerminal,
    onContentChange,
    onModeChange,
    loadHtmlAsset,
    onSave,
    onFormat,
    onSelectSqliteTable,
  } = props;
  const { t } = useTranslation(['workbench']);

  const activeTab = useMemo(() => {
    if (tabs.length === 0) {
      return null;
    }

    return tabs.find((tab) => tab.id === activeTabId) ?? tabs[0];
  }, [activeTabId, tabs]);
  const activeTabStableId = activeTab?.id ?? null;
  const activeAriaIds = useMemo(
    () => (activeTab ? createTabAriaIds(activeTab.id) : null),
    [activeTab],
  );

  const handleTabActivate = useCallback(
    (event: MouseEvent<HTMLButtonElement>) => {
      const nextId = event.currentTarget.dataset.tabId;

      if (nextId) {
        onActivate(nextId);
      }
    },
    [onActivate],
  );

  const handleTabClose = useCallback(
    (event: MouseEvent<HTMLButtonElement>) => {
      event.stopPropagation();
      const closeId = event.currentTarget.dataset.tabId;

      if (closeId) {
        onClose(closeId);
      }
    },
    [onClose],
  );

  const handleTabKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>) => {
      const currentId = event.currentTarget.dataset.tabId;

      if (!currentId || tabs.length === 0) {
        return;
      }

      const currentIndex = tabs.findIndex((tab) => tab.id === currentId);

      if (currentIndex < 0) {
        return;
      }

      let nextIndex: number;

      switch (event.key) {
        case 'ArrowRight':
        case 'ArrowDown':
          nextIndex = (currentIndex + 1) % tabs.length;
          break;
        case 'ArrowLeft':
        case 'ArrowUp':
          nextIndex = (currentIndex - 1 + tabs.length) % tabs.length;
          break;
        case 'Home':
          nextIndex = 0;
          break;
        case 'End':
          nextIndex = tabs.length - 1;
          break;
        default:
          return;
      }

      event.preventDefault();
      const nextTab = tabs[nextIndex];
      onActivate(nextTab.id);
      focusTabButton(nextTab.id);
    },
    [onActivate, tabs],
  );

  const handleContentChange = useCallback(
    (nextValue: string) => {
      if (activeTabStableId) {
        onContentChange(activeTabStableId, nextValue);
      }
    },
    [activeTabStableId, onContentChange],
  );

  const handleMarkdownModeChange = useCallback(
    (nextMode: WorkbenchMarkdownMode) => {
      if (activeTabStableId) {
        onModeChange(activeTabStableId, nextMode);
      }
    },
    [activeTabStableId, onModeChange],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   HTML 文件复用 Workbench 的 source/wysiwyg/split 模式状态，切换预览方式时需要写回当前 tab。
   *
   * Code Logic（这个函数做什么）:
   *   捕获当前 active tab id，将 HTML 组件的三模式值透传给页面层 onModeChange。
   */
  const handleHtmlModeChange = useCallback(
    (nextMode: WorkbenchHtmlMode) => {
      if (activeTabStableId) {
        onModeChange(activeTabStableId, nextMode);
      }
    },
    [activeTabStableId, onModeChange],
  );

  const handleSave = useCallback(() => {
    if (activeTabStableId) {
      onSave(activeTabStableId);
    }
  }, [activeTabStableId, onSave]);

  const handleFormat = useCallback(() => {
    if (activeTabStableId) {
      onFormat(activeTabStableId);
    }
  }, [activeTabStableId, onFormat]);

  const handleSelectSqliteTable = useCallback(
    (table: string) => {
      if (activeTabStableId) {
        onSelectSqliteTable(activeTabStableId, table);
      }
    },
    [activeTabStableId, onSelectSqliteTable],
  );

  let fileContent: ReactElement | null = null;

  if (activeTab) {
    const { opened } = activeTab;

    switch (opened.detectedType) {
      case 'markdown':
        fileContent = (
          <WorkbenchMarkdownEditor
            value={activeTab.content}
            mode={coerceMarkdownMode(activeTab.mode)}
            readOnly={saving}
            onModeChange={handleMarkdownModeChange}
            onChange={handleContentChange}
          />
        );
        break;
      case 'html':
        fileContent = (
          <WorkbenchHtmlPreview
            value={activeTab.content}
            documentPath={activeTab.path}
            mode={coerceHtmlMode(activeTab.mode)}
            readOnly={saving}
            loadAsset={loadHtmlAsset}
            onModeChange={handleHtmlModeChange}
            onChange={handleContentChange}
          />
        );
        break;
      case 'image':
        fileContent = opened.image ? (
          <WorkbenchImagePreview preview={opened.image} name={activeTab.name} />
        ) : (
          <div className={styles.unavailable}>{t('workbench:fileWorkspace.previewUnavailable')}</div>
        );
        break;
      case 'csv':
        fileContent = opened.csv ? (
          <WorkbenchCsvPreview preview={opened.csv} />
        ) : (
          <div className={styles.unavailable}>{t('workbench:fileWorkspace.previewUnavailable')}</div>
        );
        break;
      case 'sqlite':
        fileContent = opened.sqlite ? (
          <WorkbenchSqlitePreview
            preview={opened.sqlite}
            onSelectTable={handleSelectSqliteTable}
          />
        ) : (
          <div className={styles.unavailable}>{t('workbench:fileWorkspace.previewUnavailable')}</div>
        );
        break;
      default:
        fileContent = (
          <WorkbenchCodeEditor
            value={activeTab.content}
            language={deriveEditorLanguage(opened)}
            readOnly={!opened.capabilities.canEdit || saving}
            onChange={handleContentChange}
          />
        );
        break;
    }
  }

  return (
    <section className={styles.fileWorkspace}>
      <WorkbenchWorkspaceNav
        ariaLabel={t('workbench:fileWorkspace.tabs')}
        actionsAriaLabel={t('workbench:fileWorkspace.actions')}
        tabs={
          <div className={styles.fileTabs} role="tablist" aria-label={t('workbench:fileWorkspace.tabs')}>
            {tabs.map((tab) => {
              const active = tab.id === activeTab?.id;
              const tabAriaIds = createTabAriaIds(tab.id);

              return (
                <div key={tab.id} className={styles.fileTab} data-active={active}>
                  <button
                    id={tabAriaIds.tabButtonId}
                    type="button"
                    role="tab"
                    className={styles.tabButton}
                    data-tab-id={tab.id}
                    aria-controls={active ? tabAriaIds.tabPanelId : undefined}
                    aria-selected={active}
                    tabIndex={active ? 0 : -1}
                    title={tab.path}
                    onClick={handleTabActivate}
                    onKeyDown={handleTabKeyDown}
                  >
                    <span className={styles.tabName}>{tab.path}</span>
                    {tab.dirty ? (
                      <span
                        className={styles.dirtyMarker}
                        role="img"
                        aria-label={t('workbench:fileWorkspace.dirty')}
                        title={t('workbench:fileWorkspace.dirty')}
                      />
                    ) : null}
                  </button>
                  <button
                    type="button"
                    className={styles.closeTabButton}
                    data-tab-id={tab.id}
                    aria-label={t('workbench:fileWorkspace.closeTab', { name: tab.name })}
                    onClick={handleTabClose}
                  >
                    <XIcon size={14} aria-hidden="true" />
                  </button>
                </div>
              );
            })}
          </div>
        }
        actions={
          activeTab ? (
            <div className={styles.toolbarActions}>
              {activeTab.opened.capabilities.canFormat ? (
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<RefreshIcon aria-hidden="true" />}
                  disabled={saving}
                  onClick={handleFormat}
                >
                  {t('workbench:fileWorkspace.format')}
                </Button>
              ) : null}
              {activeTab.opened.capabilities.canEdit ? (
                <Button
                  variant="primary"
                  size="sm"
                  icon={<CheckIcon aria-hidden="true" />}
                  loading={saving}
                  disabled={!activeTab.dirty || saving}
                  onClick={handleSave}
                >
                  {t('workbench:fileWorkspace.save')}
                </Button>
              ) : null}
              <Button
                variant="secondary"
                size="sm"
                icon={<TerminalIcon aria-hidden="true" />}
                onClick={onReturnToTerminal}
              >
                {t('workbench:fileWorkspace.returnTerminal')}
              </Button>
            </div>
          ) : null
        }
      />

      {activeTab ? (
        <div
          id={activeAriaIds?.tabPanelId}
          className={styles.fileBody}
          role="tabpanel"
          aria-labelledby={activeAriaIds?.tabButtonId}
        >
          <div className={styles.fileContentStack}>
            {activeTab.opened.notice ? (
              <div className={styles.fileNotice} role="status">
                {activeTab.opened.notice}
              </div>
            ) : null}

            <div className={styles.fileContent}>{fileContent}</div>
          </div>
        </div>
      ) : (
        <div className={styles.emptyState}>
          <p>{t('workbench:fileWorkspace.empty')}</p>
          <Button
            variant="secondary"
            size="sm"
            icon={<TerminalIcon aria-hidden="true" />}
            onClick={onReturnToTerminal}
          >
            {t('workbench:fileWorkspace.returnTerminal')}
          </Button>
        </div>
      )}
    </section>
  );
}
