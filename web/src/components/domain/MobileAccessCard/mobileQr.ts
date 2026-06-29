import QRCode from 'qrcode';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端可能拿到多个局域网 URL，二维码区域需要选择一个主 URL 供手机扫码。
 *
 * Code Logic（这个函数做什么）:
 *   返回第一个非空 URL；没有可用 URL 时返回 null。
 */
export function selectPrimaryMobileUrl(urls: string[]): string | null {
  return urls.find((url) => url.trim().length > 0) ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户希望手机扫码打开移动 Workbench，前端需要把 URL 渲染成二维码。
 *
 * Code Logic（这个函数做什么）:
 *   调用 qrcode 库生成 SVG 字符串，交给组件作为只读 SVG 片段渲染。
 */
export function renderMobileQrSvg(url: string): Promise<string> {
  return QRCode.toString(url, {
    type: 'svg',
    margin: 1,
    width: 180,
  });
}
