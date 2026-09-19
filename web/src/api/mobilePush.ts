/**
 * 移动推送中转配置 API。
 *
 * Business Logic（为什么需要）:
 *   桌面 Settings 需要配置 APNs/FCM 中转 URL，PC 才能在 Attention 变化时通知手机。
 *
 * Code Logic（做什么）:
 *   invoke get/update_mobile_push_config；token 只在写入时提交，读取只返回是否已配置。
 */
import { invoke } from './client';

export interface MobilePushConfig {
  relayUrl: string;
  relayTokenConfigured: boolean;
}

export const mobilePushApi = {
  get: () => invoke<MobilePushConfig>('get_mobile_push_config'),
  update: (relayUrl: string, relayToken?: string) =>
    invoke<MobilePushConfig>('update_mobile_push_config', {
      relayUrl,
      relayToken: relayToken ?? null,
    }),
};
