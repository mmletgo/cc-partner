import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import App from './App';
import { bootstrapTheme } from './hooks/useTheme';

import './styles/tokens.css';
import './styles/reset.css';
import './styles/globals.css';
import './i18n'; // i18next 初始化(副作用导入,必须在 render 前)

// 主窗/卫星窗首屏前写入 data-theme，避免独立 WebView 先闪默认浅色。
bootstrapTheme();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter
      future={{
        v7_startTransition: true,
        v7_relativeSplatPath: true,
      }}
    >
      <App />
    </BrowserRouter>
  </StrictMode>
);
