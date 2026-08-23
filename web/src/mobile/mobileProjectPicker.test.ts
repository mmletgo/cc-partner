import { describe, expect, test } from 'vitest';
import type { MobileTransferDevice } from '@/api/transferHttp';
import {
  filterOnlineLanDevices,
  initialMobileProjectPickerState,
  mobileProjectPickerReducer,
} from './mobileProjectPicker';

/**
 * Business Logic（为什么需要这个函数）:
 *   局域网选择器不得把主机自己当成对端，也不得列出离线设备。
 *
 * Code Logic（这个测试做什么）:
 *   过滤 isSelf 与 offline，只保留在线对端。
 */
describe('filterOnlineLanDevices', () => {
  test('drops self and offline devices', () => {
    const devices: MobileTransferDevice[] = [
      {
        id: 'self',
        name: '这台电脑',
        address: '127.0.0.1',
        port: 62116,
        status: 'online',
        isSelf: true,
      },
      {
        id: 'office',
        name: 'office-mac',
        address: '100.1.2.3',
        port: 62116,
        status: 'online',
        isSelf: false,
      },
      {
        id: 'gone',
        name: 'old-linux',
        address: '100.1.2.4',
        port: 62116,
        status: 'offline',
        isSelf: false,
      },
    ];
    expect(filterOnlineLanDevices(devices).map((item) => item.id)).toEqual(['office']);
  });
});

describe('mobileProjectPickerReducer', () => {
  test('opens local browse by clearing device selection', () => {
    const next = mobileProjectPickerReducer(initialMobileProjectPickerState, {
      type: 'openLocal',
    });
    expect(next.mode).toBe('local');
    expect(next.selectedDeviceId).toBeNull();
    expect(next.error).toBeNull();
  });

  test('opens lan device list and then browse after selecting a device', () => {
    const listed = mobileProjectPickerReducer(initialMobileProjectPickerState, {
      type: 'openLan',
    });
    expect(listed.mode).toBe('lan-devices');
    const selected = mobileProjectPickerReducer(listed, {
      type: 'deviceSelected',
      deviceId: 'office',
    });
    expect(selected.mode).toBe('lan-browse');
    expect(selected.selectedDeviceId).toBe('office');
    expect(selected.currentPath).toBeNull();
  });

  test('does not change selection while openBusy', () => {
    const busy = mobileProjectPickerReducer(initialMobileProjectPickerState, {
      type: 'openStarted',
    });
    const blocked = mobileProjectPickerReducer(busy, {
      type: 'deviceSelected',
      deviceId: 'office',
    });
    expect(blocked.selectedDeviceId).toBeNull();
  });

  test('createSucceeded path is selected without opening', () => {
    const browsing = mobileProjectPickerReducer(initialMobileProjectPickerState, {
      type: 'openLocal',
    });
    const atParent = mobileProjectPickerReducer(browsing, {
      type: 'pathBrowsed',
      path: '/Users/hans/web_project',
    });
    const started = mobileProjectPickerReducer(atParent, { type: 'createStarted' });
    expect(started.createBusy).toBe(true);
    const blocked = mobileProjectPickerReducer(started, {
      type: 'pathBrowsed',
      path: '/Users/hans/other',
    });
    expect(blocked.currentPath).toBe('/Users/hans/web_project');
    const finished = mobileProjectPickerReducer(started, { type: 'createFinished' });
    const selected = mobileProjectPickerReducer(finished, {
      type: 'pathBrowsed',
      path: '/Users/hans/web_project/new-studio',
    });
    expect(selected.selectedPath).toBe('/Users/hans/web_project/new-studio');
    expect(selected.openBusy).toBe(false);
  });
});
