// @vitest-environment jsdom
/**
 * 终端文件交给 Agent 的纯函数测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   拖入命中、图优先、uri-list 路径和体积上限必须脱离 DOM 可测，避免误把文本粘贴当附件。
 *
 * Code Logic（这个测试做什么）:
 *   构造矩形/剪贴板桩，断言命中、非图片提取、file:// 解析与超限。
 */
import { describe, expect, test } from 'vitest';
import {
  attachFilesWithinLimit,
  clipboardEventNonImageFiles,
  logicalPointHitsRect,
  MAX_TERMINAL_ATTACH_BYTES,
  parseFileUriList,
  physicalDropPointToCss,
  safeAttachFileName,
} from './terminalFileAttach';

function makeClipboardEvent(files: File[]): ClipboardEvent {
  const fileList = {
    length: files.length,
    item: (index: number) => files[index] ?? null,
    [Symbol.iterator]: function* iterator() {
      yield* files;
    },
  } as unknown as FileList;
  return {
    clipboardData: {
      files: fileList,
      items: files.map((file) => ({
        kind: 'file' as const,
        type: file.type,
        getAsFile: () => file,
      })),
    },
  } as unknown as ClipboardEvent;
}

describe('terminalFileAttach', () => {
  test('logicalPointHitsRect only accepts points inside the terminal panel', () => {
    const rect = { left: 10, top: 20, right: 110, bottom: 120 };
    expect(logicalPointHitsRect(10, 20, rect)).toBe(true);
    expect(logicalPointHitsRect(110, 120, rect)).toBe(true);
    expect(logicalPointHitsRect(9, 50, rect)).toBe(false);
    expect(logicalPointHitsRect(50, 121, rect)).toBe(false);
  });

  test('physicalDropPointToCss divides by scale factor', () => {
    expect(physicalDropPointToCss({ x: 200, y: 100 }, 2)).toEqual({ x: 100, y: 50 });
    expect(physicalDropPointToCss({ x: 10, y: 4 }, 0)).toEqual({ x: 10, y: 4 });
  });

  test('clipboardEventNonImageFiles yields PDFs but yields nothing when an image is present', () => {
    const pdf = new File([new Uint8Array([1])], 'a.pdf', { type: 'application/pdf' });
    const image = new File([new Uint8Array([2])], 'shot.png', { type: 'image/png' });
    expect(clipboardEventNonImageFiles(makeClipboardEvent([pdf]))).toEqual([pdf]);
    expect(clipboardEventNonImageFiles(makeClipboardEvent([image, pdf]))).toEqual([]);
    expect(clipboardEventNonImageFiles(makeClipboardEvent([]))).toEqual([]);
  });

  test('parseFileUriList decodes file URLs and skips comments', () => {
    expect(
      parseFileUriList('# ignore\nfile:///Users/hans/note.pdf\nfile:///Users/hans/logs/a.log\n'),
    ).toEqual(['/Users/hans/note.pdf', '/Users/hans/logs/a.log']);
    expect(parseFileUriList('https://example.com/a.pdf')).toEqual([]);
  });

  test('safeAttachFileName strips directories and rejects traversal', () => {
    expect(safeAttachFileName('note.pdf')).toBe('note.pdf');
    expect(safeAttachFileName('/tmp/../secret')).toBe('secret');
    expect(safeAttachFileName('..')).toBeNull();
    expect(safeAttachFileName('')).toBeNull();
  });

  test('attachFilesWithinLimit rejects payloads above the 20 MiB cap', () => {
    expect(attachFilesWithinLimit([{ size: 10 }])).toBe(true);
    expect(attachFilesWithinLimit([{ size: MAX_TERMINAL_ATTACH_BYTES + 1 }])).toBe(false);
    expect(
      attachFilesWithinLimit([{ size: MAX_TERMINAL_ATTACH_BYTES - 1 }, { size: 2 }]),
    ).toBe(false);
  });
});
