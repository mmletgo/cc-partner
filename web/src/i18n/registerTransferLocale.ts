/**
 * 传输文案延迟注册。
 *
 * Business Logic（为什么需要这个模块）:
 *   transfer.json 若随 i18n/index 同步 import，会打进 main/mobile initial graph，
 *   撑破 no-growth baseline；传输页与手机传输面板本身已 lazy。
 *
 * Code Logic（这个模块做什么）:
 *   幂等 addResourceBundle(en/zh, transfer)；供 Transfer / TransferItem /
 *   手机传输 controller 在模块求值时调用。
 */
import i18n from 'i18next';
import enTransfer from './locales/en/transfer.json';
import zhTransfer from './locales/zh/transfer.json';

let registered = false;

/**
 * Business Logic（为什么需要这个函数）:
 *   传输 UI 渲染前必须有 transfer namespace，否则 t() 会打出 key 名。
 *
 * Code Logic（这个函数做什么）:
 *   只注册一次 en/zh transfer 资源。
 */
export function registerTransferLocale(): void {
  if (registered) return;
  i18n.addResourceBundle('en', 'transfer', enTransfer, true, true);
  i18n.addResourceBundle('zh', 'transfer', zhTransfer, true, true);
  registered = true;
}
