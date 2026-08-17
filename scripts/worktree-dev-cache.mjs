/**
 * worktree-dev-cache.mjs — 多 git worktree 的 sccache 路径归一化 + 闲置 target 回收。
 *
 * Business Logic（为什么需要）:
 *   并行 worktree 不能共用 CARGO_TARGET_DIR（Cargo 整目录锁会串行，且同名 crate
 *   fingerprint 会串味）。sccache 按编译内容缓存 rustc 产物，但 cache key 含绝对路径，
 *   必须把每个 worktree 根列入 SCCACHE_BASEDIRS。闲置树的 target/debug 仍占数 GB，
 *   有 sccache 后可以 cargo clean；正在 cargo/tauri/rustc 的树绝不能删。
 *
 * Code Logic（做什么）:
 *   --print-sccache-basedirs：列出本仓库 git worktree 根（+ 可选额外路径），stdout 一行。
 *   --mode=idle-clean：对「非当前、无编译进程、存在 src-tauri/target」的 worktree 执行
 *   cargo clean。缺 ps 时拒绝清，避免误删。
 *
 * 用法:
 *   node scripts/worktree-dev-cache.mjs --print-sccache-basedirs
 *   node scripts/worktree-dev-cache.mjs --mode=idle-clean [--dry-run]
 *   node scripts/worktree-dev-cache.mjs --self-test
 */

import assert from 'node:assert/strict';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readlinkSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_CACHE_SIZE = '15G';

/**
 * Business Logic:
 *   sccache 文档：Windows 用 `;`，其它系统用 `:`。
 * Code Logic:
 *   按 process.platform 选分隔符。
 */
export function basedirsSeparator(platform = process.platform) {
  return platform === 'win32' ? ';' : ':';
}

/**
 * Business Logic:
 *   `git worktree list --porcelain` 是本仓库链接 checkout 的权威清单。
 * Code Logic:
 *   收集 `worktree <path>`；跳过 bare。
 * @param {string} text
 * @returns {string[]}
 */
export function parseGitWorktreePorcelain(text) {
  const roots = [];
  let current = null;
  let bare = false;
  const flush = () => {
    if (current && !bare) {
      roots.push(current);
    }
    current = null;
    bare = false;
  };
  for (const line of text.split(/\r?\n/)) {
    if (line === '') {
      flush();
      continue;
    }
    if (line.startsWith('worktree ')) {
      flush();
      current = line.slice('worktree '.length);
      continue;
    }
    if (line === 'bare' || line.startsWith('bare ')) {
      bare = true;
    }
  }
  flush();
  return roots;
}

/**
 * Business Logic:
 *   子路径必须落到「最长」worktree 根，避免嵌套树误标到父树。
 * Code Logic:
 *   根按路径长度降序，精确或 `root/` 前缀匹配。
 * @param {string} absPath
 * @param {string[]} roots
 * @returns {string | null}
 */
export function longestMatchingRoot(absPath, roots) {
  const normalized = absPath.replace(/\\/g, '/').replace(/\/+$/, '');
  const sorted = [...roots].sort((a, b) => b.length - a.length);
  for (const root of sorted) {
    const r = root.replace(/\\/g, '/').replace(/\/+$/, '');
    if (normalized === r || normalized.startsWith(`${r}/`)) {
      return root;
    }
  }
  return null;
}

/**
 * Business Logic:
 *   只把「正在编 Rust / 跑 Tauri CLI」的树当忙；编辑器、普通 node 不算，
 *   否则每个 Cursor worktree 都永远清不掉。
 * Code Logic:
 *   cargo/rustc/rustdoc、tauri CLI、macos-dev-cargo-runner、从该树 target 拉起的
 *   backend 视为编译占用。sccache 守护进程本身不算。
 * @param {string} cmd
 * @returns {boolean}
 */
