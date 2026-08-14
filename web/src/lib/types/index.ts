/**
 * 前端业务类型域 barrel。
 *
 * Business Logic（为什么需要这个模块）:
 *   按 core/settings/workbench/orchestrator/attention 拆分后，需要单一 re-export 入口供兼容层与新代码引用。
 *
 * Code Logic（这个模块做什么）:
 *   仅 re-export 各域类型；不含重复 interface 与 runtime 值；禁止回指 ../types 兼容层。
 */

export * from './core';
export * from './settings';
export * from './workbench';
export * from './orchestrator';
export * from './attention';
export * from './agentRuntime';
export * from './agentLedger';
export * from './agentHub';
export * from './portableInventory';
export * from './lanFleet';
export * from './providerManager';
export * from './wordgame';
export * from './gamePlugin';
export * from './battery';
