/**
 * CSS 模块空 stub loader —— 仅用于 tsx 直接跑脚本式测试时拦截 `.css` import。
 *
 * Business Logic（为什么需要这个 loader）:
 *   HealthPanel.tsx 通过 @/components/primitives 间接 import *.module.css;
 *   tsx 无 CSS loader,直接 npx tsx 跑测试会 ERR_UNKNOWN_FILE_EXTENSION。
 *   用空对象 stub 让纯函数回归测试能跑通,不影响 vite 构建时的真实 CSS。
 *
 * Code Logic（做什么）:
 *   ESM load hook:以 .css 结尾的 URL 返回空模块源,其余交给下一级 loader。
 *   仅被 HealthPanel.test.ts 通过 node:module.register 加载,运行时生效。
 */
export async function load(url, context, nextLoad) {
  if (url.endsWith('.css')) {
    return { format: 'module', shortCircuit: true, source: 'export default {};' };
  }
  return nextLoad(url, context);
}
