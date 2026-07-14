// @vitest-environment jsdom
/**
 * TagInput accessible-name 合同测试
 *
 * Business Logic（为什么需要这些测试）:
 *   标签输入框必须有显式 accessible name；placeholder 不能充当名称，
 *   调用方必须提供 ariaLabel 或 ariaLabelledBy 恰好其一。
 *
 * Code Logic（这些测试做什么）:
 *   渲染 TagInput 并断言 input 的 aria-label / aria-labelledby 与交互行为。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { TagInput } from './TagInput';

afterEach(() => {
  cleanup();
});

describe('TagInput accessible name', () => {
  test('ariaLabel 成为 input 的 accessible name（placeholder 不充当名称）', () => {
    render(
      <TagInput
        tags={[]}
        onChange={() => undefined}
        placeholder="Type a tag"
        ariaLabel="Prompt tags"
      />,
    );
    const input = screen.getByRole('textbox', { name: 'Prompt tags' });
    expect(input.getAttribute('aria-label')).toBe('Prompt tags');
    expect(input.getAttribute('placeholder')).toBe('Type a tag');
    expect(input.getAttribute('aria-labelledby')).toBeNull();
  });

  test('ariaLabelledBy 绑定外部标签', () => {
    render(
      <>
        <span id="tag-field-label">Tags</span>
        <TagInput tags={[]} onChange={() => undefined} ariaLabelledBy="tag-field-label" />
      </>,
    );
    const input = screen.getByRole('textbox', { name: 'Tags' });
    expect(input.getAttribute('aria-labelledby')).toBe('tag-field-label');
    expect(input.getAttribute('aria-label')).toBeNull();
  });

  test('Enter 添加标签并回调 onChange', () => {
    const onChange = vi.fn();
    render(
      <TagInput tags={['a']} onChange={onChange} ariaLabel="Tags" />,
    );
    const input = screen.getByRole('textbox', { name: 'Tags' });
    fireEvent.change(input, { target: { value: 'b' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onChange).toHaveBeenCalledWith(['a', 'b']);
  });
});
