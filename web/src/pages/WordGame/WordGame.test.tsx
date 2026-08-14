// @vitest-environment jsdom

/**
 * 记单词闪卡题面合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   中译英不能把 lemma 当标题露出来，否则答案直接写在题面上。
 *
 * Code Logic（这个测试做什么）:
 *   渲染 zhToEn / enToZh 卡，断言 lemma 标题只在不会泄题的题型出现。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { WordGame } from './WordGame';
import type { WordgameCard } from '@/lib/types/wordgame';

vi.mock('@/api/wordgame', () => ({
  wordgameApi: {
    submitAnswer: vi.fn(),
  },
}));

afterEach(() => {
  cleanup();
});

beforeEach(async () => {
  await i18n.changeLanguage('zh');
});

function card(overrides: Partial<WordgameCard>): WordgameCard {
  return {
    lemma: 'feature',
    questionType: 'enToZh',
    kind: 'choice',
    prompt: 'feature 的中文是？',
    options: ['特性', '失败'],
    ...overrides,
  };
}

describe('WordGame', () => {
  test('hides the English lemma on Chinese-to-English cards', () => {
    render(
      <I18nextProvider i18n={i18n}>
        <WordGame
          initialCard={card({
            questionType: 'zhToEn',
            kind: 'input',
            prompt: '请写出「特性、功能」对应的英文单词',
            options: [],
          })}
          onBack={() => undefined}
        />
      </I18nextProvider>,
    );

    expect(screen.queryByTestId('wordgame-lemma')).toBeNull();
    expect(screen.queryByText('feature')).toBeNull();
    expect(screen.getByTestId('wordgame-prompt').textContent).toContain('特性、功能');
  });

  test('still shows the English lemma on English-to-Chinese cards', () => {
    render(
      <I18nextProvider i18n={i18n}>
        <WordGame initialCard={card({})} onBack={() => undefined} />
      </I18nextProvider>,
    );

    expect(screen.getByTestId('wordgame-lemma').textContent).toBe('feature');
  });
});
