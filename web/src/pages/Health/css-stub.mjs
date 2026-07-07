/**
 * CSS 模块 stub loader —— 仅用于 tsx 直接跑脚本式测试时拦截 `.css` import。
 *
 * Business Logic（为什么需要这个 loader）:
 *   HabitStatsCard.tsx 直接 import *.module.css;tsx 无 CSS loader,
 *   直接 npx tsx 跑测试会 ERR_UNKNOWN_FILE_EXTENSION。
 *   需要让 className 表达式(styles.bar 等)在 SSR 渲染时得到"类名本身"字符串,
 *   以便测试用 class 正则断言 7 柱 sparkline / 今日高亮等结构;
 *   不影响 vite 构建时的真实 CSS(构建走真实 CSS Modules,哈希类名)。
 *
 * Code Logic（做什么）:
 *   ESM load hook:以 .css 结尾的 URL 返回模块源,default 导出一个 Proxy,
 *   任意属性访问返回属性名字符串(模拟 CSS Modules 的 local name 映射),
 *   其余 URL 交给下一级 loader。
 *   仅被 HabitStatsCard.test.ts 通过 node:module.register 加载,运行时生效。
 */
export async function load(url, context, nextLoad) {
  if (url.endsWith('.css')) {
    const source =
      'const p = new Proxy({}, { get: (_, k) => (typeof k === "string" ? k : undefined) });\n' +
      'export default p;';
    return { format: 'module', shortCircuit: true, source };
  }
  return nextLoad(url, context);
}
