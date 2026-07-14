#!/usr/bin/env node
/**
 * check-docs.mjs — static documentation fact guard.
 *
 * Business Logic（为什么需要这个脚本）:
 *   Plan 8 校准后，README / PRD / 分层开发指令里的命令、链接、端口与发布机制
 *   必须与仓库事实一致。人工校对无法阻止旧 runner、tauri-action、动态端口
 *   等说法回流；本脚本在 CI 与本地对「当前用户/开发文档」做零依赖静态校验。
 *
 * Code Logic（这个脚本做什么）:
 *   - 无新 npm 依赖，仅用 Node 内置 fs/path/os/child_process。
 *   - 检查相对文件链接、GitHub 风格标题锚点、三反引号围栏配对。
 *   - 按文件作用域拒绝陈旧表述（README 的 tauri-action/动态端口、web/CLAUDE.md
 *     的 npx --yes tsx、docs 中把 hosted smoke 写成已覆盖 WSL/tmux/GUI/权限/
 *     多机 mDNS 等、src-tauri/CLAUDE.md 对 encode_mdns_capabilities 的 caps= 前缀误写）。
 *   - README 代码块中的命令名需命中 package scripts / CLI 分发 / 仓库脚本白名单。
 *   - CLI 子命令白名单只从 `src-tauri/src/backend/cli.rs` 的 `dispatch` match arms
 *     解析，不预置硬编码命令；源缺失或解析失败使检查失败。
 *   - 用户文档中引用的 `E2E-` / `L2-` / `L3-` evidence ID 必须存在于
 *     `docs/development/quality-matrix.json`（只做 ID 存在性，不重复完整矩阵校验；
 *     完整校验见 `scripts/check-quality-traceability.mjs`）。
 *   - 无参数时默认扫描全部 git 跟踪的 Markdown，仅排除 docs/superpowers/**。
 *   - 忽略 http(s)/mailto 外链；不检查 docs/superpowers/** 历史设计记录。
 *   - `--self-test` 用临时 Markdown fixture 覆盖正/反例，失败时打印 file:line。
 *
 * Usage:
 *   node scripts/check-docs.mjs [files...]
 *   node scripts/check-docs.mjs --self-test
 *   node scripts/check-docs.mjs            # 默认：全部 tracked Markdown（排除 superpowers）
 */

import { spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadKnownEvidenceIds } from './check-quality-traceability.mjs';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, '..');

/** CLI 源（dispatch match arms 是子命令唯一权威）。 */
const CLI_SOURCE_REL = 'src-tauri/src/backend/cli.rs';

/** 文档可引用的自动化/真机 evidence ID（不含 L0；L0 由矩阵自身与 unit 守卫）。 */
const EVIDENCE_ID_RE = /\b((?:E2E|L2|L3)-[A-Z0-9]+(?:-[A-Z0-9]+)*)\b/g;

/**
 * 从 Markdown 抽出 E2E/L2/L3 evidence ID 引用。
 *
 * Business Logic（为什么需要这个函数）:
 *   人类文档会链接稳定 test ID；写错 ID 会导致 coverage 断链且完整矩阵校验发现不了。
 *
 * Code Logic（这个函数做什么）:
 *   用正则扫描全文，返回去重后的 ID 列表（保持首次出现顺序）。
 *
 * @param {string} content
 * @returns {string[]}
 */
function extractEvidenceIds(content) {
  /** @type {string[]} */
  const ids = [];
  /** @type {Set<string>} */
  const seen = new Set();
  EVIDENCE_ID_RE.lastIndex = 0;
  let m;
  while ((m = EVIDENCE_ID_RE.exec(content)) !== null) {
    const id = m[1];
    if (!seen.has(id)) {
      seen.add(id);
      ids.push(id);
    }
  }
  return ids;
}

/**
 * 校验文档中引用的 evidence ID 是否在 quality-matrix 中。
 *
 * Business Logic（为什么需要这个函数）:
 *   Docs workflow 只需轻量确认「文档写的 ID 真的登记过」，不重复 JSON schema 校验。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 content 中的 E2E-/L2-/L3- ID；若 knownIds 非空且 ID 不在集合内则报 finding。
 *   knownIds 为空（矩阵缺失）时不报错，由 traceability 门禁单独失败。
 *
 * @param {string} relFile
 * @param {string} content
 * @param {Set<string>} knownIds
 * @returns {Finding[]}
 */
function checkEvidenceIdRefs(relFile, content, knownIds) {
  /** @type {Finding[]} */
  const findings = [];
  if (!knownIds || knownIds.size === 0) return findings;
  const lines = content.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    EVIDENCE_ID_RE.lastIndex = 0;
    let m;
    while ((m = EVIDENCE_ID_RE.exec(line)) !== null) {
      const id = m[1];
      if (!knownIds.has(id)) {
        findings.push(
          finding(
            relFile,
            i + 1,
            `unknown evidence id ${id} (not in docs/development/quality-matrix.json)`,
          ),
        );
      }
    }
  }
  return findings;
}

/** docs/superpowers 下的历史 plan/spec 不参与当前文档守卫。 */

/**
 * 作用域陈旧表述规则。
 * @type {Array<{ id: string, test: (relPath: string, content: string) => string | null }>}
 */
