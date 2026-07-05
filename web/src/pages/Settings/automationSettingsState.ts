import type {
  OrchestratorAutomationConfig,
  OrchestratorAutomationConfigPatch,
} from '@/api/orchestratorConfig';

/** Settings 自动化 tab 的受控表单值。 */
export interface AutomationSettingsForm {
  enabled: boolean;
  maxConcurrentTasks: number;
  verificationCommandsText: string;
  autoCommit: boolean;
  autoPushTaskBranch: boolean;
  autoMergeToMain: boolean;
  autoPushMain: boolean;
}

/** 自动化表单加载前占位值；真实当前值和默认值由后端命令覆盖。 */
export const PENDING_AUTOMATION_SETTINGS_FORM: AutomationSettingsForm = {
  enabled: false,
  maxConcurrentTasks: 1,
  verificationCommandsText: '',
  autoCommit: false,
  autoPushTaskBranch: false,
  autoMergeToMain: false,
  autoPushMain: false,
};

const AUTOMATION_MAX_CONCURRENT_TASKS_MIN = 1;
const AUTOMATION_MAX_CONCURRENT_TASKS_MAX = 8;

/**
 * 将验证命令数组转换为 textarea 文本
 *
 * Business Logic（为什么需要）:
 *   后端配置以字符串数组保存验证命令，Settings 自动化 tab 需要用多行 textarea 展示和编辑。
 *
 * Code Logic（做什么）:
 *   使用换行符把命令按原顺序 join；不 trim，不过滤，避免前端与后端归一化规则分叉。
 */
export function commandsToTextarea(commands: string[]): string {
  return commands.join('\n');
}

/**
 * 归一化 textarea 文本行尾
 *
 * Business Logic（为什么需要）:
 *   用户可能从不同系统复制验证命令，CRLF/CR 行尾需要在前端保存为稳定文本，
 *   但空行和空格仍交给后端统一校验与过滤。
 *
 * Code Logic（做什么）:
 *   只把 \r\n / \r 归一为 \n，保留用户输入的多行文本内容。
 */
export function textareaToCommandsText(value: string): string {
  return value.replace(/\r\n?/g, '\n');
}

/**
 * 约束自动化并发任务上限
 *
 * Business Logic（为什么需要）:
 *   Settings 自动化 tab 允许用户手动输入项目并发上限，前端需要即时收敛到后端允许的 1..8 范围，
 *   避免用户保存时才收到可预期的范围错误。
 *
 * Code Logic（做什么）:
 *   将非有限数字回退为下限；对有限数字取整数后夹在 1..8 之间，返回可直接写回受控 input 的值。
 */
export function clampAutomationMaxConcurrentTasks(value: number): number {
  const normalized = Number.isFinite(value)
    ? Math.trunc(value)
    : AUTOMATION_MAX_CONCURRENT_TASKS_MIN;
  return Math.min(
    AUTOMATION_MAX_CONCURRENT_TASKS_MAX,
    Math.max(AUTOMATION_MAX_CONCURRENT_TASKS_MIN, normalized),
  );
}

/**
 * 将后端 Orchestrator 自动化配置映射为 Settings 表单
 *
 * Business Logic（为什么需要）:
 *   设置页加载当前配置和默认配置时都要进入同一套受控表单结构，null 占位不能复用共享常量引用。
 *
 * Code Logic（做什么）:
 *   null/undefined 返回 pending form 的新对象；非空配置复制布尔/数字字段并把验证命令数组转为 textarea 文本。
 */
export function automationConfigToForm(
  config: OrchestratorAutomationConfig | null | undefined,
): AutomationSettingsForm {
  if (!config) return { ...PENDING_AUTOMATION_SETTINGS_FORM };
  return {
    enabled: config.enabled,
    maxConcurrentTasks: clampAutomationMaxConcurrentTasks(config.maxConcurrentTasks),
    verificationCommandsText: commandsToTextarea(config.verificationCommands),
    autoCommit: config.autoCommit,
    autoPushTaskBranch: config.autoPushTaskBranch,
    autoMergeToMain: config.autoMergeToMain,
    autoPushMain: config.autoPushMain,
  };
}

/**
 * 将自动化表单映射为后端 update patch
 *
 * Business Logic（为什么需要）:
 *   用户点击应用配置时，应把所有自动化字段提交给 Phase 1 后端命令，并让后端负责验证命令归一化。
 *
 * Code Logic（做什么）:
 *   复制布尔/数字字段；verificationCommands 使用 textarea 文本，且只做行尾归一。
 */
export function automationFormToPatch(
  form: AutomationSettingsForm,
): OrchestratorAutomationConfigPatch {
  return {
    enabled: form.enabled,
    maxConcurrentTasks: clampAutomationMaxConcurrentTasks(form.maxConcurrentTasks),
    verificationCommands: textareaToCommandsText(form.verificationCommandsText),
    autoCommit: form.autoCommit,
    autoPushTaskBranch: form.autoPushTaskBranch,
    autoMergeToMain: form.autoMergeToMain,
    autoPushMain: form.autoPushMain,
  };
}

/**
 * 判断自动化表单是否有未应用修改
 *
 * Business Logic（为什么需要）:
 *   Settings 自动化 tab 需要展示 dirty/saved 状态并禁用无变更保存，比较必须覆盖全部字段。
 *
 * Code Logic（做什么）:
 *   当前字段均为可 JSON 序列化的原始值，直接序列化比较保持实现简单确定。
 */
export function isAutomationFormDirty(
  form: AutomationSettingsForm,
  initial: AutomationSettingsForm,
): boolean {
  return JSON.stringify(form) !== JSON.stringify(initial);
}
