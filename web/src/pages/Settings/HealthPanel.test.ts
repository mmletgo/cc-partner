/**
 * normalizeTimeDraft 归一化回归测试(脚本式,沿用 settingsState.test.ts 风格)
 *
 * Business Logic（为什么需要这个测试）:
 *   免打扰时间输入草稿归一化是健康 tab 免打扰字段提交的关键纯函数,
 *   需脱离测试框架用脚本式直接验证,便于 `npx tsx` 直接跑(0 退出码=全过)。
 *
 * Code Logic（做什么）:
 *   先注册 css-stub loader(HealthPanel.tsx 经 @/components/primitives 间接 import *.module.css,
 *   tsx 无 CSS loader,需 stub 成空对象);再动态 import HealthPanel 取 normalizeTimeDraft,
 *   遍历 9 组「输入→期望」用例,结果严格不等则抛错让 node 进程非零退出。
 *   node:module 这一行用 @ts-expect-error 抑制类型错误(见下方行内注释)。
 */

// node:module 类型由 @types/node 提供,但本仓库 tsconfig 未在 compilerOptions.types 显式纳入 node,
// tsx 测试上下文下类型缺失,故局部抑制(运行时 tsx 正常解析;node:module 是 node 内置,无需安装)。
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:module 类型缺失,运行时 tsx 正常
import { register } from 'node:module';
register('./css-stub.mjs', import.meta.url);

const { normalizeTimeDraft } = await import('./HealthPanel');

const cases: Array<[string, string | null | undefined]> = [
  ['', null],
  ['09:30', '09:30'],
  ['9:30', '09:30'],
  ['0930', '09:30'],
  ['930', '09:30'],
  ['23:59', '23:59'],
  ['25:00', undefined],
  ['12:60', undefined],
  ['abc', undefined],
];

for (const [input, expected] of cases) {
  const actual = normalizeTimeDraft(input);
  if (actual !== expected) {
    throw new Error(
      `normalizeTimeDraft('${input}') expected ${String(expected)}, got ${String(actual)}`,
    );
  }
}

console.log(`normalizeTimeDraft: ${cases.length} cases passed`);