export function isCompileCommand(cmd) {
  if (!cmd) {
    return false;
  }
  if (/(?:^|[\\/\s])(?:cargo|rustc|rustdoc)(?:\.exe)?(?:\s|$)/.test(cmd)) {
    return true;
  }
  if (cmd.includes('macos-dev-cargo-runner')) {
    return true;
  }
  if (cmd.includes('@tauri-apps/cli') || /node_modules[/\\]\.bin[/\\]tauri/.test(cmd)) {
    return true;
  }
  if (cmd.includes('cc-partner-backend') && /[/\\]target[/\\]/.test(cmd)) {
    return true;
  }
  return false;
}

/**
 * Business Logic:
 *   命令行里带 worktree 路径时不必再查 cwd（`--manifest-path` / 真 binary）。
 * Code Logic:
 *   路径后跟分隔符、空白，或恰好结束。
 * @param {string} cmd
 * @param {string} worktree
 * @returns {boolean}
 */
export function commandMentionsWorktree(cmd, worktree) {
  const wt = worktree.replace(/\\/g, '/').replace(/\/+$/, '');
  const n = cmd.replace(/\\/g, '/');
  return n === wt || n.includes(`${wt}/`) || n.includes(`${wt} `) || n.endsWith(wt);
}

/**
 * Business Logic:
 *   当前树要编，永远保留；忙树保留；没 target 的跳过；其余可 cargo clean。
 * Code Logic:
 *   返回 { keep, clean } 两个 worktree 路径数组。
 * @param {{
 *   currentRoot: string,
 *   worktrees: string[],
 *   busyRoots: string[],
 *   hasTarget: (worktree: string) => boolean,
 * }} input
 */
export function classifyIdleWorktrees(input) {
  const current = resolve(input.currentRoot);
  const busy = new Set(input.busyRoots.map((root) => resolve(root)));
  const keep = [];
  const clean = [];
  const seen = new Set();
  for (const raw of input.worktrees) {
    const worktree = resolve(raw);
    if (seen.has(worktree)) {
      continue;
    }
    seen.add(worktree);
    if (worktree === current || busy.has(worktree)) {
      keep.push(worktree);
      continue;
    }
    if (!input.hasTarget(worktree)) {
      keep.push(worktree);
      continue;
    }
    clean.push(worktree);
  }
  return { keep, clean };
}

/**
 * Business Logic:
 *   用户可能已有 SCCACHE_BASEDIRS；追加本仓库 worktree 根，去重。
 * Code Logic:
 *   只收绝对路径；按分隔符拼回一行。
 * @param {string[]} roots
 * @param {string} [existing]
 * @param {string} [sepChar]
 * @returns {string}
 */
export function formatSccacheBasedirs(roots, existing = '', sepChar = basedirsSeparator()) {
  const out = [];
  const seen = new Set();
  const push = (value) => {
    const trimmed = value.trim();
    if (!trimmed) {
      return;
    }
    const abs = resolve(trimmed);
    if (seen.has(abs)) {
      return;
    }
    seen.add(abs);
    out.push(abs);
  };
  for (const part of existing.split(sepChar)) {
    push(part);
  }
  for (const root of roots) {
    push(root);
  }
  return out.join(sepChar);
}

function log(message) {
  console.error(`[worktree-dev-cache] ${message}`);
}

/**
 * @param {string} repoRoot
 * @returns {string[]}
 */
function extraCheckoutRoots(repoRoot) {
  const extras = [];
  const envExtra = process.env.CC_PARTNER_SCCACHE_BASEDIRS ?? '';
  const sepChar = basedirsSeparator();
  for (const part of envExtra.split(sepChar)) {
    const trimmed = part.trim();
    if (trimmed) {
      extras.push(resolve(trimmed));
    }
  }
  for (const rel of ['.worktrees', join('.claude', 'worktrees')]) {
    const parent = join(repoRoot, rel);
    if (!existsSync(parent)) {
      continue;
    }
    let names;
    try {
      names = readdirSync(parent);
    } catch {
      continue;
    }
    for (const name of names) {
      const child = join(parent, name);
      try {
        if (statSync(child).isDirectory() && existsSync(join(child, 'src-tauri'))) {
          extras.push(child);
        }
      } catch {
        // skip
      }
    }
  }
  return extras;
}

