/**
 * check-i18n-jsx.mjs — 生产 TSX 用户可见文案 i18n 合同守卫。
 *
 * Business Logic（为什么需要）:
 *   组件里硬编码中/英文会在语言切换时残留错误语言，且难以审计。
 *   需要 CI 可执行的 AST 合同：JSXText 与 title/aria-label/placeholder/alt
 *   字面量必须走 t() 或有限品牌/技术词 allowlist，禁止无限 baseline。
 *
 * Code Logic（做什么）:
 *   用 typescript compiler API 解析 .tsx，扫描 JsxText 与目标属性的字符串字面量；
 *   含中文或拉丁字母的文本若不在 ALLOWED_LITERAL_COPY 则报 `file:line:column`；
 *   排除 .test/.stories、DesignSystem；导出 analyze 与 shouldScan 供 fixture 测试；
 *   CLI 扫描 web/src，exit 0/1。
 *
 * Usage:
 *   node scripts/check-i18n-jsx.mjs
 *   npm run check:i18n
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

/**
 * 允许以字面量出现在生产 TSX 中的品牌/技术词（有限集合，禁止无限增长 baseline）。
 * 纯标点/符号不在此集合，由 isLetterful 过滤放行。
 */
export const ALLOWED_LITERAL_COPY = Object.freeze([
  'cc-partner',
  'Claude Code',
  'Git',
  'GitHub',
  'HTML',
  'JSON',
  'SQLite',
  'tmux',
  'WSL',
]);

const ALLOWED_SET = new Set(ALLOWED_LITERAL_COPY);

/** 需检查的 JSX 属性名（仅字符串字面量）。 */
const LITERAL_ATTRS = new Set(['title', 'aria-label', 'placeholder', 'alt']);

/** 匹配中文或拉丁字母（用户可见文案信号）。 */
const LETTERFUL_RE = /[A-Za-z一-鿿]/;

/**
 * 判断文本是否包含需本地化的字母内容。
 *
 * Business Logic:
 *   纯符号（✓ · —）可直接作 UI chrome；含中文/拉丁字母的才是文案。
 *
 * Code Logic:
 *   对 trim 后文本做 LETTERFUL_RE 测试。
 *
 * @param {string} text
 * @returns {boolean}
 */
export function isLetterfulCopy(text) {
  return LETTERFUL_RE.test(text.trim());
}

/**
 * 判断字面量是否允许保留。
 *
 * Business Logic:
 *   品牌与技术词在 en/zh 中通常保持原文，不应强制进 locale 文件。
 *
 * Code Logic:
 *   空白 / 非 letterful / allowlist 精确匹配 → 允许。
 *
 * @param {string} text
 * @returns {boolean}
 */
export function isAllowedLiteral(text) {
  const trimmed = text.trim();
  if (!trimmed) return true;
  if (!isLetterfulCopy(trimmed)) return true;
  return ALLOWED_SET.has(trimmed);
}

/**
 * 判断相对路径是否应纳入生产 i18n 扫描。
 *
 * Business Logic:
 *   测试、故事、DesignSystem 与生成声明不承载产品文案合同。
 *
 * Code Logic:
 *   仅 .tsx；排除 .test. / .stories. / DesignSystem 路径段。
 *
 * @param {string} relativePath posix 风格相对路径
 * @returns {boolean}
 */
export function shouldScanTsxPath(relativePath) {
  const normalized = relativePath.split('\\').join('/');
  if (!normalized.endsWith('.tsx')) return false;
  if (normalized.includes('.test.') || normalized.includes('.stories.')) return false;
  // 测试 harness / fixtures 目录不承载产品文案合同
  if (normalized.includes('/testing/') || normalized.startsWith('testing/')) {
    return false;
  }
  if (
    normalized.includes('/DesignSystem/') ||
    normalized.startsWith('DesignSystem/') ||
    normalized.includes('/pages/DesignSystem/')
  ) {
    return false;
  }
  return true;
}

/**
 * 从 AST 节点取 1-based 行列。
 *
 * Business Logic:
 *   CI 诊断需要可跳转的位置。
 *
 * Code Logic:
 *   sourceFile.getLineAndCharacterOfPosition + 1。
 *
 * @param {ts.SourceFile} sourceFile
 * @param {number} pos
 * @returns {{ line: number, column: number }}
 */
function getLocation(sourceFile, pos) {
  const { line, character } = sourceFile.getLineAndCharacterOfPosition(pos);
  return { line: line + 1, column: character + 1 };
}

