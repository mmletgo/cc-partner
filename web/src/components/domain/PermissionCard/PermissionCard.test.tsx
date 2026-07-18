// @vitest-environment jsdom

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { PermissionCard } from './PermissionCard';

afterEach(() => cleanup());

describe('PermissionCard', () => {
  test('renders the parent supplied action label and callback', async () => {
    await i18n.changeLanguage('zh');
    const onAction = vi.fn();

    render(
      <I18nextProvider i18n={i18n}>
        <PermissionCard
          icon={<span />}
          title="Input Monitoring"
          description="description"
          granted={false}
          actionLabel="打开系统设置"
          onRequestAccess={onAction}
        />
      </I18nextProvider>,
    );

    fireEvent.click(screen.getByRole('button', { name: '打开系统设置' }));
    expect(onAction).toHaveBeenCalledTimes(1);
  });
});
