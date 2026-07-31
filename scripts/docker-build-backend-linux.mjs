#!/usr/bin/env node
/**
 * docker-build-backend-linux.mjs — 在 Docker 内为本机交叉编译 Linux x86_64 的
 * `cc-partner-backend` 独立后端 CLI 二进制。
 *
 * Business Logic（为什么需要这个脚本）:
 *   本机是 macOS arm64；远端 power-vpn 是 Ubuntu 24.04 x86_64 且无 Rust 工具链。
 *   不能复用 macOS 产物，需用 Linux 容器原生编译以保证 glibc/动态库兼容。
 *   容器用 rust:bookworm（glibc 2.36）构建，产物兼容 Ubuntu 24.04（glibc 2.39）。
 *
 * Code Logic（这个脚本做什么）:
 *   1) 用内联 Dockerfile 构建 `cc-partner-linux-builder` 镜像（rust:bookworm + Tauri Linux 依赖）。
 *   2) 以宿主 UID 运行容器，挂载仓库 RW，CARGO_TARGET_DIR 与 CARGO_HOME 都用容器内/子目录，
 *      避免污染 macOS target 且文件归属宿主用户。
 *   3) `cargo build --release --bin cc-partner-backend --target x86_64-unknown-linux-gnu --locked`。
 *   4) 产物落在 src-tauri/target/x86_64-unknown-linux-gnu/release/cc-partner-backend。
 *
 * 用法: node scripts/docker-build-backend-linux.mjs
 *   远端运行还需 webkit2gtk/gtk 等 runtime 库（见 deploy 步骤）。
 */
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync, unlinkSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..');
const SRC_TAURI = resolve(REPO_ROOT, 'src-tauri');
const OUT_DIR = resolve(SRC_TAURI, 'target-linux', 'release');
const OUT_BIN = resolve(OUT_DIR, 'cc-partner-backend');
const IMAGE_TAG = 'cc-partner-linux-builder:bookworm';

const RUNTIME_DEPS = [
  'build-essential',
  'pkg-config',
  'libssl-dev',
  'libglib2.0-dev',
  'libgtk-3-dev',
  'libwebkit2gtk-4.1-dev',
  'libsoup-3.0-dev',
  'libjavascriptcoregtk-4.1-dev',
  'libayatana-appindicator3-dev',
  'libdbus-1-dev',
  // xcap / arboard / global-hotkey 需要的 X11 运行/构建库
  'libxcb1',
  'libxcb-randr0-dev',
  'libxcb-xfixes0-dev',
  'libxrandr-dev',
  'libxfixes-dev',
  'libxext-dev',
  'libxi-dev',
  'libxkbcommon-dev',
  'libegl-dev',
  'file',
].join(' ');

const DOCKERFILE = `FROM rust:1.95-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends ${RUNTIME_DEPS} \\
    && rm -rf /var/lib/apt/lists/*
ENV CARGO_TERM_COLOR=always
`;

function run(cmd, args, opts = {}) {
  const res = spawnSync(cmd, args, { stdio: 'inherit', ...opts });
  if (res.status !== 0) {
    throw new Error(`${cmd} ${args.join(' ')} 退出码 ${res.status ?? 'null'}`);
  }
}

function main() {
  if (!existsSync(resolve(SRC_TAURI, 'Cargo.toml'))) {
    throw new Error('未找到 src-tauri/Cargo.toml，请在仓库根目录运行');
  }

  const dockerfileTmp = resolve(REPO_ROOT, '.tmp.Dockerfile.linux-builder');
  writeFileSync(dockerfileTmp, DOCKERFILE);
  console.log('[docker-build] 构建镜像', IMAGE_TAG, '(linux/amd64 via QEMU)');
  try {
    run('docker', ['build', '--platform', 'linux/amd64', '-t', IMAGE_TAG, '-f', dockerfileTmp, '.'], { cwd: REPO_ROOT });
  } finally {
    try { unlinkSync(dockerfileTmp); } catch {}
  }

  console.log('[docker-build] 容器内 cargo build（amd64 原生，host UID 避免根归属）');
  const uidGid = `${process.getuid && process.getuid()}:${process.getgid && process.getgid()}`;
  run(
    'docker',
    [
      'run', '--rm',
      '--platform', 'linux/amd64',
      '--user', uidGid,
      '-e', 'HOME=/tmp',
      '-e', `CARGO_HOME=/tmp/cargo-home`,
      '-e', `CARGO_TARGET_DIR=/work/src-tauri/target-linux`,
      '-v', `${REPO_ROOT}:/work`,
      IMAGE_TAG,
      'bash', '-c',
      `export PATH=/usr/local/cargo/bin:$PATH && cd /work/src-tauri && cargo build --release --bin cc-partner-backend --locked`,
    ],
  );

  if (!existsSync(OUT_BIN)) {
    throw new Error(`未找到构建产物: ${OUT_BIN}`);
  }
  mkdirSync(OUT_DIR, { recursive: true });
  console.log('[docker-build] 完成:', OUT_BIN);
}

try {
  main();
} catch (e) {
  console.error('[docker-build] 失败:', e.message);
  process.exit(1);
}
