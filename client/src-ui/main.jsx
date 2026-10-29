import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import App from './App.jsx';
import { installRtcDownloads } from './shared/api.js';
import ErrorBoundary, { reportUiError } from './shared/ErrorBoundary.jsx';
import './shared/i18n.js';
import { ConfirmProvider, ToastProvider } from './shared/ui.jsx';
import './styles.css';

window.addEventListener('error', (e) => reportUiError(e.message, e.error?.stack ?? `${e.filename}:${e.lineno}`));
window.addEventListener('unhandledrejection', (e) => {
  const r = e.reason;
  // 接口返回的业务错误（ApiError）已经在界面上提示过，不算脚本错误
  if (r?.code && r?.status !== undefined) return;
  reportUiError(r?.message ?? String(r), r?.stack ?? '');
});

installRtcDownloads();

createRoot(document.getElementById('root')).render(
  <StrictMode>
    <ToastProvider>
      <ConfirmProvider>
        <ErrorBoundary page>
          <App />
        </ErrorBoundary>
      </ConfirmProvider>
    </ToastProvider>
  </StrictMode>,
);
