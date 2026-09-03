/**
 * relayDevices 纯函数测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   影子设备（经跳板可见）与直连设备的去重/过滤规则被桌面 Picker、mobile Picker
 *   与 Settings 跳板行共用，口径错了会导致设备重复出现或跳板候选污染。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 isRelayShadowDevice 判定、直连+影子去重、Picker 列表（影子离线保留）、
 *   跳板候选过滤与 buildRelayViaRows 组装（含跳板失联占位）。
 */

import { describe, expect, test } from 'vitest';
import type { Device } from '@/lib/types';
import {
  buildRelayViaRows,
  dedupeRelayShadowDevices,
  filterRelayViaCandidates,
  isRelayShadowDevice,
  pickRelayAwarePickerDevices,
} from './relayDevices';

/**
 * Business Logic（为什么需要这个函数）:
 *   各用例需要形状一致的设备 fixture（直连/影子/离线组合），避免重复字面量漂移。
 *
 * Code Logic（这个函数做什么）:
 *   构造带可选 via 标记与 status 的 Device。
 */
function makeDevice(overrides: Partial<Device> & Pick<Device, 'id' | 'name'>): Device {
  return {
    address: '192.168.1.10',
    port: 62116,
    status: 'online',
    ...overrides,
  };
}

describe('isRelayShadowDevice', () => {
  test('treats non-empty viaDeviceId as shadow', () => {
    expect(isRelayShadowDevice({ viaDeviceId: 'relay-b' })).toBe(true);
    expect(isRelayShadowDevice({ viaDeviceId: '' })).toBe(false);
    expect(isRelayShadowDevice({})).toBe(false);
    expect(isRelayShadowDevice({ viaDeviceId: undefined })).toBe(false);
  });
});

describe('dedupeRelayShadowDevices', () => {
  test('keeps direct entry when same id also appears as shadow', () => {
    const devices = [
      makeDevice({ id: 'c-1', name: 'target', viaDeviceId: 'b-1', viaDeviceName: 'b' }),
      makeDevice({ id: 'c-1', name: 'target' }),
    ];
    const deduped = dedupeRelayShadowDevices(devices);
    expect(deduped).toHaveLength(1);
    expect(deduped[0]?.viaDeviceId).toBeUndefined();
  });

  test('keeps shadow entries for devices without direct counterpart and sorts direct first', () => {
    const devices = [
      makeDevice({ id: 'shadow-1', name: 'via-only', viaDeviceId: 'b-1' }),
      makeDevice({ id: 'direct-1', name: 'direct' }),
      makeDevice({ id: 'shadow-2', name: 'via-only-2', viaDeviceId: 'b-2' }),
    ];
    const deduped = dedupeRelayShadowDevices(devices);
    expect(deduped.map((device) => device.id)).toEqual([
      'direct-1',
      'shadow-1',
      'shadow-2',
    ]);
  });
});

describe('pickRelayAwarePickerDevices', () => {
  test('drops offline direct devices but keeps offline shadow devices', () => {
    const devices = [
      makeDevice({ id: 'direct-online', name: 'a' }),
      makeDevice({ id: 'direct-offline', name: 'b', status: 'offline' }),
      makeDevice({ id: 'shadow-online', name: 'c', viaDeviceId: 'b-1' }),
      makeDevice({
        id: 'shadow-offline',
        name: 'd',
        status: 'offline',
        viaDeviceId: 'b-1',
      }),
    ];
    const picked = pickRelayAwarePickerDevices(devices);
    expect(picked.map((device) => device.id)).toEqual([
      'direct-online',
      'shadow-online',
      'shadow-offline',
    ]);
  });

  test('dedupes direct+shadow duplicates before filtering', () => {
    const devices = [
      makeDevice({ id: 'dup', name: 'x', viaDeviceId: 'b-1', status: 'offline' }),
      makeDevice({ id: 'dup', name: 'x', status: 'offline' }),
    ];
    // 直连条目离线被过滤，影子条目因同 id 直连存在先被去重丢弃 → 最终列表为空
    expect(pickRelayAwarePickerDevices(devices)).toHaveLength(0);
  });
});

describe('filterRelayViaCandidates', () => {
  test('only keeps online direct non-self devices', () => {
    const devices = [
      makeDevice({ id: 'self', name: 'me' }),
      makeDevice({ id: 'direct-online', name: 'peer' }),
      makeDevice({ id: 'direct-offline', name: 'gone', status: 'offline' }),
      makeDevice({ id: 'shadow', name: 'via', viaDeviceId: 'b-1' }),
    ];
    expect(filterRelayViaCandidates(devices, 'self').map((device) => device.id)).toEqual([
      'direct-online',
    ]);
  });
});

describe('buildRelayViaRows', () => {
  test('assembles rows with shadow lists and counts per via device', () => {
    const devices = [
      makeDevice({ id: 'b-1', name: 'jump', address: '10.0.0.2' }),
      makeDevice({ id: 'c-1', name: 'target-1', viaDeviceId: 'b-1', viaDeviceName: 'jump' }),
      makeDevice({
        id: 'c-2',
        name: 'target-2',
        status: 'offline',
        viaDeviceId: 'b-1',
        viaDeviceName: 'jump',
      }),
      makeDevice({ id: 'c-3', name: 'other-target', viaDeviceId: 'b-2' }),
    ];
    const rows = buildRelayViaRows(devices, ['b-1']);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.deviceName).toBe('jump');
    expect(rows[0]?.address).toBe('10.0.0.2');
    expect(rows[0]?.shadowCount).toBe(2);
    expect(rows[0]?.shadows.map((shadow) => shadow.id)).toEqual(['c-1', 'c-2']);
    expect(rows[0]?.shadows[1]?.status).toBe('offline');
  });

  test('renders missing via device as offline placeholder row', () => {
    const rows = buildRelayViaRows([], ['b-gone']);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.deviceName).toBe('b-gone');
    expect(rows[0]?.status).toBe('offline');
    expect(rows[0]?.shadowCount).toBe(0);
  });
});