/**
 * @param {string} repoRoot
 * @returns {string[]}
 */
export function listLinkedWorktrees(repoRoot) {
  const result = spawnSync('git', ['worktree', 'list', '--porcelain'], {
    cwd: repoRoot,
    encoding: 'utf8',
  });
  const fromGit = result.status === 0 ? parseGitWorktreePorcelain(result.stdout ?? '') : [];
  const roots = [repoRoot, ...fromGit, ...extraCheckoutRoots(repoRoot)];
  const existing = [];
  const seen = new Set();
  for (const root of roots) {
    const abs = resolve(root);
    if (seen.has(abs) || !existsSync(abs)) {
      continue;
    }
    seen.add(abs);
    existing.push(abs);
  }
  return existing;
}

/**
 * @returns {{ pid: string, cmd: string }[]}
 */
function listProcessTable() {
  const result = spawnSync('ps', ['-axo', 'pid=,command='], { encoding: 'utf8' });
  if (result.status !== 0) {
    return [];
  }
  const rows = [];
  for (const line of (result.stdout ?? '').split('\n')) {
    const match = line.trim().match(/^(\d+)\s+(.*)$/);
    if (!match) {
      continue;
    }
    rows.push({ pid: match[1], cmd: match[2] });
  }
  return rows;
}

/**
 * @param {string[]} pids
 * @returns {string[]}
 */
function listProcessCwds(pids) {
  if (pids.length === 0) {
    return [];
  }
  if (process.platform === 'linux') {
    const cwds = [];
    for (const pid of pids) {
      try {
        cwds.push(readlinkSync(`/proc/${pid}/cwd`));
      } catch {
        // skip
      }
    }
    return cwds;
  }
  const result = spawnSync(
    'lsof',
    ['-n', '-P', '-a', '-d', 'cwd', '-Fn', '-p', pids.join(',')],
    {
      encoding: 'utf8',
    },
  );
  if (result.status !== 0 && !(result.stdout ?? '').includes('\nn/')) {
    return [];
  }
  const cwds = [];
  for (const line of (result.stdout ?? '').split('\n')) {
    if (line.startsWith('n/')) {
      cwds.push(line.slice(1));
    }
  }
  return cwds;
}

/**
 * @param {string[]} worktrees
 * @param {{ processes?: { pid: string, cmd: string }[], cwds?: string[] }} [injected]
 * @returns {{ busyRoots: string[], probed: boolean }}
 */
export function detectBusyWorktrees(worktrees, injected) {
  const processes = injected?.processes ?? listProcessTable();
  if (!injected?.processes && processes.length === 0) {
    return { busyRoots: [], probed: false };
  }
  const busy = new Set();
  const compilePids = [];
  for (const { pid, cmd } of processes) {
    const compile = isCompileCommand(cmd);
    if (compile) {
      compilePids.push(pid);
    }
    for (const worktree of worktrees) {
      if (commandMentionsWorktree(cmd, worktree) && compile) {
        busy.add(resolve(worktree));
      }
    }
  }
  const cwds = injected?.cwds ?? listProcessCwds(compilePids);
  for (const cwd of cwds) {
    const match = longestMatchingRoot(cwd, worktrees);
    if (match) {
      busy.add(resolve(match));
    }
  }
  return { busyRoots: [...busy], probed: true };
}

/**
 * @param {string} worktree
 * @returns {boolean}
 */
export function worktreeHasCargoTarget(worktree) {
  return existsSync(join(worktree, 'src-tauri', 'target'));
}

/**
 * @param {string} worktree
 * @param {{ dryRun: boolean, cargoBin: string }} opts
 * @returns {boolean}
 */
