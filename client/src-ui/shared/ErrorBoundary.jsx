import { AlertTriangle, RefreshCw } from 'lucide-react';
import { Component } from 'react';
import { useTranslation } from 'react-i18next';

// 页面脚本出错时记进客户端的错误记录（设置 → 问题与日志），报告问题时一并附上。
let lastUiError = '';
export function reportUiError(message, detail) {
  const key = `${message}|${detail}`.slice(0, 300);
  if (!message || key === lastUiError) return;
  lastUiError = key;
  fetch('/api/diag/ui-error', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Looklook-Client': '1' },
    body: JSON.stringify({ message: String(message).slice(0, 1000), detail: `${location.pathname} ${detail ?? ''}`.slice(0, 4000) }),
  }).catch(() => {});
}

function Crashed({ page, onRetry }) {
  const { t } = useTranslation();
  return (
    <div className={`note note-error crashed ${page ? 'crashed-page' : ''}`} role="alert">
      <AlertTriangle size={16} className="icon" aria-hidden="true" />
      <div className="crashed-body">
        <span>{t(page ? 'crash.page' : 'crash.part')}</span>
        <span className="inline">
          {!page && (
            <button type="button" className="btn btn-sm" onClick={onRetry}>
              {t('crash.retry')}
            </button>
          )}
          <button type="button" className="btn btn-sm btn-plain" onClick={() => location.reload()}>
            <RefreshCw size={14} />
            {t('crash.reload')}
          </button>
        </span>
      </div>
    </div>
  );
}

/**
 * 把渲染时出的错关在这一块里：别的地方照常能用，这里显示出错提示（重试 / 重新加载），并记进错误记录。
 * `page`：整页的兜底，只给“重新加载”。`resetKey` 变了（换页、换栏）自动重试。
 */
export default class ErrorBoundary extends Component {
  state = { error: null, key: undefined };

  static getDerivedStateFromError(error) {
    return { error };
  }

  static getDerivedStateFromProps(props, state) {
    if (props.resetKey === state.key) return null;
    return { error: null, key: props.resetKey };
  }

  componentDidCatch(error, info) {
    reportUiError(error?.message ?? String(error), `${error?.stack ?? ''}\n${info?.componentStack ?? ''}`);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return <Crashed page={this.props.page} onRetry={() => this.setState({ error: null })} />;
  }
}
