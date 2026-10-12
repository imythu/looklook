// 远程通道连接状态：连接中（动画）/ 已连接 / 失败（给出原因、怎么办，以及可展开的技术详情）。
// RelayPill：管理台右上角的小标记，点开是同样的详情和“立即重连”。
import { RefreshCw } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { errorText } from './i18n.js';
import { relativeTime } from './time.js';
import { Button, useToast } from './ui.jsx';

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

/** 右上角的远程通道状态（本机 / 局域网打开时显示；远程打开时顶栏已有“远程”标记）。 */
export function RelayPill() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const close = (e) => {
      if (e.type === 'keydown' ? e.key === 'Escape' : !ref.current?.contains(e.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', close);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', close);
    };
  }, [open]);
  const r = status.relay;
  if (!r || r.state === 'off') return null;
  const reconnect = async () => {
    setBusy(true);
    try {
      await api.post('/relay/reconnect');
      toast(t('relay.reconnecting'));
      setTimeout(reload, 1500);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <span className="relay-pill-wrap" ref={ref}>
      <button type="button" className={`relay-pill relay-pill-${r.state}`} onClick={() => setOpen(!open)} aria-expanded={open} aria-haspopup="dialog" title={t(`relay.state.${r.state}`)}>
        <span className={`dot ${r.state === 'connected' ? 'on' : r.state === 'failed' ? 'bad' : 'wait'}`} aria-hidden="true" />
        <span className="relay-pill-label">{t(`relay.pill.${r.state}`)}</span>
      </button>
      {open && (
        <div className="relay-pop" role="dialog" aria-label={t('relay.pill_title')}>
          <RelayStatus />
          {r.state === 'connected' && r.since > 0 && <p className="muted small" style={{ margin: '4px 0 0' }}>{t('relay.connected_since', { when: relativeTime(t, new Date(r.since).toISOString()) })}</p>}
          <div className="relay-pop-actions">
            <Button variant="ghost" icon={RefreshCw} busy={busy} onClick={reconnect}>
              {t('relay.reconnect')}
            </Button>
          </div>
          <p className="muted small" style={{ margin: 0 }}>{t('relay.auto_note')}</p>
        </div>
      )}
    </span>
  );
}