const STALE_RULES = [
  {
    id: 'readme-tauri-action',
    test(relPath, content) {
      if (!isPath(relPath, 'README.md')) return null;
      if (/\btauri-action\b/i.test(content) || /tauri-apps\/tauri-action/i.test(content)) {
        return 'README must not mention tauri-action (release uses native tauri CLI three-job flow)';
      }
      return null;
    },
  },
  {
    id: 'readme-dynamic-port',
    test(relPath, content) {
      if (!isPath(relPath, 'README.md')) return null;
      // 禁止把 P2P HTTP 描述成「动态端口」(preferred 62116 + 占用递增，不是 OS ephemeral 0)。
      if (/动态端口/.test(content)) {
        return 'README must not call P2P HTTP “动态端口” (preferred 62116 + increment; health http_port is actual)';
      }
      return null;
    },
  },
  {
    id: 'web-claude-npx-yes-tsx',
    test(relPath, content) {
      if (!isPath(relPath, 'web/CLAUDE.md')) return null;
      if (/npx\s+--yes\s+tsx/.test(content)) {
        return 'web/CLAUDE.md must not recommend `npx --yes tsx` (use npm test / package scripts)';
      }
      return null;
    },
  },
  {
    id: 'docs-hosted-smoke-overclaim',
    test(relPath, content) {
      if (!isDocsUserFacing(relPath)) return null;
      // 禁止把 hosted smoke 写成已验证下列能力。
      const overclaims = [
        {
          re: /hosted\s+smoke[^.\n]{0,80}(verif(?:y|ies|ied)|covers?|覆盖|验证)[^.\n]{0,80}(WSL\s*\+?\s*tmux|WSL.*tmux)/i,
          msg: 'docs must not claim hosted smoke covers WSL+tmux',
        },
        {
          re: /(WSL\s*\+?\s*tmux|WSL.*tmux)[^.\n]{0,80}(hosted\s+smoke|CI\s+smoke)[^.\n]{0,40}(verif|cover|覆盖|验证)/i,
          msg: 'docs must not claim hosted smoke covers WSL+tmux',
        },
        {
          re: /hosted\s+smoke[^.\n]{0,80}(verif(?:y|ies|ied)|covers?|覆盖|验证)[^.\n]{0,80}(GUI|WebView)/i,
          msg: 'docs must not claim hosted smoke covers GUI/WebView',
        },
        {
          re: /(GUI|WebView)[^.\n]{0,80}(hosted\s+smoke|CI\s+smoke)[^.\n]{0,40}(verif|cover|覆盖|验证)/i,
          msg: 'docs must not claim hosted smoke covers GUI/WebView',
        },
        {
          re: /hosted\s+smoke[^.\n]{0,100}(verif(?:y|ies|ied)|covers?|覆盖|验证)[^.\n]{0,80}(permission|权限)/i,
          msg: 'docs must not claim hosted smoke covers permissions',
        },
        {
          re: /(permission|权限弹窗|macOS\s*权限)[^.\n]{0,80}(hosted\s+smoke|CI\s+smoke)[^.\n]{0,40}(verif|cover|覆盖|验证)/i,
          msg: 'docs must not claim hosted smoke covers permissions',
        },
        {
          re: /hosted\s+smoke[^.\n]{0,100}(verif(?:y|ies|ied)|covers?|覆盖|验证)[^.\n]{0,80}(multi[- ]host\s+mDNS|多机\s*mDNS|跨主机\s*mDNS)/i,
          msg: 'docs must not claim hosted smoke covers multi-host mDNS',
        },
        {
          re: /(multi[- ]host\s+mDNS|多机\s*mDNS|跨主机\s*mDNS)[^.\n]{0,80}(hosted\s+smoke|CI\s+smoke)[^.\n]{0,40}(verif|cover|覆盖|验证)/i,
          msg: 'docs must not claim hosted smoke covers multi-host mDNS',
        },
        // 中文常见误写：smoke 已验证 WSL/tmux / GUI / 权限 / 多机 mDNS
        {
          re: /(smoke|冒烟)[^。\n]{0,40}(已验证|覆盖了|验证了)[^。\n]{0,40}(WSL|tmux|GUI|WebView|权限|多机\s*mDNS|multi[- ]host)/i,
          msg: 'docs must not claim smoke already verifies WSL/tmux, GUI/WebView, permissions, or multi-host mDNS',
        },
      ];
      for (const rule of overclaims) {
        if (rule.re.test(content)) return rule.msg;
      }
      return null;
    },
  },
  {
    id: 'mdns-caps-prefix',
    test(relPath, content) {
      // encode_mdns_capabilities 返回裸 token；caps= 由 TXT key 提供，禁止文档写“输出始终带 caps= 前缀”。
      if (!isPath(relPath, 'src-tauri/CLAUDE.md') && !isPath(relPath, 'CLAUDE.md')) {
        return null;
      }
      if (
        /encode_mdns_capabilities[^。\n]{0,120}(始终以\s*`?caps=`?\s*前缀|输出始终以\s*`?caps=`?|带\s*`?caps=`?\s*前缀开头)/i.test(
          content,
        ) ||
        /encode_mdns_capabilities[^.\n]{0,120}(always\s+(?:starts\s+with|prefixes?|emits)\s+`?caps=`?)/i.test(
          content,
        )
      ) {
        return 'encode_mdns_capabilities must be documented as bare comma-separated tokens (TXT key supplies caps=)';
      }
      return null;
    },
  },
  {
    id: 'docs-lan-configurable-mode',
    test(relPath, content) {
      if (!isDocsUserFacing(relPath)) return null;
      // 产品只有一种固定 LAN 行为；禁止可切换暴露/只读模式、mode DTO、LAN 权限 capability token。
      const banned = [
        {
          re: /\bLAN\s+mode\b/i,
          msg: 'docs must not describe configurable LAN mode (fixed unauthenticated LAN only)',
        },
        {
          re: /\blan\.mode\b/i,
          msg: 'docs must not document lan.mode config (fixed unauthenticated LAN only)',
        },
        {
          re: /\blan\.permission\b|\blan\.auth\b/i,
          msg: 'docs must not document lan.permission / lan.auth capability tokens',
        },
        {
          re: /LAN\s+permission\s+capability|capability\s+token[^.\n]{0,60}LAN|LAN[^.\n]{0,60}capability\s+token/i,
          msg: 'docs must not document LAN permission capability tokens',
        },
        {
          re: /可切换[^。\n]{0,24}LAN|LAN[^。\n]{0,24}可切换/,
          msg: 'docs must not describe switchable LAN exposure modes',
        },
        {
          re: /暴露模式/,
          msg: 'docs must not describe LAN 暴露模式 as a product mode',
        },
        {
          re: /只读模式/,
          msg: 'docs must not describe LAN 只读模式 as a product mode',
        },
      ];
      for (const rule of banned) {
        if (!rule.re.test(content)) continue;
        // 允许“禁止/不得/不提供 … 模式”这类否定说明，避免工程指令自伤。
        if (hasLanNegationContext(content, rule.re)) continue;
        return rule.msg;
      }
      return null;
    },
  },
  {
    id: 'docs-lan-route-authorization',
    test(relPath, content) {
      if (!isDocsUserFacing(relPath)) return null;
      // 禁止“每条 /api 路由有设备级授权效果”的矩阵叙事；保留 retry class 表（含 read-only 类名）。
      const banned = [
        {
          re: /route\s+authorization\s+matrix/i,
          msg: 'docs must not define a route authorization matrix for LAN APIs',
        },
        {
          re: /per[- ]route\s+(auth(?:orization)?|permission)\b/i,
          msg: 'docs must not claim per-route authorization/permission for LAN APIs',
        },
        {
          re: /\bauth(?:orization)?\s+effect\b/i,
          msg: 'docs must not add authorization/effect columns to the P2P route inventory',
        },
        {
          re: /路由授权矩阵|按路由[^。\n]{0,20}授权|设备级[^。\n]{0,12}授权/,
          msg: 'docs must not define per-route/device authorization for LAN APIs',
        },
      ];
      for (const rule of banned) {
        if (!rule.re.test(content)) continue;
        if (hasLanNegationContext(content, rule.re)) continue;
        return rule.msg;
      }
      return null;
    },
  },
  {
    id: 'docs-lan-peer-identity-claims',
    test(relPath, content) {
      if (!isDocsUserFacing(relPath)) return null;
      // 禁止把通过 socket gate 的 peer 写成身份意义上的“已认证/可信/安全设备”。
      // 刻意不匹配 Windows Defender「高级安全」产品名；只盯身份断言短语。
      const banned = [
        {
          re: /authenticated\s+LAN\s+peers?|LAN\s+peers?\s+are\s+authenticated|peers?\s+are\s+(authenticated|trusted)\b/i,
          msg: 'docs must not claim LAN peers are authenticated/trusted identities',
        },
        {
          re: /\b(trusted|secure)\s+devices?\b/i,
          msg: 'docs must not call LAN peers trusted/secure devices (identity claim)',
        },
        {
          re: /(局域网|LAN)[^。.\n]{0,24}(已认证|可信设备|安全设备)|(已认证|可信设备|安全设备)[^。.\n]{0,24}(局域网|LAN|peer)/i,
          msg: 'docs must not claim LAN peers are 已认证/可信设备/安全设备',
        },
        {
          re: /是可信设备|为可信设备|作为可信设备|是安全设备|为安全设备/,
          msg: 'docs must not claim peers are 可信/安全设备',
        },
      ];
      for (const rule of banned) {
        if (!rule.re.test(content)) continue;
        if (hasLanNegationContext(content, rule.re)) continue;
        return rule.msg;
      }
      return null;
    },
  },
  {
    id: 'docs-lan-vague-trust',
    test(relPath, content) {
      if (!isDocsUserFacing(relPath)) return null;
      // 模糊“可信局域网”产品信任声称必须换成固定无身份风险声明。
      if (/个人可信局域网/.test(content)) {
        return 'replace “个人可信局域网” with fixed no-identity LAN risk wording';
      }
      if (/可信局域网/.test(content)) {
        return 'replace “可信局域网” with fixed no-identity LAN risk wording';
      }
      if (/trusted personal LAN/i.test(content)) {
        return 'replace “trusted personal LAN” with fixed no-identity LAN risk wording';
      }
      if (/\btrusted LAN\b/i.test(content)) {
        return 'replace “trusted LAN” with fixed no-identity LAN risk wording';
      }
      return null;
    },
  },
  {
    id: 'docs-lan-fixed-risk-required',
    test(relPath, content) {
      // PRD 与运维文档必须出现固定无身份风险声明（中文或英文等价）。
      const n = normalizeRel(relPath);
      if (n !== 'docs/prd.md' && n !== 'docs/development/backend-operations.md') {
        return null;
      }
      if (hasFixedLanRiskStatement(content)) return null;
      return 'must include fixed no-identity LAN risk statement (Chinese fixed phrase or English equivalent)';
    },
  },
  {
    id: 'docs-lan-firewall-guidance',
    test(relPath, content) {
      // README / backend-operations 防火墙指引必须同时写 UDP 5353 与实际/首选 TCP 端口。
      const n = normalizeRel(relPath);
      if (n !== 'README.md' && n !== 'docs/development/backend-operations.md') {
        return null;
      }
      const hasUdp5353 = /UDP\s*\*?\*?5353\*?\*?|\*?\*?5353\*?\*?[^.\n]{0,40}UDP|UDP[^.\n]{0,40}\*?\*?5353\*?\*?/i.test(
        content,
      );
      const hasTcpPort =
        /\b62116\b/.test(content) ||
        /actual\s+(TCP\s+)?port|TCP\s+\*?\*?actual|实际.*(?:TCP\s*)?端口|实际端口/i.test(
          content,
        );
      if (!hasUdp5353 || !hasTcpPort) {
        return 'firewall guidance must mention UDP 5353 and actual/preferred TCP port (62116)';
      }
      return null;
    },
  },
];