function cargoCleanWorktree(worktree, opts) {
  const cwd = join(worktree, 'src-tauri');
  if (opts.dryRun) {
    log(`dry-run cargo clean cwd=${cwd}`);
    return true;
  }
  const result = spawnSync(opts.cargoBin, ['clean'], {
    cwd,
    encoding: 'utf8',
  });
  if (result.status !== 0) {
    log(
      `skip cargo clean ${cwd}: ${(result.stderr || result.stdout || `exit ${result.status}`).trim()}`,
    );
    return false;
  }
  log(`cargo clean ${cwd}`);
  return true;
}

function printBasedirs(repoRoot) {
  const roots = listLinkedWorktrees(repoRoot);
  const formatted = formatSccacheBasedirs(
    roots,
    process.env.SCCACHE_BASEDIRS ?? '',
    basedirsSeparator(),
  );
  process.stdout.write(`${formatted}\n`);
}

function runIdleClean(repoRoot, { dryRun }) {
  if (process.env.CC_PARTNER_IDLE_CARGO_CLEAN === '0') {
    log('CC_PARTNER_IDLE_CARGO_CLEAN=0，跳过 idle-clean');
    return;
  }
  const worktrees = listLinkedWorktrees(repoRoot);
  const { busyRoots, probed } = detectBusyWorktrees(worktrees);
  if (!probed) {
    log('无法列出进程表，跳过 idle-clean，以免误删正在编译的 target');
    return;
  }
  const { clean } = classifyIdleWorktrees({
    currentRoot: repoRoot,
    worktrees,
    busyRoots,
    hasTarget: worktreeHasCargoTarget,
  });
  if (clean.length === 0) {
    log(`idle-clean: 无闲置 target（worktrees=${worktrees.length} busy=${busyRoots.length}）`);
    return;
  }
  const cargoBin = process.env.CARGO ?? 'cargo';
  let cleaned = 0;
  for (const worktree of clean) {
    if (cargoCleanWorktree(worktree, { dryRun, cargoBin })) {
      cleaned += 1;
    }
  }
  log(`idle-clean: 处理 ${cleaned}/${clean.length} 个闲置 worktree`);
}

function parseArgs(argv) {
  const options = {
    printBasedirs: false,
    mode: '',
    dryRun: false,
    selfTest: false,
    help: false,
    repoRoot: REPO_ROOT,
  };
  for (const arg of argv) {
    if (arg === '--print-sccache-basedirs') {
      options.printBasedirs = true;
    } else if (arg === '--dry-run') {
      options.dryRun = true;
    } else if (arg === '--self-test') {
      options.selfTest = true;
    } else if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else if (arg.startsWith('--mode=')) {
      options.mode = arg.slice('--mode='.length);
    } else if (arg.startsWith('--repo-root=')) {
      options.repoRoot = resolve(arg.slice('--repo-root='.length));
    } else {
      throw new Error(`未知参数: ${arg}`);
    }
  }
  if (
    !options.printBasedirs &&
    !options.selfTest &&
    !options.help &&
    options.mode !== 'idle-clean'
  ) {
    throw new Error('需要 --print-sccache-basedirs、--mode=idle-clean 或 --self-test');
  }
  return options;
}

function printHelp() {
  console.log(`用法: node scripts/worktree-dev-cache.mjs [选项]

选项:
  --print-sccache-basedirs   stdout 打印 SCCACHE_BASEDIRS（本仓库 git worktree 根）
  --mode=idle-clean          cargo clean 闲置 worktree 的 src-tauri/target
  --dry-run
  --self-test
  --repo-root=<path>
  -h, --help

环境变量:
  SCCACHE_BASEDIRS / CC_PARTNER_SCCACHE_BASEDIRS  额外绝对路径（${basedirsSeparator()} 分隔）
  CC_PARTNER_IDLE_CARGO_CLEAN=0                   关闭 idle-clean
  SCCACHE_CACHE_SIZE                              由 start.sh 默认设为 ${DEFAULT_CACHE_SIZE}
`);
}

