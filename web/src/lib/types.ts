/**
 * 前端业务类型兼容 barrel。
 *
 * Business Logic（为什么需要这个模块）:
 *   历史代码大量 `import type { ... } from '@/lib/types'`，拆分后必须保持该路径可用，避免一次改全仓 import。
 *
 * Code Logic（这个模块做什么）:
 *   纯 re-export `./types/index`；不在此文件声明任何 interface/type/value。
 */

export * from './types/index';
