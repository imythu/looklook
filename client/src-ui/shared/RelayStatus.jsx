// 远程通道连接状态：连接中（动画）/ 已连接 / 失败（给出原因、怎么办，以及可展开的技术详情）。
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { relativeTime } from './time.js';

const KNOWN = ['dns', 'refused', 'timeout', 'network', 'auth', 'handshake', 'heartbeat', 'config'];

export default function RelayStatus() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const [open, setOpen] = useState(false);
  const r = status.relay;
  if (!r || r.state === 'off') return null;
  const failed = r.state === 'failed';
  const reason = KNOWN.includes(r.reason) ? r.reason : 'other';
  return (
    <div className={`relay-status relay-${r.state}`} role="status" aria-live="polite">
      <div className="relay-line">
        {r.state === 'connecting' ? (
          <span className="dots" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
        ) : (
          <span className={`dot ${r.state === 'connected' ? 'on' : 'bad'}`} aria-hidden="true" />
        )}
        <b>{t(`relay.state.${r.state}`)}</b>
        {failed && r.failures > 1 && <span className="muted small">{t('relay.retrying', { n: r.failures })}</span>}
      </div>
      {failed && (
        <>
          <p className="relay-why">{t(`relay.reason.${reason}`)}</p>
          <p className="muted small relay-local">{t('relay.local_ok')}</p>
          {r.detail && (
            <>
              <button type="button" className="link-btn" aria-expanded={open} onClick={() => setOpen(!open)}>
                {t(open ? 'relay.hide_detail' : 'relay.show_detail')}
              </button>
              {open && (
                <pre className="relay-detail mono small">
                  {r.detail}
                  {'\n'}
                  {t('relay.since', { when: relativeTime(t, new Date(r.since).toISOString()) })}
                </pre>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