function selfTest() {
  const porcelain = [
    'worktree /tmp/cc-a',
    'HEAD abc',
    'branch refs/heads/a',
    '',
    'worktree /tmp/cc-a/.worktrees/nested',
    'HEAD def',
    'detached',
    '',
    'worktree /tmp/cc-bare',
    'bare',
    '',
  ].join('\n');
  assert.deepEqual(parseGitWorktreePorcelain(porcelain), [
    '/tmp/cc-a',
    '/tmp/cc-a/.worktrees/nested',
  ]);

  assert.equal(
    longestMatchingRoot('/tmp/cc-a/.worktrees/nested/src-tauri', [
      '/tmp/cc-a',
      '/tmp/cc-a/.worktrees/nested',
    ]),
    '/tmp/cc-a/.worktrees/nested',
  );
  assert.equal(longestMatchingRoot('/tmp/other', ['/tmp/cc-a']), null);

  assert.equal(isCompileCommand('cargo build --bin cc-partner-backend'), true);
  assert.equal(isCompileCommand('rustc --crate-name serde'), true);
  assert.equal(
    isCompileCommand('node /x/node_modules/@tauri-apps/cli/tauri.js dev'),
    true,
  );
  assert.equal(isCompileCommand('node /Applications/Cursor.app/Contents/MacOS/Cursor'), false);
  assert.equal(isCompileCommand('sccache --start-server'), false);

  assert.equal(
    commandMentionsWorktree('cargo --manifest-path /wt/foo/src-tauri/Cargo.toml', '/wt/foo'),
    true,
  );
  assert.equal(commandMentionsWorktree('cargo test', '/wt/foo'), false);

  const { busyRoots, probed } = detectBusyWorktrees(['/wt/a', '/wt/b'], {
    processes: [
      { pid: '1', cmd: 'cargo test' },
      { pid: '2', cmd: '/wt/b/src-tauri/target/debug/cc-partner-backend' },
    ],
    cwds: ['/wt/a/src-tauri'],
  });
  assert.equal(probed, true);
  assert.equal(busyRoots.includes(resolve('/wt/a')), true);
  assert.equal(busyRoots.includes(resolve('/wt/b')), true);

  const classified = classifyIdleWorktrees({
    currentRoot: '/wt/current',
    worktrees: ['/wt/current', '/wt/busy', '/wt/idle', '/wt/empty'],
    busyRoots: ['/wt/busy'],
    hasTarget: (worktree) => worktree !== resolve('/wt/empty'),
  });
  assert.deepEqual(classified.keep.map((p) => resolve(p)).sort(), [
    resolve('/wt/busy'),
    resolve('/wt/current'),
    resolve('/wt/empty'),
  ]);
  assert.deepEqual(classified.clean, [resolve('/wt/idle')]);

  assert.equal(
    formatSccacheBasedirs(['/b', '/a'], '/a', ':'),
    `${resolve('/a')}:${resolve('/b')}`,
  );
  assert.equal(basedirsSeparator('win32'), ';');
  assert.equal(basedirsSeparator('darwin'), ':');

  const root = mkdtempSync(join(tmpdir(), 'cc-partner-wt-cache-'));
  try {
    mkdirSync(join(root, 'src-tauri', 'target', 'debug'), { recursive: true });
    writeFileSync(join(root, 'src-tauri', 'target', 'debug', 'x'), 'x');
    assert.equal(worktreeHasCargoTarget(root), true);
    assert.equal(worktreeHasCargoTarget(join(root, 'missing')), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }

  log('self-test passed');
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printHelp();
    return;
  }
  if (options.selfTest) {
    selfTest();
    return;
  }
  if (options.printBasedirs) {
    printBasedirs(options.repoRoot);
    return;
  }
  if (options.mode === 'idle-clean') {
    runIdleClean(options.repoRoot, { dryRun: options.dryRun });
  }
}

try {
  main();
} catch (error) {
  console.error(
    `[worktree-dev-cache] ${error instanceof Error ? error.message : String(error)}`,
  );
  process.exit(1);
}