/**
 * Business Logic（为什么需要这个函数）:
 *   LAN 边界相关文档既要禁止“模式/已认证”等正向错误叙事，又允许“禁止写模式”
 *   这类工程否定句；否则 CLAUDE/计划指令会自伤。
 *
 * Code Logic（这个函数做什么）:
 *   在 content 中定位 re 的首次命中，检查命中行前后各 1 行是否含否定措辞；
 *   命中否定上下文则返回 true（调用方应跳过该规则）。
 * @param {string} content
 * @param {RegExp} re
 * @returns {boolean}
 */
function hasLanNegationContext(content, re) {
  const lines = content.split(/\r?\n/);
  const flags = re.flags.includes('g') ? re.flags : `${re.flags}g`;
  const globalRe = new RegExp(re.source, flags);
  for (let i = 0; i < lines.length; i++) {
    globalRe.lastIndex = 0;
    if (!globalRe.test(lines[i])) continue;
    // 前后各 1 行；去掉 markdown 粗体标记，便于匹配 **no** / **not**。
    const window = [lines[i - 1] ?? '', lines[i], lines[i + 1] ?? '']
      .join(' ')
      .replace(/\*\*/g, '');
    if (
      /禁止|不得|勿|不要|避免|不提供|不实现|不增加|不新增|不引入|不描述|不得写|禁止写|\bmust not\b|\bdo not\b|\bdoes not\b|\bnever\b|\bwithout\b|\bno\b|\bnot\b|\bneither\b/i.test(
        window,
      )
    ) {
      return true;
    }
  }
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   PRD 与运维文档必须固定声明“无调用者身份校验”的剩余风险，防止产品文案回退到
 *   模糊“可信局域网”叙事。
 *
 * Code Logic（这个函数做什么）:
 *   匹配固定中文风险句或英文等价句；允许分号/逗号/句号轻微标点差异。
 * @param {string} content
 * @returns {boolean}
 */
function hasFixedLanRiskStatement(content) {
  if (
    /同一可达网络中的任何设备均可读取、写入和执行[；;,.，]?\s*系统不验证调用者身份/.test(
      content,
    )
  ) {
    return true;
  }
  if (
    /Any device on the same reachable network can read,\s*write,\s*and execute[；;,.，]?\s*the system does not verify caller identity/i.test(
      content,
    )
  ) {
    return true;
  }
  return false;
}

/**
 * Business Logic: 路径比较需兼容 Windows 分隔符与相对路径写法。
 * Code Logic: 归一化为相对 repo 根的 posix-ish 字符串再比较。
 * @param {string} relPath
 * @param {string} expected
 */
function isPath(relPath, expected) {
  return normalizeRel(relPath) === normalizeRel(expected);
}

/**
 * @param {string} p
 */
function normalizeRel(p) {
  return p.split(/[/\\]/).filter(Boolean).join('/');
}

/**
 * 用户/开发文档（非 superpowers 历史记录）。
 * @param {string} relPath
 */
function isDocsUserFacing(relPath) {
  const n = normalizeRel(relPath);
  if (n.startsWith('docs/superpowers/')) return false;
  if (n === 'README.md' || n === 'AGENTS.md' || n === 'CLAUDE.md') return true;
  if (n.startsWith('docs/')) return true;
  if (n.endsWith('/CLAUDE.md') || n.endsWith('/AGENTS.md')) return true;
  return false;
}

/**
 * @typedef {{ file: string, line: number, message: string }} Finding
 */

/**
 * Business Logic: 诊断必须带文件与行号，方便 CI 日志定位。
 * Code Logic: 统一 finding 结构。
 * @param {string} file
 * @param {number} line
 * @param {string} message
 * @returns {Finding}
 */
function finding(file, line, message) {
  return { file, line, message };
}

/**
 * GitHub-style heading slug（GFM）：
 * 小写、去标点（保留字母数字/空格/-/CJK）、空格→-、折叠重复 -。
 * @param {string} text
 */
function githubSlug(text) {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s\-]/gu, '')
    .replace(/\s+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '');
}

/**
 * 去掉围栏代码块内容，避免把代码里的 # 或链接当文档结构。
 * 同时返回围栏配对错误。
 * @param {string} content
 * @param {string} file
 * @returns {{ stripped: string, fenceFindings: Finding[], lineMap: number[] }}
 */
function stripFences(content, file) {
  const lines = content.split(/\r?\n/);
  const out = [];
  /** @type {number[]} 输出行 → 原始 1-based 行号 */
  const lineMap = [];
  /** @type {Finding[]} */
  const fenceFindings = [];
  let inFence = false;
  let fenceStart = 0;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^(\s*)(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (!inFence) {
        inFence = true;
        fenceStart = i + 1;
        out.push(''); // 保留行占位，锚点行号仍对齐
        lineMap.push(i + 1);
      } else {
        inFence = false;
        out.push('');
        lineMap.push(i + 1);
      }
      continue;
    }
    if (inFence) {
      out.push('');
      lineMap.push(i + 1);
    } else {
      out.push(line);
      lineMap.push(i + 1);
    }
  }

  if (inFence) {
    fenceFindings.push(
      finding(file, fenceStart, 'unbalanced fenced code block (opening ``` not closed)'),
    );
  }

  return { stripped: out.join('\n'), fenceFindings, lineMap };
}

/**
 * 收集标题 slug → 首次出现行号。
 * @param {string} stripped
 * @param {number[]} lineMap
 * @returns {Map<string, number>}
 */
