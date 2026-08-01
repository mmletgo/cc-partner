/**
 * 终端 buffer 写入计划纯函数（共享 neutral 模块）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面 buffer store 的「同 authority resync 保 scrollback」与移动端 HTTP replay 都需要
 *   「旧 buffer → 新 buffer」的最小写入计划，避免各自重写一份 KMP。把纯函数抽到 lib 层
 *   供 hooks（store）与 pages（mobile replay）共同复用，杜绝跨层环形依赖。
 *
 * Code Logic（这个模块做什么）:
 *   只导出 longestSuffixPrefixOverlap / planTerminalBufferWrite 与对应类型；无副作用、无导入。
 */

export type TerminalBufferWriteMode = 'none' | 'append' | 'replay';

export interface TerminalBufferWritePlan {
  mode: TerminalBufferWriteMode;
  data: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端输出缓存达到上限后会裁掉前缀，但活跃 xterm 仍需要继续追加新输出；
 *   桌面 store 同 authority resync 时也要判断「新快照是否只是旧 buffer 的滑动延续」。
 *
 * Code Logic（这个函数做什么）:
 *   用 KMP 前缀表计算 previous 后缀与 next 前缀的最长重叠长度，避免长度相同的滑动缓存被误判为无变化。
 */
export function longestSuffixPrefixOverlap(previous: string, next: string): number {
  const maxLength = Math.min(previous.length, next.length);
  if (maxLength === 0) return 0;

  const pattern = next.slice(0, maxLength);
  const prefixTable = new Array<number>(pattern.length).fill(0);

  for (let index = 1, matched = 0; index < pattern.length; index += 1) {
    while (matched > 0 && pattern[index] !== pattern[matched]) {
      matched = prefixTable[matched - 1] ?? 0;
    }
    if (pattern[index] === pattern[matched]) {
      matched += 1;
      prefixTable[index] = matched;
    }
  }

  let matched = 0;
  const start = previous.length - maxLength;
  for (let index = start; index < previous.length; index += 1) {
    while (matched > 0 && previous[index] !== pattern[matched]) {
      matched = prefixTable[matched - 1] ?? 0;
    }
    if (previous[index] === pattern[matched]) {
      matched += 1;
      if (matched === pattern.length && index < previous.length - 1) {
        matched = prefixTable[matched - 1] ?? 0;
      }
    }
  }

  return matched;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输出缓存截断后，当前 buffer 长度可能保持不变，但用户仍需要看到新产生的终端输出；
 *   桌面 store 同 authority resync 时也要据此判断能否只追加差异（保 scrollback）而非整屏重放。
 *
 * Code Logic（这个函数做什么）:
 *   对比上次已写入的 buffer 与最新缓存：前缀扩展走 append，滑动截断走重叠 append，无法对齐时要求 clear + replay。
 */
export function planTerminalBufferWrite(
  previousBuffer: string,
  nextBuffer: string,
): TerminalBufferWritePlan {
  if (previousBuffer === nextBuffer) return { mode: 'none', data: '' };
  if (nextBuffer.startsWith(previousBuffer)) {
    return { mode: 'append', data: nextBuffer.slice(previousBuffer.length) };
  }

  const overlap = longestSuffixPrefixOverlap(previousBuffer, nextBuffer);
  if (overlap > 0) {
    return { mode: 'append', data: nextBuffer.slice(overlap) };
  }

  return { mode: 'replay', data: nextBuffer };
}
