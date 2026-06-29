import { selectPrimaryMobileUrl } from './mobileQr';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 未启用 Node 类型，测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

assertEqual(
  selectPrimaryMobileUrl(['http://192.168.1.23:51842/mobile']),
  'http://192.168.1.23:51842/mobile',
  'first LAN URL should be selected',
);
assertEqual(
  selectPrimaryMobileUrl(['', '  ', 'http://10.0.0.8:51842/mobile']),
  'http://10.0.0.8:51842/mobile',
  'blank URLs should be skipped',
);
assertEqual(selectPrimaryMobileUrl([]), null, 'empty URL list should return null');

console.log('mobileAccessCard.test.ts passed');
