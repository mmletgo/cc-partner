/**
 * Workbench 文件检查器叶子视图 —— 项目目录树 + path info + create/rename/delete/copy 操作。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspector "files" tab 渲染（FileTree + path info + 操作按钮）
 *   抽到独立叶子组件。组件只接收 controller 派生的渲染数据与回调，不持有自己的状态，也不导入 Git 域。
 *
 * Code Logic（这个组件做什么）:
 *   - 内部封装 FileTree / FileTreeNode 递归渲染（随文件检查器一起从页面迁出）；
 *   - 渲染刷新/创建文件/创建文件夹/重命名/删除/复制等按钮，并展示当前选中 path info；
 *   - 暴露 WorkbenchFileInspectorProps 类型，所有数据均来自 useWorkbenchFileController。
 */
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Input } from '@/components/primitives';
import {
  ChevronRightIcon,
  CopyIcon,
  EditIcon,
  FileIcon,
  FolderIcon,
  SyncIcon,
  TrashIcon,
} from '@/lib/icons';
import type { WorkbenchFileNode, WorkbenchPathInfo } from '@/lib/types';
import styles from './Workbench.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   文件操作默认作用在当前选中文件夹；若选中的是文件，则作用在它的父目录。
 *
 * Code Logic（这个函数做什么）:
 *   从相对路径中取最后一个 `/` 之前的部分；根级文件返回空字符串。
 */