function collectHeadingSlugs(stripped, lineMap) {
  /** @type {Map<string, number>} */
  const slugs = new Map();
  const lines = stripped.split(/\r?\n/);
  /** @type {Map<string, number>} */
  const counts = new Map();

  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^(#{1,6})\s+(.+?)\s*#*\s*$/);
    if (!m) continue;
    let base = githubSlug(m[2].replace(/\[([^\]]+)\]\([^)]+\)/g, '$1').replace(/`([^`]+)`/g, '$1'));
    if (!base) continue;
    const n = counts.get(base) ?? 0;
    counts.set(base, n + 1);
    const slug = n === 0 ? base : `${base}-${n}`;
    if (!slugs.has(slug)) slugs.set(slug, lineMap[i] ?? i + 1);
  }
  return slugs;
}

/**
 * 提取 Markdown 链接（行内 + 引用定义跳过图片）。
 * @param {string} stripped
 * @param {number[]} lineMap
 * @returns {Array<{ href: string, line: number, text: string }>}
 */
function extractLinks(stripped, lineMap) {
  const lines = stripped.split(/\r?\n/);
  /** @type {Array<{ href: string, line: number, text: string }>} */
  const links = [];
  // [text](href) 与 ![alt](href)；只校验非图片链接的目标文件，图片也校验相对路径存在性
  const re = /(!)?\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    re.lastIndex = 0;
    let m;
    while ((m = re.exec(line)) !== null) {
      const href = m[3].trim();
      links.push({
        href,
        line: lineMap[i] ?? i + 1,
        text: m[2],
      });
    }
  }
  return links;
}

/**
 * 是否应忽略的链接（外链 / 纯锚点占位 / 邮件）。
 * @param {string} href
 */
function shouldIgnoreHref(href) {
  if (!href || href === '#') return true;
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) {
    // scheme: http https mailto 等全部忽略
    return true;
  }
  return false;
}

/**
 * 校验相对链接与锚点。
 * @param {string} absFile
 * @param {string} relFile
 * @param {string} stripped
 * @param {number[]} lineMap
 * @param {Map<string, Map<string, number>>} slugIndex relPath → slugs
 * @returns {Finding[]}
 */
function checkLinks(absFile, relFile, stripped, lineMap, slugIndex) {
  /** @type {Finding[]} */
  const findings = [];
  const links = extractLinks(stripped, lineMap);
  const fileDir = dirname(absFile);
  const ownSlugs = slugIndex.get(normalizeRel(relFile)) ?? new Map();

  for (const { href, line } of links) {
    if (shouldIgnoreHref(href)) continue;

    let pathPart = href;
    let hash = '';
    const hashIdx = href.indexOf('#');
    if (hashIdx >= 0) {
      pathPart = href.slice(0, hashIdx);
      hash = href.slice(hashIdx + 1);
    }

    // 纯同文件锚点
    if (!pathPart) {
      if (hash && !ownSlugs.has(hash) && !ownSlugs.has(decodeURIComponent(hash))) {
        findings.push(
          finding(relFile, line, `missing heading anchor #${hash}`),
        );
      }
      continue;
    }

    // 拒绝绝对磁盘路径式链接
    if (isAbsolute(pathPart) || /^[a-zA-Z]:[\\/]/.test(pathPart)) {
      findings.push(finding(relFile, line, `absolute path link not allowed: ${pathPart}`));
      continue;
    }

    const targetAbs = resolve(fileDir, pathPart);
    const targetRel = normalizeRel(relative(repoRoot, targetAbs));

    // 逃出仓库
    if (targetRel.startsWith('..')) {
      findings.push(finding(relFile, line, `link escapes repository: ${pathPart}`));
      continue;
    }

    if (!existsSync(targetAbs)) {
      findings.push(finding(relFile, line, `broken relative link: ${pathPart}`));
      continue;
    }

    // 目录链接：接受存在的目录
    let st;
    try {
      st = statSync(targetAbs);
    } catch {
      findings.push(finding(relFile, line, `broken relative link: ${pathPart}`));
      continue;
    }

    if (st.isDirectory()) {
      // 目录不校验锚点
      continue;
    }

    if (hash) {
      const targetSlugs =
        slugIndex.get(targetRel) ??
        loadSlugsForFile(targetAbs, targetRel);
      if (!targetSlugs.has(hash) && !targetSlugs.has(decodeURIComponent(hash))) {
        findings.push(
          finding(
            relFile,
            line,
            `missing heading anchor ${pathPart}#${hash}`,
          ),
        );
      }
    }
  }

  return findings;
}

/**
 * 懒加载未在本轮检查集合中的目标文件标题。
 * @param {string} abs
 * @param {string} rel
 */
function loadSlugsForFile(abs, rel) {
  try {
    const content = readFileSync(abs, 'utf8');
    const { stripped, lineMap } = stripFences(content, rel);
    return collectHeadingSlugs(stripped, lineMap);
  } catch {
    return new Map();
  }
}

/**
 * 作用域陈旧表述检查。
 * @param {string} relFile
 * @param {string} content
 * @returns {Finding[]}
 */
function checkStaleClaims(relFile, content) {
  /** @type {Finding[]} */
  const findings = [];
  for (const rule of STALE_RULES) {
    const msg = rule.test(relFile, content);
    if (!msg) continue;
    // 定位首次命中行
    const line = firstMatchingLine(content, rule);
    findings.push(finding(relFile, line, `[${rule.id}] ${msg}`));
  }
  return findings;
}

/**
 * @param {string} content
 * @param {{ id: string, test: Function }} rule
 */
function firstMatchingLine(content, rule) {
  const lines = content.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    // 逐行试探：用单行内容调用规则（部分规则看全文，回退全文命中行搜索）
    if (rule.id === 'readme-tauri-action' && /tauri-action/i.test(lines[i])) return i + 1;
    if (rule.id === 'readme-dynamic-port' && /动态端口/.test(lines[i])) return i + 1;
    if (rule.id === 'web-claude-npx-yes-tsx' && /npx\s+--yes\s+tsx/.test(lines[i])) return i + 1;
    if (rule.id === 'docs-hosted-smoke-overclaim') {
      if (/(hosted\s+smoke|CI\s+smoke|smoke|冒烟)/i.test(lines[i]) &&
          /(WSL|tmux|GUI|WebView|权限|mDNS|verif|cover|覆盖|验证)/i.test(lines[i])) {
        return i + 1;
      }
    }
    if (
      rule.id === 'mdns-caps-prefix' &&
      /encode_mdns_capabilities/.test(lines[i]) &&
      /caps=/.test(lines[i])
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-configurable-mode' &&
      (/\bLAN\s+mode\b|\blan\.mode\b|\blan\.permission\b|\blan\.auth\b|暴露模式|只读模式|可切换/i.test(
        lines[i],
      ) ||
        /LAN\s+permission\s+capability|capability\s+token/i.test(lines[i]))
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-route-authorization' &&
      /route\s+authorization|per[- ]route\s+(auth|permission)|auth(?:orization)?\s+effect|路由授权|按路由|设备级/i.test(
        lines[i],
      )
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-peer-identity-claims' &&
      /(authenticated|trusted device|secure device|已认证|可信设备|安全设备)/i.test(lines[i])
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-vague-trust' &&
      /(可信局域网|个人可信局域网|trusted personal LAN|trusted LAN)/i.test(lines[i])
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-fixed-risk-required' &&
      /(LAN|局域网|mobile|trust|风险|identity|peer)/i.test(lines[i])
    ) {
      return i + 1;
    }
    if (
      rule.id === 'docs-lan-firewall-guidance' &&
      /(firewall|防火墙|5353|62116|UDP|TCP|port|端口)/i.test(lines[i])
    ) {
      return i + 1;
    }
  }
  return 1;
}

/**
 * 从 cli.rs 的 dispatch match arms 解析 CLI 子命令（空集合起步，不预置硬编码）。
 *
 * Business Logic: README 推荐的 backend 子命令必须以源码 dispatch 为准；
 *   预置白名单会在命令删除后继续放行，造成假绿。
 * Code Logic: 定位 `fn dispatch` 后的第一个 `match command { ... }`，
 *   抽取 `Some("name")` 字面量；源缺失或解析不到任何 arm 则返回 error。
 *
 * @param {string} [cliSourceAbs] 可选：注入 fixture 路径；默认仓库 CLI_SOURCE_REL
 * @returns {{ ok: true, commands: Set<string> } | { ok: false, error: string }}
 */
