/**
 * WorkspaceSnapshotDialog — 命名 snapshot 二级入口。
 *
 * Business Logic（为什么需要）:
 *   用户可保存/应用/删除当前结构 metadata；不是可执行命令配方，无命令编辑器。
 *
 * Code Logic（做什么）:
 *   复用 Dialog/Button；props-only，不 import @/api/*。
 */

import { useId, useState } from 'react';
import type { ReactElement } from 'react';
import { Dialog } from '@/components/primitives/Dialog';
import { Button } from '@/components/primitives/Button';
import { Input } from '@/components/primitives/Input';
import type { WorkspaceLayout } from '../workspaceLayout';
import styles from '../Workbench.module.css';

export interface WorkspaceSnapshotDialogProps {
  open: boolean;
  onClose: () => void;
  snapshots: WorkspaceLayout[];
  onSaveCurrent: (name: string) => Promise<void> | void;
  onApply: (layoutId: string) => Promise<void> | void;
  onDelete: (layoutId: string) => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 二级动作管理命名结构现场。
 *
 * Code Logic（这个函数做什么）:
 *   列表 + 保存名称 + 应用/删除确认；无 command/provider 字段。
 */
export function WorkspaceSnapshotDialog(
  props: WorkspaceSnapshotDialogProps,
): ReactElement {
  const { open, onClose, snapshots, onSaveCurrent, onApply, onDelete } = props;
  const titleId = useId();
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   保存当前结构为命名 snapshot。
   *
   * Code Logic（这个函数做什么）:
   *   trim name 后调用 onSaveCurrent。
   */
  async function handleSave(): Promise<void> {
    const trimmed = name.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      await onSaveCurrent(trimmed);
      setName('');
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} titleId={titleId} onClose={onClose}>
      <div className={styles.snapshotDialog} data-testid="workspace-snapshot-dialog">
        <h2 id={titleId}>工作现场快照</h2>
        <p className={styles.snapshotHint}>
          仅保存项目/工作区/终端选择与视图结构，不包含终端输出、命令或文件内容。
        </p>
        <div className={styles.snapshotSaveRow}>
          <Input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="快照名称"
            aria-label="快照名称"
          />
          <Button
            variant="primary"
            size="sm"
            type="button"
            loading={busy}
            onClick={() => void handleSave()}
          >
            保存当前结构
          </Button>
        </div>
        <ul className={styles.snapshotList}>
          {snapshots.map((item) => (
            <li key={item.id} className={styles.snapshotItem}>
              <span>{item.name ?? item.slotKey}</span>
              <div className={styles.snapshotItemActions}>
                <Button
                  variant="secondary"
                  size="sm"
                  type="button"
                  onClick={() => void onApply(item.id)}
                >
                  应用
                </Button>
                {pendingDeleteId === item.id ? (
                  <Button
                    variant="danger"
                    size="sm"
                    type="button"
                    onClick={() => {
                      void onDelete(item.id);
                      setPendingDeleteId(null);
                    }}
                  >
                    确认删除
                  </Button>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    type="button"
                    onClick={() => setPendingDeleteId(item.id)}
                  >
                    删除
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
        {/* 明确不提供命令编辑器 */}
      </div>
    </Dialog>
  );
}
