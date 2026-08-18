// @vitest-environment jsdom
/**
 * 终端图片粘贴纯函数测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   远端 Agent 粘贴依赖「事件里有图才拦截」；误把纯文本 Ctrl+V 当图片会丢掉 Claude 文本粘贴。
 *
 * Code Logic（这个测试做什么）:
 *   构造 ClipboardEvent / KeyboardEvent 桩，断言取图、超限与 Ctrl+V 识别。
 */
import { describe, expect, test } from 'vitest';
import {
  clipboardEventImageFile,
  fileToPngDataUrl,
  isCtrlVPasteKey,
  MAX_TERMINAL_PASTE_IMAGE_BYTES,
} from './terminalImagePaste';

function makeClipboardEvent(files: File[]): ClipboardEvent {
  const fileList = {
    length: files.length,
    item: (index: number) => files[index] ?? null,
    [Symbol.iterator]: function* iterator() {
      yield* files;
    },
  } as unknown as FileList;
  const items = files.map((file) => ({
    kind: 'file' as const,
    type: file.type,
    getAsFile: () => file,
  }));
  return {
    clipboardData: {
      items,
      files: fileList,
    },
  } as unknown as ClipboardEvent;
}

describe('terminalImagePaste', () => {
  test('clipboardEventImageFile prefers image files and ignores text-only paste', () => {
    const image = new File([new Uint8Array([1, 2, 3])], 'shot.png', { type: 'image/png' });
    expect(clipboardEventImageFile(makeClipboardEvent([image]))).toBe(image);
    expect(clipboardEventImageFile(makeClipboardEvent([]))).toBeNull();
    const textEvent = {
      clipboardData: {
        items: [{ kind: 'string', type: 'text/plain', getAsFile: () => null }],
        files: { length: 0, item: () => null, [Symbol.iterator]: function* empty() {} },
      },
    } as unknown as ClipboardEvent;
    expect(clipboardEventImageFile(textEvent)).toBeNull();
  });

  test('isCtrlVPasteKey matches Agent image paste chord and ignores Cmd+V', () => {
    expect(
      isCtrlVPasteKey({
        type: 'keydown',
        key: 'v',
        ctrlKey: true,
        metaKey: false,
        altKey: false,
        shiftKey: false,
      } as KeyboardEvent),
    ).toBe(true);
    expect(
      isCtrlVPasteKey({
        type: 'keydown',
        key: 'v',
        ctrlKey: false,
        metaKey: true,
        altKey: false,
        shiftKey: false,
      } as KeyboardEvent),
    ).toBe(false);
  });

  test('fileToPngDataUrl reads png and rejects oversized blobs', async () => {
    const png = new File([new Uint8Array([137, 80, 78, 71])], 'a.png', { type: 'image/png' });
    const dataUrl = await fileToPngDataUrl(png);
    expect(dataUrl.startsWith('data:')).toBe(true);
    const huge = new File(
      [new Uint8Array(MAX_TERMINAL_PASTE_IMAGE_BYTES + 1)],
      'huge.png',
      { type: 'image/png' },
    );
    await expect(fileToPngDataUrl(huge)).rejects.toThrow('粘贴图片过大');
  });
});