function parseCliSubcommandsFromSource(cliSourceAbs) {
  const abs = cliSourceAbs ?? join(repoRoot, CLI_SOURCE_REL);
  if (!existsSync(abs)) {
    return {
      ok: false,
      error: `CLI source missing: ${cliSourceAbs ? abs : CLI_SOURCE_REL}`,
    };
  }
  let src;
  try {
    src = readFileSync(abs, 'utf8');
  } catch (err) {
    return {
      ok: false,
      error: `CLI source unreadable: ${err instanceof Error ? err.message : String(err)}`,
    };
  }

  // 锚定 dispatch 函数体，避免误匹配其它 match。
  const dispatchIdx = src.search(/fn\s+dispatch\b/);
  if (dispatchIdx < 0) {
    return { ok: false, error: 'CLI dispatch function not found in cli.rs' };
  }
  const afterDispatch = src.slice(dispatchIdx);
  const matchIdx = afterDispatch.search(/match\s+command\s*\{/);
  if (matchIdx < 0) {
    return { ok: false, error: 'CLI dispatch match command block not found' };
  }
  const braceStart = afterDispatch.indexOf('{', matchIdx);
  if (braceStart < 0) {
    return { ok: false, error: 'CLI dispatch match block missing opening brace' };
  }

  // 扫描配对花括号，截取 match 体。
  let depth = 0;
  let end = -1;
  for (let i = braceStart; i < afterDispatch.length; i++) {
    const ch = afterDispatch[i];
    if (ch === '{') depth += 1;
    else if (ch === '}') {
      depth -= 1;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  if (end < 0) {
    return { ok: false, error: 'CLI dispatch match block unclosed' };
  }
  const body = afterDispatch.slice(braceStart + 1, end);
  /** @type {Set<string>} */
  const commands = new Set();
  for (const m of body.matchAll(/Some\(\s*"([a-zA-Z][a-zA-Z0-9_-]*)"\s*\)/g)) {
    commands.add(m[1]);
  }
  if (commands.size === 0) {
    return {
      ok: false,
      error: 'CLI dispatch match arms yielded zero subcommands',
    };
  }
  return { ok: true, commands };
}

/**
 * 从 web/package.json / start.sh / backend CLI / scripts 构建命令白名单。
 * @param {{ cliSourceAbs?: string }} [opts]
 * @returns {{
 *   npmScripts: Set<string>,
 *   cliSubcommands: Set<string>,
 *   rootScripts: Set<string>,
 *   startModes: Set<string>,
 *   cliError: string | null,
 * }}
 */
function loadCommandAllowlist(opts = {}) {
  /** @type {Set<string>} */
  const npmScripts = new Set();
  const pkgPath = join(repoRoot, 'web/package.json');
  if (existsSync(pkgPath)) {
    try {
      const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
      for (const name of Object.keys(pkg.scripts ?? {})) {
        npmScripts.add(name);
      }
    } catch {
      // ignore parse errors; command check will surface empty allowlist issues
    }
  }

  /** @type {Set<string>} */
  const cliSubcommands = new Set();
  let cliError = /** @type {string | null} */ (null);
  const parsed = parseCliSubcommandsFromSource(opts.cliSourceAbs);
  if (parsed.ok) {
    for (const c of parsed.commands) cliSubcommands.add(c);
  } else {
    cliError = parsed.error;
  }

  /** @type {Set<string>} */
  const rootScripts = new Set();
  const scriptsDir = join(repoRoot, 'scripts');
  if (existsSync(scriptsDir)) {
    for (const name of readdirSync(scriptsDir)) {
      if (/\.(mjs|js|sh)$/.test(name)) rootScripts.add(name);
    }
  }

  /** @type {Set<string>} */
  const startModes = new Set(['dev', 'build', 'web', 'clean', 'help']);

  return { npmScripts, cliSubcommands, rootScripts, startModes, cliError };
}

/**
 * 校验 README 中出现的可执行命令是否能在源中找到。
 * @param {string} relFile
 * @param {string} content
 * @param {ReturnType<typeof loadCommandAllowlist>} allow
 * @returns {Finding[]}
 */
function checkReadmeCommands(relFile, content, allow) {
  if (!isPath(relFile, 'README.md')) return [];

  /** @type {Finding[]} */
  const findings = [];
  const lines = content.split(/\r?\n/);
  let inFence = false;
  let fenceLang = '';

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^(\s*)(`{3,}|~{3,})\s*([\w+-]*)\s*$/);
    if (fence) {
      if (!inFence) {
        inFence = true;
        fenceLang = (fence[3] || '').toLowerCase();
      } else {
        inFence = false;
        fenceLang = '';
      }
      continue;
    }
    if (!inFence) continue;
    // 只检查 shell 类代码块；无语言标记的块也扫（README 常用 bash）
    if (fenceLang && !/^(bash|sh|shell|zsh|console|powershell|text|)$/.test(fenceLang)) {
      continue;
    }

    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;

    // npm run <script> / npm test / npm ci / npm install / npm exec
    const npmRun = trimmed.match(/\bnpm\s+run\s+([a-zA-Z0-9:_-]+)/);
    if (npmRun && !allow.npmScripts.has(npmRun[1])) {
      findings.push(
        finding(relFile, i + 1, `README npm run script not in web/package.json: ${npmRun[1]}`),
      );
    }
    // bare `npm test` is package script "test"
    if (/\bnpm\s+test\b/.test(trimmed) && !allow.npmScripts.has('test')) {
      findings.push(finding(relFile, i + 1, 'README `npm test` but web/package.json has no test script'));
    }

    // node scripts/<file>
    const nodeScript = trimmed.match(/\bnode\s+scripts\/([A-Za-z0-9._-]+)/);
    if (nodeScript && !allow.rootScripts.has(nodeScript[1])) {
      findings.push(
        finding(relFile, i + 1, `README node scripts/${nodeScript[1]} not found under scripts/`),
      );
    }

    // ./start.sh [mode]
    const startSh = trimmed.match(/(?:\.\/)?start\.sh(?:\s+([a-zA-Z0-9_-]+))?/);
    if (startSh && startSh[1] && !allow.startModes.has(startSh[1])) {
      findings.push(
        finding(relFile, i + 1, `README start.sh mode not recognized: ${startSh[1]}`),
      );
    }

    // cc-partner-backend <sub>（跳过 cargo 行里的 `--` 分隔符）
    const backend = trimmed.match(/\bcc-partner-backend\s+([a-zA-Z][a-zA-Z0-9_-]*)/);
    if (backend && !allow.cliSubcommands.has(backend[1])) {
      findings.push(
        finding(
          relFile,
          i + 1,
          `README cc-partner-backend subcommand not in CLI allowlist: ${backend[1]}`,
        ),
      );
    }

    // cargo run --bin cc-partner-backend -- <sub>
    const cargoBackend = trimmed.match(
      /cargo\s+run\b[^&\n]*--bin\s+cc-partner-backend\s+--\s+([a-zA-Z][a-zA-Z0-9_-]*)/,
    );
    if (cargoBackend && !allow.cliSubcommands.has(cargoBackend[1])) {
      findings.push(
        finding(
          relFile,
          i + 1,
          `README cargo backend subcommand not in CLI allowlist: ${cargoBackend[1]}`,
        ),
      );
    }

    // 明确禁止的浮动 runner
    if (/\bnpx\s+--yes\b/.test(trimmed)) {
      findings.push(
        finding(relFile, i + 1, 'README must not recommend `npx --yes` floating installs'),
      );
    }
  }

  return findings;
}

/**
 * 判断路径是否应跳过（superpowers 历史记录）。
 * @param {string} relPath
 */
function shouldSkipPath(relPath) {
  const n = normalizeRel(relPath);
  return n.startsWith('docs/superpowers/');
}

/**
 * 列出 git 跟踪的 Markdown（排除 docs/superpowers/** 与 node_modules 等）。
 * @returns {string[]}
 */
function listTrackedMarkdown() {
  const ls = spawnSync('git', ['-C', repoRoot, 'ls-files', '*.md', '**/*.md'], {
    encoding: 'utf8',
  });
  if (ls.status !== 0 || !ls.stdout.trim()) {
    return [];
  }
  return ls.stdout
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean)
    .filter((p) => !shouldSkipPath(p))
    .filter((p) => !normalizeRel(p).includes('node_modules/'));
}

/**
 * 解析检查目标：显式参数优先；无参数时扫描全部 tracked Markdown（排除 superpowers）。
 * @param {string[]} args
 * @returns {string[]} rel paths
 */
function resolveTargets(args) {
  if (args.length > 0) {
    return args.map((a) => normalizeRel(a));
  }
  return listTrackedMarkdown();
}

/**
 * 检查单个 Markdown 文件。
 * @param {string} relFile
 * @param {Map<string, Map<string, number>>} slugIndex
 * @param {ReturnType<typeof loadCommandAllowlist>} allow
 * @param {{ softMissing?: boolean }} [opts]
 * @returns {Finding[]}
 */
function checkFile(relFile, slugIndex, allow, opts = {}) {
  if (shouldSkipPath(relFile)) return [];

  const abs = join(repoRoot, relFile);
  if (!existsSync(abs)) {
    // 默认文档集合中 T5 尚未落地的 development 指南：跳过而非失败，便于 T6 并行合入。
    if (opts.softMissing) {
      console.warn(`check-docs: skip missing optional target ${relFile}`);
      return [];
    }
    return [finding(relFile, 1, `file not found: ${relFile}`)];
  }

  const content = readFileSync(abs, 'utf8');
  const { stripped, fenceFindings, lineMap } = stripFences(content, relFile);
  const slugs = collectHeadingSlugs(stripped, lineMap);
  slugIndex.set(normalizeRel(relFile), slugs);

  /** @type {Finding[]} */
  const findings = [...fenceFindings];
  findings.push(...checkLinks(abs, relFile, stripped, lineMap, slugIndex));
  findings.push(...checkStaleClaims(relFile, content));
  findings.push(...checkReadmeCommands(relFile, content, allow));
  const knownIds =
    opts.knownEvidenceIds instanceof Set
      ? opts.knownEvidenceIds
      : loadKnownEvidenceIds(repoRoot);
  findings.push(...checkEvidenceIdRefs(relFile, content, knownIds));
  return findings;
}

/**
 * 两轮检查：先建 slug 索引，再校验交叉锚点。
 * CLI 源缺失/解析失败作为全局 finding 直接失败。
 * @param {string[]} targets
 * @returns {Finding[]}
 */
function checkAll(targets) {
  const allow = loadCommandAllowlist();
  /** @type {Finding[]} */
  const all = [];

  if (allow.cliError) {
    all.push(finding(CLI_SOURCE_REL, 1, allow.cliError));
  }

  /** @type {Map<string, Map<string, number>>} */
  const slugIndex = new Map();
  const knownEvidenceIds = loadKnownEvidenceIds(repoRoot);

  // 第一轮：索引标题
  for (const rel of targets) {
    if (shouldSkipPath(rel)) continue;
    const abs = join(repoRoot, rel);
    if (!existsSync(abs)) continue;
    const content = readFileSync(abs, 'utf8');
    const { stripped, lineMap } = stripFences(content, rel);
    slugIndex.set(normalizeRel(rel), collectHeadingSlugs(stripped, lineMap));
  }

  for (const rel of targets) {
    all.push(...checkFile(rel, slugIndex, allow, { knownEvidenceIds }));
  }
  return all;
}

/**
 * 打印 findings 并以非零退出。
 * @param {Finding[]} findings
 */
function report(findings) {
  if (findings.length === 0) {
    console.log('check-docs: ok');
    return 0;
  }
  for (const f of findings) {
    console.error(`${f.file}:${f.line}: ${f.message}`);
  }
  console.error(`check-docs: ${findings.length} issue(s)`);
  return 1;
}

/**
 * 在临时目录跑 fixture 自测（不依赖仓库当前文档是否干净）。
 * Business Logic: Step 1 要求先写 fixture 再实现；CI/本地用 --self-test 回归守卫本身。
 * Code Logic: 写临时 md，spawn 本脚本检查，断言 exit code 与诊断。
 */
function runSelfTest() {
  const dir = mkdtempSync(join(tmpdir(), 'check-docs-'));
  /** @type {string[]} */
  const failures = [];

  /**
   * @param {string} name
   * @param {Record<string, string>} files
   * @param {{ expectFail: boolean, mustMatch?: RegExp[] }} opts
   */
  function caseRun(name, files, opts) {
    const caseDir = join(dir, name);
    mkdirSync(caseDir, { recursive: true });
    // 构造最小假仓库根：复制脚本 + 白名单源
    writeFileSync(
      join(caseDir, 'package-root-marker'),
      'fixture',
      'utf8',
    );
    mkdirSync(join(caseDir, 'scripts'), { recursive: true });
    // 使用真实脚本，但把 CWD/repoRoot 切到 fixture：通过环境变量不方便，
    // 改为在 fixture 内写独立 mini checker harness 调用导出逻辑太重；
    // 直接在本进程内对临时文件跑核心检查函数。
    for (const [rel, body] of Object.entries(files)) {
      const abs = join(caseDir, rel);
      mkdirSync(dirname(abs), { recursive: true });
      writeFileSync(abs, body, 'utf8');
    }

    // 在 caseDir 上局部检查：临时切换 repoRoot 不可变，故内联简化校验
    const findings = checkFixtureTree(caseDir, Object.keys(files));
    const failed = findings.length > 0;
    if (opts.expectFail && !failed) {
      failures.push(`${name}: expected failure, got ok`);
      return;
    }
    if (!opts.expectFail && failed) {
      failures.push(
        `${name}: expected ok, got:\n  ${findings.map((f) => `${f.file}:${f.line}: ${f.message}`).join('\n  ')}`,
      );
      return;
    }
    if (opts.mustMatch) {
      const blob = findings.map((f) => `${f.file}:${f.line}: ${f.message}`).join('\n');
      for (const re of opts.mustMatch) {
        if (!re.test(blob)) {
          failures.push(`${name}: diagnostics missing ${re}:\n  ${blob}`);
        }
      }
    }
    // 可读 file:line
    if (failed) {
      for (const f of findings) {
        if (!f.file || !f.line || !f.message) {
          failures.push(`${name}: unreadable diagnostic ${JSON.stringify(f)}`);
        }
      }
    }
  }

  // 有效：相对文件链接
  caseRun(
    'valid-file-link',
    {
      'README.md':
        '# Title\n\nSee [prd](docs/prd.md).\n\nFirewall: UDP 5353 and TCP 62116.\n',
      'docs/prd.md':
        '# PRD\n\n同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。\n',
    },
    { expectFail: false },
  );

  // 有效：同文件 / 跨文件锚点
  caseRun(
    'valid-anchors',
    {
      'README.md':
        '# Title\n\n## Install\n\nJump [down](#install).\n\nSee [prd section](docs/prd.md#features).\n\nFirewall: UDP 5353 + actual TCP port 62116.\n',
      'docs/prd.md':
        '# PRD\n\n## Features\n\n同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。\n',
    },
    { expectFail: false },
  );

  // 无效：缺文件
  caseRun(
    'missing-file',
    {
      'README.md': '# T\n\n[x](docs/nope.md)\n',
    },
    {
      expectFail: true,
      mustMatch: [/README\.md:\d+:.*broken relative link/],
    },
  );

  // 无效：缺锚点
  caseRun(
    'missing-anchor',
    {
      'README.md': '# T\n\n## Install\n\n[x](#missing-heading)\n',
    },
    {
      expectFail: true,
      mustMatch: [/README\.md:\d+:.*missing heading anchor/],
    },
  );

  // 忽略 http / mailto
  caseRun(
    'ignore-external',
    {
      'README.md':
        '# T\n\n[a](https://example.com/x)\n[b](mailto:a@b.c)\n\nFirewall: UDP 5353 and TCP 62116.\n',
    },
    { expectFail: false },
  );

  // 无效：未闭合围栏
  caseRun(
    'unbalanced-fence',
    {
      'README.md': '# T\n\n```bash\necho hi\n',
    },
    {
      expectFail: true,
      mustMatch: [/README\.md:\d+:.*unbalanced fenced/],
    },
  );

  // 无效：README tauri-action
  caseRun(
    'banned-tauri-action',
    {
      'README.md': '# T\n\nBuilt with tauri-action.\n',
    },
    {
      expectFail: true,
      mustMatch: [/README\.md:\d+:.*tauri-action/],
    },
  );

  // 无效：README 动态端口
  caseRun(
    'banned-dynamic-port',
    {
      'README.md': '# T\n\naxum HTTP server（动态端口）\n',
    },
    {
      expectFail: true,
      mustMatch: [/README\.md:\d+:.*动态端口/],
    },
  );

  // 无效：web/CLAUDE.md npx --yes tsx
  caseRun(
    'banned-npx-yes-tsx',
    {
      'web/CLAUDE.md': '# web\n\nRun `npx --yes tsx src/foo.test.ts`.\n',
    },
    {
      expectFail: true,
      mustMatch: [/web\/CLAUDE\.md:\d+:.*npx --yes tsx/],
    },
  );

  // 无效：docs 宣称 smoke 覆盖 WSL
  caseRun(
    'banned-smoke-overclaim',
    {
      'docs/development/testing.md':
        '# Testing\n\nHosted smoke verifies WSL+tmux on CI runners.\n',
    },
    {
      expectFail: true,
      mustMatch: [/docs\/development\/testing\.md:\d+:/],
    },
  );

  // superpowers 历史文本不因 tauri-action 失败（不在检查集合时）
  caseRun(
    'superpowers-skipped-when-not-listed',
    {
      'README.md':
        '# T\n\nOk preferred port 62116.\n\nFirewall: UDP 5353 and actual TCP port.\n',
      'docs/superpowers/plans/old.md': '# Old\n\ntauri-action dynamic stuff\n',
    },
    { expectFail: false },
  );

  // 无效：文档误写 encode_mdns_capabilities 输出始终带 caps= 前缀
  caseRun(
    'banned-mdns-caps-prefix',
    {
      'src-tauri/CLAUDE.md':
        '# backend\n\n`encode_mdns_capabilities` 输出始终以 `caps=` 前缀开头，按 UTF-8 字节计数。\n',
    },
    {
      expectFail: true,
      mustMatch: [/src-tauri\/CLAUDE\.md:\d+:.*encode_mdns_capabilities|bare comma-separated/],
    },
  );

  // 无效：模糊“可信局域网” + authenticated LAN peers + LAN mode + lan.permission
  caseRun(
    'banned-lan-trust-modes',
    {
      'README.md':
        '# T\n\n面向个人可信局域网。Authenticated LAN peers are trusted devices.\nLAN mode: read-only. Capability: lan.permission.\n',
    },
    {
      expectFail: true,
      mustMatch: [
        /README\.md:\d+:.*(可信局域网|authenticated|LAN mode|lan\.permission|trusted)/i,
      ],
    },
  );

  // 无效：docs/prd.md 缺少固定无身份风险声明
  caseRun(
    'missing-fixed-risk-prd',
    {
      'docs/prd.md':
        '# PRD\n\nMobile Workbench uses LAN HTTP without tokens.\n',
    },
    {
      expectFail: true,
      mustMatch: [/docs\/prd\.md:\d+:.*fixed no-identity LAN risk/],
    },
  );

  // 无效：backend-operations 缺少固定风险声明
  caseRun(
    'missing-fixed-risk-ops',
    {
      'docs/development/backend-operations.md':
        '# Backend Operations\n\nFirewall: UDP 5353 and TCP 62116.\n',
    },
    {
      expectFail: true,
      mustMatch: [
        /docs\/development\/backend-operations\.md:\d+:.*fixed no-identity LAN risk/,
      ],
    },
  );

  // 有效：固定风险 + 无 mode 语言 + 防火墙端口齐全
  caseRun(
    'valid-fixed-lan-boundary',
    {
      'docs/prd.md': [
        '# PRD',
        '',
        '同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。',
        '',
        '产品只有一种固定局域网行为，不提供可切换暴露或只读产品模式。',
        '',
      ].join('\n'),
      'docs/development/backend-operations.md': [
        '# Backend Operations',
        '',
        'Any device on the same reachable network can read, write, and execute; the system does not verify caller identity.',
        '',
        'Firewall: allow UDP 5353 and the actual TCP port (preferred 62116, increment when occupied).',
        '',
      ].join('\n'),
      'README.md': [
        '# T',
        '',
        'Local/LAN only. No caller identity checks.',
        '',
        'Firewall: UDP 5353 + actual TCP port (preferred 62116).',
        '',
      ].join('\n'),
    },
    { expectFail: false },
  );

  // evidence ID：文档引用不存在的 E2E/L2/L3 ID → 失败
  {
    const name = 'unknown-evidence-id';
    const known = new Set(['E2E-TRANSFER-001', 'L2-LAN-TRUST-BOUNDARY-001']);
    const findings = checkEvidenceIdRefs(
      'docs/development/testing.md',
      [
        '# Testing',
        '',
        'Covered: E2E-TRANSFER-001 and L2-LAN-TRUST-BOUNDARY-001.',
        'Missing: E2E-NOT-REGISTERED-001 should fail.',
        '',
      ].join('\n'),
      known,
    );
    if (findings.length === 0) {
      failures.push(`${name}: expected failure for unknown evidence id`);
    } else {
      const blob = findings.map((f) => f.message).join('\n');
      if (!/E2E-NOT-REGISTERED-001/.test(blob)) {
        failures.push(`${name}: diagnostics missing unknown id:\n  ${blob}`);
      }
      if (/E2E-TRANSFER-001/.test(blob)) {
        failures.push(`${name}: known id should not fail:\n  ${blob}`);
      }
    }
  }

  // evidence ID：已知 ID 通过
  {
    const name = 'known-evidence-id';
    const known = new Set(['E2E-TRANSFER-001', 'L3-DUAL-HOST-LAN-001']);
    const findings = checkEvidenceIdRefs(
      'docs/development/testing.md',
      'See E2E-TRANSFER-001 and L3-DUAL-HOST-LAN-001.\n',
      known,
    );
    if (findings.length !== 0) {
      failures.push(
        `${name}: expected ok, got ${findings.map((f) => f.message).join('; ')}`,
      );
    }
  }

  // CLI：README 推荐已从 dispatch 删除的子命令 → 失败
  {
    const name = 'cli-deleted-command';
    const caseDir = join(dir, name);
    mkdirSync(join(caseDir, 'src-tauri/src/backend'), { recursive: true });
    writeFileSync(
      join(caseDir, 'src-tauri/src/backend/cli.rs'),
      [
        'fn dispatch(args: Vec<String>) -> i32 {',
        '    let command = args.get(1).map(String::as_str);',
        '    match command {',
        '        Some("start") => 0,',
        '        Some("status") => 0,',
        '        _ => 2,',
        '    }',
        '}',
        '',
      ].join('\n'),
      'utf8',
    );
    writeFileSync(
      join(caseDir, 'README.md'),
      '# T\n\n```bash\ncc-partner-backend doctor\n```\n',
      'utf8',
    );
    const parsed = parseCliSubcommandsFromSource(
      join(caseDir, 'src-tauri/src/backend/cli.rs'),
    );
    if (!parsed.ok) {
      failures.push(`${name}: expected parse ok, got ${parsed.error}`);
    } else {
      const allow = {
        npmScripts: new Set(),
        cliSubcommands: parsed.commands,
        rootScripts: new Set(),
        startModes: new Set(),
        cliError: null,
      };
      const findings = checkReadmeCommands(
        'README.md',
        readFileSync(join(caseDir, 'README.md'), 'utf8'),
        allow,
      );
      if (findings.length === 0) {
        failures.push(`${name}: expected failure for deleted doctor subcommand`);
      } else {
        const blob = findings.map((f) => f.message).join('\n');
        if (!/doctor/.test(blob)) {
          failures.push(`${name}: diagnostics missing doctor:\n  ${blob}`);
        }
      }
    }
  }

  // CLI：源文件缺失 → parse 失败
  {
    const name = 'cli-source-missing';
    const missing = join(dir, name, 'no-such-cli.rs');
    const parsed = parseCliSubcommandsFromSource(missing);
    if (parsed.ok) {
      failures.push(`${name}: expected parse failure for missing source`);
    } else if (!/missing/i.test(parsed.error)) {
      failures.push(`${name}: expected missing error, got ${parsed.error}`);
    }
  }

  // CLI：dispatch arms 为空 → parse 失败
  {
    const name = 'cli-empty-dispatch';
    const caseDir = join(dir, name);
    mkdirSync(caseDir, { recursive: true });
    const abs = join(caseDir, 'cli.rs');
    writeFileSync(
      abs,
      'fn dispatch() -> i32 {\n    match command {\n        _ => 2,\n    }\n}\n',
      'utf8',
    );
    const parsed = parseCliSubcommandsFromSource(abs);
    if (parsed.ok) {
      failures.push(`${name}: expected parse failure for empty arms`);
    } else if (!/zero subcommands|not found/i.test(parsed.error)) {
      failures.push(`${name}: unexpected error ${parsed.error}`);
    }
  }

  rmSync(dir, { recursive: true, force: true });

  if (failures.length > 0) {
    console.error('check-docs --self-test FAILED:');
    for (const f of failures) console.error(` - ${f}`);
    return 1;
  }
  console.log('check-docs --self-test: ok');
  return 0;
}

/**
 * 对临时 fixture 树运行与生产相同的规则（repoRoot 固定为真实仓库时，
 * 链接目标相对于 fixture 文件目录解析——checkLinks 用 abs 文件目录，
 * 故相对链接在 fixture 内自洽；陈旧规则只看 relPath 与内容）。
 *
 * 注意：checkFile 用全局 repoRoot join relFile，因此 fixture 自测不能走 checkFile。
 * 这里复制核心路径：围栏 / 链接（以 caseDir 为根）/ 陈旧表述。
 *
 * @param {string} caseDir
 * @param {string[]} relFiles
 * @returns {Finding[]}
 */
function checkFixtureTree(caseDir, relFiles) {
  /** @type {Finding[]} */
  const findings = [];
  /** @type {Map<string, Map<string, number>>} */
  const slugIndex = new Map();

  const targets = relFiles.filter((p) => !shouldSkipPath(p));

  for (const rel of targets) {
    const abs = join(caseDir, rel);
    if (!existsSync(abs)) {
      findings.push(finding(rel, 1, `file not found: ${rel}`));
      continue;
    }
    const content = readFileSync(abs, 'utf8');
    const { stripped, fenceFindings, lineMap } = stripFences(content, rel);
    findings.push(...fenceFindings);
    slugIndex.set(normalizeRel(rel), collectHeadingSlugs(stripped, lineMap));
  }

  for (const rel of targets) {
    const abs = join(caseDir, rel);
    if (!existsSync(abs)) continue;
    const content = readFileSync(abs, 'utf8');
    const { stripped, lineMap } = stripFences(content, rel);
    // 链接：以 caseDir 为“仓库根”
    findings.push(
      ...checkLinksWithRoot(caseDir, abs, rel, stripped, lineMap, slugIndex),
    );
    findings.push(...checkStaleClaims(rel, content));
  }

  return findings;
}

/**
 * 与 checkLinks 相同，但仓库根可注入（供 fixture）。
 * @param {string} root
 * @param {string} absFile
 * @param {string} relFile
 * @param {string} stripped
 * @param {number[]} lineMap
 * @param {Map<string, Map<string, number>>} slugIndex
 */
function checkLinksWithRoot(root, absFile, relFile, stripped, lineMap, slugIndex) {
  /** @type {Finding[]} */
  const findings = [];
  const links = extractLinks(stripped, lineMap);
  const fileDir = dirname(absFile);
  const ownSlugs = slugIndex.get(normalizeRel(relFile)) ?? new Map();

  for (const { href, line } of links) {
    if (shouldIgnoreHref(href)) continue;
    let pathPart = href;
    let hash = '';
    const hashIdx = href.indexOf('#');
    if (hashIdx >= 0) {
      pathPart = href.slice(0, hashIdx);
      hash = href.slice(hashIdx + 1);
    }
    if (!pathPart) {
      if (hash && !ownSlugs.has(hash) && !ownSlugs.has(decodeURIComponent(hash))) {
        findings.push(finding(relFile, line, `missing heading anchor #${hash}`));
      }
      continue;
    }
    if (isAbsolute(pathPart) || /^[a-zA-Z]:[\\/]/.test(pathPart)) {
      findings.push(finding(relFile, line, `absolute path link not allowed: ${pathPart}`));
      continue;
    }
    const targetAbs = resolve(fileDir, pathPart);
    const targetRel = normalizeRel(relative(root, targetAbs));
    if (targetRel.startsWith('..')) {
      findings.push(finding(relFile, line, `link escapes repository: ${pathPart}`));
      continue;
    }
    if (!existsSync(targetAbs)) {
      findings.push(finding(relFile, line, `broken relative link: ${pathPart}`));
      continue;
    }
    let st;
    try {
      st = statSync(targetAbs);
    } catch {
      findings.push(finding(relFile, line, `broken relative link: ${pathPart}`));
      continue;
    }
    if (st.isDirectory()) continue;
    if (hash) {
      let targetSlugs = slugIndex.get(targetRel);
      if (!targetSlugs) {
        try {
          const c = readFileSync(targetAbs, 'utf8');
          const s = stripFences(c, targetRel);
          targetSlugs = collectHeadingSlugs(s.stripped, s.lineMap);
        } catch {
          targetSlugs = new Map();
        }
      }
      if (!targetSlugs.has(hash) && !targetSlugs.has(decodeURIComponent(hash))) {
        findings.push(
          finding(relFile, line, `missing heading anchor ${pathPart}#${hash}`),
        );
      }
    }
  }
  return findings;
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    process.exitCode = runSelfTest();
    return;
  }

  const files = argv.filter((a) => a !== '--');
  const targets = resolveTargets(files);
  if (targets.length === 0) {
    console.error('check-docs: no markdown targets');
    process.exitCode = 1;
    return;
  }

  // 显式传入但不存在的文件 → 报错
  const findings = checkAll(targets);
  process.exitCode = report(findings);
}

main();
