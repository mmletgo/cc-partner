// @vitest-environment jsdom
/**
 * WORKFLOW 向导 UI 合同测试
 *
 * Business Logic（为什么需要这个测试）:
 *   missing 应展示模板预览/创建，valid 展示摘要/打开文件，invalid 聚焦诊断行，
 *   hash conflict 保留草稿并提供重新加载。
 *
 * Code Logic（这个测试做什么）:
 *   jsdom 渲染纯 props 对话框并断言关键 testid / 按钮 / 草稿保留。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { createRef } from 'react';
import {
  WorkflowWizardDialog,
  type WorkflowWizardDialogProps,
} from './WorkflowWizardDialog';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) => {
      const map: Record<string, string> = {
        'orchestrator:workflowWizard.title': 'WORKFLOW 向导',
        'orchestrator:workflowWizard.subtitle': '检测并安全保存 WORKFLOW.md',
        'orchestrator:workflowWizard.close': '关闭',
        'orchestrator:workflowWizard.loading': '加载中',
        'orchestrator:workflowWizard.templatePreview': '默认模板预览',
        'orchestrator:workflowWizard.createFromTemplate': '从模板创建',
        'orchestrator:workflowWizard.openFile': '打开文件',
        'orchestrator:workflowWizard.validate': '校验',
        'orchestrator:workflowWizard.save': '保存',
        'orchestrator:workflowWizard.reload': '重新加载',
        'orchestrator:workflowWizard.draft': '草稿',
        'orchestrator:workflowWizard.draftPlaceholder': '在此编辑 WORKFLOW.md',
        'orchestrator:workflowWizard.summaryValid': '文档有效',
        'orchestrator:workflowWizard.summaryNoDelivery': '不会改变 delivery',
        'orchestrator:workflowWizard.diagnostics': '诊断',
        'orchestrator:workflowWizard.conflict': '文件已变化，草稿已保留',
        'orchestrator:workflowWizard.status.missing': '缺失',
        'orchestrator:workflowWizard.status.valid': '有效',
        'orchestrator:workflowWizard.status.invalid': '无效',
        'orchestrator:workflowWizard.status.readError': '读取失败',
        'orchestrator:workflowWizard.status.unknown': '未知',
      };
      if (key === 'orchestrator:workflowWizard.hash' && opts?.hash) {
        return `hash ${opts.hash}`;
      }
      if (key === 'orchestrator:workflowWizard.diagnosticMeta' && opts) {
        return `${opts.path}:${opts.line}:${opts.column} ${opts.code}`;
      }
      return map[key] ?? key;
    },
    i18n: { language: 'zh' },
  }),
}));

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要稳定的纯 props 默认值，避免每个用例重复样板。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖的 WorkflowWizardDialogProps。
 */
function buildProps(overrides: Partial<WorkflowWizardDialogProps> = {}): WorkflowWizardDialogProps {
  return {
    open: true,
    loadState: 'ready',
    documentStatus: 'missing',
    draft: '',
    expectedHash: '',
    diagnostics: [],
    preview: '---\nworkflow:\n  default_create_state: backlog\n---\n',
    loadError: null,
    saveError: null,
    conflict: false,
    busy: false,
    focusedDiagnosticLine: null,
    draftTextareaRef: createRef<HTMLTextAreaElement>(),
    onClose: vi.fn(),
    onDraftChange: vi.fn(),
    onCreateFromTemplate: vi.fn(),
    onValidate: vi.fn(),
    onSave: vi.fn(),
    onReload: vi.fn(),
    onOpenFile: vi.fn(),
    onFocusDiagnostic: vi.fn(),
    ...overrides,
  };
}

describe('WorkflowWizardDialog', () => {
  test('missing shows template preview and create action', () => {
    const onCreateFromTemplate = vi.fn();
    render(
      <WorkflowWizardDialog
        {...buildProps({
          documentStatus: 'missing',
          preview: 'DEFAULT_TEMPLATE',
          onCreateFromTemplate,
        })}
      />,
    );

    expect(screen.getByTestId('workflow-wizard-template-preview').textContent).toContain(
      'DEFAULT_TEMPLATE',
    );
    fireEvent.click(screen.getByTestId('workflow-wizard-create-template'));
    expect(onCreateFromTemplate).toHaveBeenCalledTimes(1);
  });

  test('valid shows parsed summary and open file', () => {
    const onOpenFile = vi.fn();
    render(
      <WorkflowWizardDialog
        {...buildProps({
          documentStatus: 'valid',
          draft: '---\nworkflow:\n  default_create_state: backlog\n---\n',
          expectedHash: 'abc123def456',
          preview: null,
          onOpenFile,
        })}
      />,
    );

    expect(screen.getByTestId('workflow-wizard-summary').textContent).toContain('文档有效');
    expect(screen.getByTestId('workflow-wizard-summary').textContent).toContain('不会改变 delivery');
    fireEvent.click(screen.getByTestId('workflow-wizard-open-file'));
    expect(onOpenFile).toHaveBeenCalledTimes(1);
  });

  test('invalid focuses diagnostic line on click', () => {
    const onFocusDiagnostic = vi.fn();
    render(
      <WorkflowWizardDialog
        {...buildProps({
          documentStatus: 'invalid',
          draft: 'broken',
          diagnostics: [
            {
              path: 'WORKFLOW.md',
              line: 3,
              column: 1,
              code: 'workflow.invalid_yaml',
              message: 'YAML 语法错误',
            },
          ],
          focusedDiagnosticLine: 3,
          onFocusDiagnostic,
        })}
      />,
    );

    const item = screen.getByTestId('workflow-wizard-diagnostic-0');
    expect(item.getAttribute('data-focused')).toBe('true');
    fireEvent.click(item);
    expect(onFocusDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({ line: 3, code: 'workflow.invalid_yaml' }),
    );
    expect(screen.getByTestId('workflow-wizard-draft').getAttribute('data-focused-line')).toBe('3');
  });

  test('hash conflict preserves draft and offers reload', () => {
    const onReload = vi.fn();
    render(
      <WorkflowWizardDialog
        {...buildProps({
          documentStatus: 'valid',
          draft: 'user-draft-content',
          conflict: true,
          saveError: 'workflow_document_changed',
          onReload,
        })}
      />,
    );

    expect((screen.getByTestId('workflow-wizard-draft') as HTMLTextAreaElement).value).toBe(
      'user-draft-content',
    );
    expect(screen.getByTestId('workflow-wizard-conflict').textContent).toContain('草稿已保留');
    fireEvent.click(screen.getByTestId('workflow-wizard-reload'));
    expect(onReload).toHaveBeenCalledTimes(1);
  });
});