function parentPathOf(path: string): string {
  const index = path.lastIndexOf('/');
  return index >= 0 ? path.slice(0, index) : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件树和状态栏需要展示简短路径名；根目录没有 basename 时显示根符号。
 *
 * Code Logic（这个函数做什么）:
 *   取相对路径最后一段；空路径返回 `/`。
 */
function basename(path: string, rootLabel: string): string {
  if (!path) return rootLabel;
  const parts = path.split('/').filter(Boolean);
  return parts[parts.length - 1] ?? rootLabel;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检查器要展示文件大小，直接展示字节数不利于扫描。
 *
 * Code Logic（这个函数做什么）:
 *   把字节数格式化为 B/KB/MB/GB；目录或未知大小返回占位符。
 */
function formatSize(size: number | null, emptyValue: string): string {
  if (size === null) return emptyValue;
  if (size < 1024) return `${size} B`;
  const kb = size / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   最近打开时间、文件修改时间需要展示成用户本地可读格式。
 *
 * Code Logic（这个函数做什么）:
 *   使用浏览器本地化短日期时间；解析失败时回退原始字符串。
 */
function formatDateTime(value: string | null, emptyValue: string): string {
  if (!value) return emptyValue;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

interface FileTreeProps {
  nodes: WorkbenchFileNode[];
  childrenByPath: Record<string, WorkbenchFileNode[]>;
  expandedPaths: Set<string>;
  selectedPath: string | null;
  loadingPath: string | null;
  onToggle: (node: WorkbenchFileNode) => void;
  onSelect: (node: WorkbenchFileNode) => void;
}

interface FileTreeNodeProps extends FileTreeProps {
  node: WorkbenchFileNode;
  depth: number;
}

interface NestedFileTreeProps extends FileTreeProps {
  depth?: number;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   文件树需要懒加载多级目录，同时保持目录展开、选中态和 loading 态一致。
 *
 * Code Logic（这个组件做什么）:
 *   递归渲染 WorkbenchFileNode；目录按钮负责展开/收起，文件点击只更新选中路径。
 */
function FileTreeNode(props: FileTreeNodeProps) {
  const { node, depth, childrenByPath, expandedPaths, selectedPath, loadingPath, onToggle, onSelect } =
    props;
  const isDir = node.kind === 'dir';
  const expanded = expandedPaths.has(node.path);
  const selected = selectedPath === node.path;
  const children = childrenByPath[node.path] ?? [];
  const paddingStyle = { paddingLeft: 8 + depth * 14 } as CSSProperties;

  return (
    <div className={styles.treeBranch}>
      <button
        type="button"
        className={styles.treeRow}
        data-selected={selected || undefined}
        style={paddingStyle}
        onClick={() => {
          onSelect(node);
          if (isDir) onToggle(node);
        }}
      >
        <span className={styles.treeChevron} data-expanded={expanded || undefined}>
          {isDir ? <ChevronRightIcon size={14} /> : null}
        </span>
        <span className={styles.treeIcon}>
          {isDir ? <FolderIcon size={14} /> : <FileIcon size={14} />}
        </span>
        <span className={styles.treeName}>{node.name}</span>
        {loadingPath === node.path ? <span className={styles.treeLoading}>…</span> : null}
      </button>
      {isDir && expanded ? (
        <FileTree
          nodes={children}
          childrenByPath={childrenByPath}
          expandedPaths={expandedPaths}
          selectedPath={selectedPath}
          loadingPath={loadingPath}
          onToggle={onToggle}
          onSelect={onSelect}
          depth={depth + 1}
        />
      ) : null}
    </div>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   右侧检查器需要展示可交互项目文件夹，支持目录展开与文件选中。
 *
 * Code Logic（这个组件做什么）:
 *   渲染同层节点列表，并把当前递归深度传给 FileTreeNode 控制缩进。
 */
function FileTree(props: NestedFileTreeProps) {
  const { nodes, depth = 0 } = props;
  return (
    <div className={styles.treeList}>
      {nodes.map((node) => (
        <FileTreeNode key={node.path || node.name} {...props} node={node} depth={depth} />
      ))}
    </div>
  );
}

/**
 * 文件检查器叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchFileController 派生；组件本身不持有状态、不导入 Git 域。
 * remoteWriteDisabled / activeProjectId 来自 Workbench.tsx 跨域共享/路由 context，由页面透传。
 */
export interface WorkbenchFileInspectorProps {
  activeProjectId: string | null;
  remoteWriteDisabled: boolean;
  rootNodes: WorkbenchFileNode[];
  childrenByPath: Record<string, WorkbenchFileNode[]>;
  expandedPaths: Set<string>;
  selectedPath: string | null;
  selectedInfo: WorkbenchPathInfo | null;
  fileLoadingPath: string | null;
  fileError: string | null;
  fileNotice: string | null;
  newEntryName: string;
  renameName: string;
  setNewEntryName: (next: string) => void;
  setRenameName: (next: string) => void;
  loadDir: (path: string) => Promise<void>;
  handleToggleNode: (node: WorkbenchFileNode) => void;
  handleSelectNode: (node: WorkbenchFileNode) => void;
  handleCreateEntry: (kind: 'file' | 'dir') => Promise<void>;
  handleRenamePath: () => Promise<void>;
  handleDeletePath: () => Promise<void>;
  handleCopySelectedPath: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 检查器的 "files" tab 需要一个独立的叶子视图，把目录树、path info、create/rename/delete/copy
 *   操作集中渲染。该组件由 WorkbenchInspector 在 files tab 时挂载；接收 controller 派生的 props。
 *
 * Code Logic（这个组件做什么）:
 *   渲染刷新/新建/重命名/删除/复制按钮 + FileTree + path info grid；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchFileInspector(props: WorkbenchFileInspectorProps) {
  const { t } = useTranslation(['workbench']);
  const {
    activeProjectId,
    remoteWriteDisabled,
    rootNodes,
    childrenByPath,
    expandedPaths,
    selectedPath,
    selectedInfo,
    fileLoadingPath,
    fileError,
    fileNotice,
    newEntryName,
    renameName,
    setNewEntryName,
    setRenameName,
    loadDir,
    handleToggleNode,
    handleSelectNode,
    handleCreateEntry,
    handleRenamePath,
    handleDeletePath,
    handleCopySelectedPath,
  } = props;

  const emptyValue = t('workbench:emptyValue');
  const rootPath = t('workbench:rootPath');
  const selectedDisplayPath = selectedInfo?.path ?? '';
  const selectedParentPath = selectedInfo
    ? selectedInfo.kind === 'dir'
      ? selectedInfo.path
      : parentPathOf(selectedInfo.path)
    : '';
  const selectedKindLabel = selectedInfo
    ? selectedInfo.kind === 'dir'
      ? t('workbench:pathKinds.dir')
      : selectedInfo.kind === 'file'
        ? t('workbench:pathKinds.file')
        : selectedInfo.kind
    : emptyValue;

  return (
    <Card className={styles.filesCard} padding="sm">
      <div className={styles.cardTitleRow}>
        <h3 className={styles.cardTitle}>{t('workbench:filesTitle')}</h3>
        <Button
          variant="icon"
          icon={<SyncIcon />}
          title={t('workbench:refreshFiles')}
          aria-label={t('workbench:refreshFiles')}
          disabled={!activeProjectId}
          onClick={() => void loadDir('')}
        />
      </div>

      {fileError ? <div className={styles.errorBox}>{fileError}</div> : null}
      {fileNotice ? <div className={styles.noticeBox}>{fileNotice}</div> : null}

      <div className={styles.fileActions}>
        <Input
          value={newEntryName}
          onChange={(event) => setNewEntryName(event.target.value)}
          placeholder={t('workbench:newEntryPlaceholder')}
          size="sm"
        />
        <div className={styles.fileActionButtons}>
          <Button
            size="sm"
            variant="secondary"
            icon={<FileIcon />}
            disabled={!activeProjectId || !newEntryName.trim() || remoteWriteDisabled}
            onClick={() => void handleCreateEntry('file')}
          >
            {t('workbench:createFile')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<FolderIcon />}
            disabled={!activeProjectId || !newEntryName.trim() || remoteWriteDisabled}
            onClick={() => void handleCreateEntry('dir')}
          >
            {t('workbench:createFolder')}
          </Button>
        </div>
      </div>

      <div className={styles.treePanel}>
        {!activeProjectId ? (
          <div className={styles.treeEmpty}>{t('workbench:filesNoProject')}</div>
        ) : rootNodes.length === 0 && fileLoadingPath === '' ? (
          <div className={styles.treeEmpty}>{t('workbench:loading')}</div>
        ) : rootNodes.length === 0 ? (
          <div className={styles.treeEmpty}>{t('workbench:filesEmpty')}</div>
        ) : (
          <FileTree
            nodes={rootNodes}
            childrenByPath={childrenByPath}
            expandedPaths={expandedPaths}
            selectedPath={selectedPath}
            loadingPath={fileLoadingPath}
            onToggle={handleToggleNode}
            onSelect={handleSelectNode}
          />
        )}
      </div>

      <div className={styles.pathInfo}>
        <div className={styles.pathInfoHeader}>
          <span className={styles.pathInfoName}>{basename(selectedDisplayPath, rootPath)}</span>
          <span className={styles.pathInfoPath}>{selectedDisplayPath || emptyValue}</span>
        </div>
        <dl className={styles.pathInfoGrid}>
          <div>
            <dt>{t('workbench:pathKind')}</dt>
            <dd>{selectedKindLabel}</dd>
          </div>
          <div>
            <dt>{t('workbench:pathSize')}</dt>
            <dd>{formatSize(selectedInfo?.size ?? null, emptyValue)}</dd>
          </div>
          <div>
            <dt>{t('workbench:pathModified')}</dt>
            <dd>{formatDateTime(selectedInfo?.modifiedAt ?? null, emptyValue)}</dd>
          </div>
          <div>
            <dt>{t('workbench:pathParent')}</dt>
            <dd>{selectedParentPath || rootPath}</dd>
          </div>
        </dl>
        <div className={styles.renameRow}>
          <Input
            value={renameName}
            onChange={(event) => setRenameName(event.target.value)}
            placeholder={t('workbench:renamePlaceholder')}
            size="sm"
            disabled={!selectedInfo || remoteWriteDisabled}
          />
          <Button
            size="sm"
            variant="secondary"
            icon={<CopyIcon />}
            disabled={!selectedInfo}
            onClick={() => void handleCopySelectedPath()}
          >
            {t('workbench:copyRelativePath')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<EditIcon />}
            disabled={!selectedInfo || !renameName.trim() || remoteWriteDisabled}
            onClick={() => void handleRenamePath()}
          >
            {t('workbench:rename')}
          </Button>
          <Button
            size="sm"
            variant="danger"
            icon={<TrashIcon />}
            disabled={!selectedInfo || remoteWriteDisabled}
            onClick={() => void handleDeletePath()}
          >
            {t('workbench:delete')}
          </Button>
        </div>
      </div>
    </Card>
  );
}
