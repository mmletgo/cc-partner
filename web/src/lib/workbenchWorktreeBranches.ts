export const WORKTREE_BRANCH_PREFIXES = [
  'feature',
  'fix',
  'chore',
  'docs',
  'refactor',
  'test',
  'hotfix',
] as const;

export type WorktreeBranchPrefix = (typeof WORKTREE_BRANCH_PREFIXES)[number];

export const DEFAULT_WORKTREE_BRANCH_PREFIX: WorktreeBranchPrefix = 'feature';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端和移动端都会创建 worktree，分支名输入需要共享同一套空值处理规则。
 *
 * Code Logic（这个函数做什么）:
 *   清理输入两侧空白；结果为空时返回 null，否则返回可提交给后端的分支名。
 */
export function normalizeWorktreeBranchName(input: string): string | null {
  const branchName = input.trim();
  return branchName.length > 0 ? branchName : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新建 worktree 时分支类型应由固定前缀选择，用户只负责命名具体任务后缀。
 *
 * Code Logic（这个函数做什么）:
 *   复用后缀清理逻辑；有效后缀返回 `prefix/suffix`，空后缀返回 null。
 */
export function composeWorktreeBranchName(
  prefix: WorktreeBranchPrefix,
  suffix: string,
): string | null {
  const branchSuffix = normalizeWorktreeBranchName(suffix);
  return branchSuffix ? `${prefix}/${branchSuffix}` : null;
}
