import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { MobileApp } from './MobileApp';

import '../styles/tokens.css';
import '../styles/reset.css';
import '../styles/globals.css';
import '../i18n';

const mobileRoot = document.getElementById('mobile-root');

if (!mobileRoot) {
  throw new Error('mobile-root element is required');
}

createRoot(mobileRoot).render(
  <StrictMode>
    <MobileApp />
  </StrictMode>,
);
