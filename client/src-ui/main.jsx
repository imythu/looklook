import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import App from './App.jsx';
import './shared/i18n.js';
import { ConfirmProvider, ToastProvider } from './shared/ui.jsx';
import './styles.css';

// 页面脚本出错时记进客户端的错误记录（设置 → 问题与日志），报告问题时一并附上。
let lastUiError = '';
function reportUiError(message, detail) {
  const key = `${message}|${detail}`.slice(0, 300);
  if (!message || key === lastUiError) return;
  lastUiError = key;
  fetch('/api/diag/ui-error', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-LL-Client': '1' },
    body: JSON.stringify({ message: String(message).slice(0, 1000), detail: `${location.pathname} ${detail ?? ''}`.slice(0, 4000) }),
  }).catch(() => {});
}
window.addEventListener('error', (e) => reportUiError(e.message, e.error?.stack ?? `${e.filename}:${e.lineno}`));
window.addEventListener('unhandledrejection', (e) => {
  const r = e.reason;
  // 接口返回的业务错误（ApiError）已经在界面上提示过，不算脚本错误
  if (r?.code && r?.status !== undefined) return;
  reportUiError(r?.message ?? String(r), r?.stack ?? '');
});

createRoot(document.getElementById('root')).render(
  <StrictMode>
    <ToastProvider>
      <ConfirmProvider>
        <App />
      </ConfirmProvider>
    </ToastProvider>
  </StrictMode>,
);