/**
 * 分析单个 TSX 源文本的 i18n 违规。
 *
 * Business Logic:
 *   把「硬编码用户文案」转成稳定、可 CI 失败的诊断列表。
 *
 * Code Logic:
 *   createSourceFile(TSX) 后遍历 JsxText 与目标 JsxAttribute 的 StringLiteral；
 *   报告格式 `file:line:column`。
 *
 * @param {string} filePath 用于诊断的相对路径
 * @param {string} sourceText
 * @returns {string[]}
 */
export function analyzeTsxI18nLiterals(filePath, sourceText) {
  /** @type {string[]} */
  const diagnostics = [];
  const sourceFile = ts.createSourceFile(
    filePath,
    sourceText,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );

  /**
   * 记录一条违规。
   *
   * @param {number} pos
   * @param {string} _kind
   * @param {string} _sample
   */
  function report(pos, _kind, _sample) {
    const { line, column } = getLocation(sourceFile, pos);
    diagnostics.push(`${filePath}:${line}:${column}`);
  }

  /**
   * 递归遍历 AST。
   *
   * @param {ts.Node} node
   */
  function visit(node) {
    if (ts.isJsxText(node)) {
      const text = node.getText(sourceFile);
      if (!isAllowedLiteral(text)) {
        // 跳过纯空白节点；取首个非空白字符位置
        const full = text;
        const lead = full.match(/^\s*/)?.[0]?.length ?? 0;
        report(node.getStart(sourceFile) + lead, 'JSXText', full.trim());
      }
    } else if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(sourceFile);
      if (LITERAL_ATTRS.has(name) && node.initializer) {
        if (ts.isStringLiteral(node.initializer)) {
          const value = node.initializer.text;
          if (!isAllowedLiteral(value)) {
            report(node.initializer.getStart(sourceFile), name, value);
          }
        } else if (
          ts.isJsxExpression(node.initializer) &&
          node.initializer.expression &&
          ts.isStringLiteral(node.initializer.expression)
        ) {
          const value = node.initializer.expression.text;
          if (!isAllowedLiteral(value)) {
            report(node.initializer.expression.getStart(sourceFile), name, value);
          }
        }
      }
    }

    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  return diagnostics;
}

/**
 * 批量分析多个文件。
 *
 * Business Logic:
 *   测试与 CLI 共用同一聚合入口。
 *
 * Code Logic:
 *   仅扫描 shouldScanTsxPath 为真的路径。
 *
 * @param {Array<{ path: string, content: string }>} files
 * @returns {string[]}
 */
export function analyzeI18nJsxContract(files) {
  /** @type {string[]} */
  const diagnostics = [];
  for (const file of files) {
    if (!shouldScanTsxPath(file.path)) continue;
    diagnostics.push(...analyzeTsxI18nLiterals(file.path, file.content));
  }
  return diagnostics;
}

/**
 * 递归收集目录下全部 .tsx 文件路径。
 *
 * Business Logic:
 *   CLI 需要对前端源码中所有生产 TSX 生效。
 *
 * Code Logic:
 *   深度优先，跳过 node_modules/dist。
 *
 * @param {string} dir
 * @returns {string[]}
 */
function listTsxFiles(dir) {
  /** @type {string[]} */
  const out = [];
  for (const entry of readdirSync(dir)) {
    if (entry === 'node_modules' || entry === 'dist') continue;
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      out.push(...listTsxFiles(full));
    } else if (entry.endsWith('.tsx')) {
      out.push(full);
    }
  }
  return out;
}

/**
 * CLI 入口：扫描 web/src 并打印诊断。
 *
 * Business Logic:
 *   给 npm script / CI 提供 exit code 语义。
 *
 * Code Logic:
 *   读取全部 .tsx，analyzeI18nJsxContract；无诊断打印成功文案 exit 0。
 *
 * @returns {number}
 */
function main() {
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const webRoot = resolve(scriptDir, '..');
  const srcRoot = resolve(webRoot, 'src');

  const files = listTsxFiles(srcRoot).map((absPath) => ({
    path: relative(webRoot, absPath).split('\\').join('/'),
    content: readFileSync(absPath, 'utf8'),
  }));

  const diagnostics = analyzeI18nJsxContract(files);
  if (diagnostics.length === 0) {
    console.log('i18n JSX contract passed');
    return 0;
  }
  for (const d of diagnostics) {
    console.error(d);
  }
  return 1;
}

const isDirectRun =
  process.argv[1] &&
  fileURLToPath(import.meta.url) === resolve(process.argv[1]);

if (isDirectRun) {
  process.exit(main());
}
