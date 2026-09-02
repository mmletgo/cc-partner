/**
 * 手机邮箱虚拟目标常量与判定。
 *
 * Business Logic（为什么需要这个测试）:
 *   前后端 id 必须一致；Download 资格不得把普通 Send 当成 inbox offer。
 *
 * Code Logic（这个测试做什么）:
 *   锁定常量字符串，覆盖 device/offer 真假值。
 */

import { describe, expect, test } from 'vitest';
import {
  MOBILE_INBOX_DEVICE_ID,
  buildMobileInboxDevice,
  isMobileInboxDevice,
  isMobileInboxOffer,
} from './mobileInbox';

describe('mobileInbox', () => {
  test('device id matches backend constant', () => {
    expect(MOBILE_INBOX_DEVICE_ID).toBe('cc-partner-mobile-inbox');
    expect(isMobileInboxDevice(MOBILE_INBOX_DEVICE_ID)).toBe(true);
    expect(isMobileInboxDevice('peer-1')).toBe(false);
    expect(isMobileInboxDevice(null)).toBe(false);
  });

  test('offer is send+completed+inbox peer only', () => {
    expect(
      isMobileInboxOffer({
        direction: 'send',
        status: 'completed',
        peerDeviceId: MOBILE_INBOX_DEVICE_ID,
      }),
    ).toBe(true);
    expect(
      isMobileInboxOffer({
        direction: 'receive',
        status: 'completed',
        peerDeviceId: MOBILE_INBOX_DEVICE_ID,
      }),
    ).toBe(false);
    expect(
      isMobileInboxOffer({
        direction: 'send',
        status: 'completed',
        peerDeviceId: 'peer-1',
      }),
    ).toBe(false);
    expect(
      isMobileInboxOffer({
        direction: 'send',
        status: 'failed',
        peerDeviceId: MOBILE_INBOX_DEVICE_ID,
      }),
    ).toBe(false);
  });

  test('synthetic device has no fake address', () => {
    const device = buildMobileInboxDevice('手机');
    expect(device.id).toBe(MOBILE_INBOX_DEVICE_ID);
    expect(device.address).toBe('');
    expect(device.port).toBe(0);
    expect(device.status).toBe('online');
  });
});
