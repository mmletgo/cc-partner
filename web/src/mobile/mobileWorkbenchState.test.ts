import {
  closeMobileNav,
  openMobileNav,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assertEqual<T>(actual: T, expected: T): void {
  if (actual !== expected) {
    throw new Error(`Expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench shell 需要稳定切换当前面板，后续业务面板接入时不应破坏默认导航契约。
 *
 * Code Logic（这个函数做什么）:
 *   调用 selectMobilePanel 并断言它总是返回用户下一步选择的面板。
 */
function testSelectMobilePanelReturnsNextPanel(): void {
  const current: MobileWorkbenchPanel = 'projects';
  const next: MobileWorkbenchPanel = 'terminal';

  assertEqual(selectMobilePanel(current, next), next);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机竖屏需要通过顶部按钮打开覆盖式导航抽屉，抽屉状态 helper 必须返回打开态。
 *
 * Code Logic（这个函数做什么）:
 *   调用 openMobileNav 并断言返回 true。
 */
function testOpenMobileNavReturnsTrue(): void {
  assertEqual(openMobileNav(), true);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户选择导航项或点击遮罩后需要关闭移动端抽屉，关闭 helper 必须返回关闭态。
 *
 * Code Logic（这个函数做什么）:
 *   调用 closeMobileNav 并断言返回 false。
 */
function testCloseMobileNavReturnsFalse(): void {
  assertEqual(closeMobileNav(), false);
}

testSelectMobilePanelReturnsNextPanel();
testOpenMobileNavReturnsTrue();
testCloseMobileNavReturnsFalse();
