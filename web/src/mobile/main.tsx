import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { bootstrapTheme } from '@/hooks/useTheme';
import { MobileApp } from './MobileApp';

import '../styles/tokens.css';
import '../styles/reset.css';
import '../styles/globals.css';
import '../i18n';

// 首屏前应用浅/深主题，避免 React 挂载前闪默认浅色。
bootstrapTheme();

const mobileRoot = document.getElementById('mobile-root');

if (!mobileRoot) {
  throw new Error('mobile-root element is required');
}

createRoot(mobileRoot).render(
  <StrictMode>
    <MobileApp />
  </StrictMode>,
);
