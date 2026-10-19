// 远程通道连接状态：连接中（动画）/ 已连接 / 已连接但到线路的 UDP 不通（远程控制台连不上）/ 失败（给出原因、怎么办，以及可展开的技术详情）。
// ConnChip：管理台右上角的连接胶囊，本机、局域网、远程打开都在同一个位置：
//   本机：“本机 · 远程在线”，点开是远程通道的详情、“立即重连”和换线路；
//   远程（浏览器，或 App 里选中了别的电脑）：“远程 · 86 ms”，点开是连着哪台电脑、端到端加密、延迟和线路；
//   局域网：“局域网”，点开说明是直接连到这台电脑。
import { Lock, RefreshCw } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { deviceName, useDevices } from './devices.jsx';
import { useLanPage } from './lan.jsx';
import { errorText } from './i18n.js';
import { RELAYS_ANCHOR } from './RelayPicker.jsx';
import { Link } from './router.jsx';
import { relativeTime } from './time.js';
import { Button, useToast } from './ui.jsx';

const KNOWN = ['dns', 'refused', 'timeout', 'network', 'auth', 'handshake', 'heartbeat', 'config'];

/** 远程通道连着，但测速发现这台电脑到当前线路的 UDP 不通：远程控制台（WebRTC）连不上。 */
function udpBlocked(status) {
  return status.relay?.state === 'connected' && !!status.relay_check?.blocked;
}

export default function RelayStatus() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const [open, setOpen] = useState(false);
  const r = status.relay;
  if (!r || r.state === 'off') return null;
  const failed = r.state === 'failed';
  const blocked = udpBlocked(status);
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
          <span className={`dot ${blocked ? 'warn' : r.state === 'connected' ? 'on' : 'bad'}`} aria-hidden="true" />
        )}
        <b>{t(blocked ? 'relay.state.blocked' : `relay.state.${r.state}`)}</b>
        {failed && r.failures > 1 && <span className="muted small">{t('relay.retrying', { n: r.failures })}</span>}
      </div>
      {blocked && (
        <>
          <p className="relay-why">{t('relay.blocked_why', { name: status.relay_check.current })}</p>
          <p className="muted small relay-local">
            {t('relay.blocked_fix')} <Link to={`/settings#${RELAYS_ANCHOR}`}>{t('relays.change_in_settings')}</Link>
          </p>
        </>
      )}
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

/** 往返时间（毫秒）：每 10 秒请求一次这台电脑的 /api/ping，取最近 3 次的中位数，免得一次抖动就跳。 */
export function useRemoteLatency(enabled) {
  const [ms, setMs] = useState(null);
  useEffect(() => {
    if (!enabled) return undefined;
    const samples = [];
    let alive = true;
    const once = async () => {
      if (document.hidden) return;
      const t0 = performance.now();
      try {
        await api.get('/ping');
      } catch (e) {
        if (e.status === 0 || e.status >= 500) {
          if (alive) setMs(null);
          return;
        }
        // 旧版客户端没有这个接口（404）也是一次完整的往返
      }
      samples.push(performance.now() - t0);
      if (samples.length > 3) samples.shift();
      const sorted = [...samples].sort((a, b) => a - b);
      if (alive) setMs(Math.round(sorted[Math.floor(sorted.length / 2)]));
    };
    once();
    const id = setInterval(once, 10000);
    window.addEventListener('focus', once);
    return () => {
      alive = false;
      clearInterval(id);
      window.removeEventListener('focus', once);
    };
  }, [enabled]);
  return ms;
}

const SLOW_MS = 150;
const VERY_SLOW_MS = 400;

/** 点开胶囊、点外面或按 Esc 关上。 */
function usePop() {
  const [open, setOpen] = useState(false);
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
  return { open, setOpen, ref };
}

/** 本机打开：远程通道的状态、重连、换线路。 */
function LocalPop() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const r = status.relay;
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
    <>
      <b className="conn-pop-title">{t('conn.local_title')}</b>
      <RelayStatus />
      {r.state === 'connected' && r.since > 0 && <p className="muted small" style={{ margin: 0 }}>{t('relay.connected_since', { when: relativeTime(t, new Date(r.since).toISOString()) })}</p>}
      {status.relay_check?.current && (
        <p className="small" style={{ margin: 0 }}>
          {t('conn.route', { name: status.relay_check.current })} · <Link to={`/settings#${RELAYS_ANCHOR}`}>{t('conn.change_route')}</Link>
        </p>
      )}
      <div className="relay-pop-actions">
        <Button variant="ghost" icon={RefreshCw} busy={busy} onClick={reconnect}>
          {t('relay.reconnect')}
        </Button>
      </div>
      <p className="muted small" style={{ margin: 0 }}>{t('conn.local_note')}</p>
    </>
  );
}

/** 远程打开：连着哪台电脑、加密、延迟、线路。 */
function RemotePop({ ms, name }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  return (
    <>
      <b className="conn-pop-title">{t('conn.remote_title', { name })}</b>
      <dl className="kv conn-kv">
        <dt>{t('conn.secure')}</dt>
        <dd className="ok-text">
          <Lock size={13} aria-hidden="true" /> {t('conn.e2e')}
        </dd>
        <dt>{t('conn.latency')}</dt>
        <dd>{ms === null ? '—' : t('nav.remote_ms', { ms })}</dd>
        {status.relay_check?.current && (
          <>
            <dt>{t('conn.route_label')}</dt>
            <dd>
              {status.relay_check.current} · <Link to={`/settings#${RELAYS_ANCHOR}`}>{t('conn.change_route_there')}</Link>
            </dd>
          </>
        )}
      </dl>
      <p className="muted small" style={{ margin: 0 }}>{t('conn.remote_note')}</p>
    </>
  );
}

/** 右上角的连接胶囊（见文件开头）。 */
export function ConnChip() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const lanPage = useLanPage();
  const { open, setOpen, ref } = usePop();
  const mode = lanPage ? 'lan' : status.access === 'local' ? 'local' : 'remote';
  const ms = useRemoteLatency(mode === 'remote');
  const r = status.relay;
  if (mode === 'local' && (!r || r.state === 'off')) return null;
  const name = devices.multi ? deviceName(t, devices.byId(devices.selected)) : status.settings?.device_name || deviceName(t, null);
  let kind = 'ok';
  let label;
  let sub = null;
  if (mode === 'local') {
    const pill = udpBlocked(status) ? 'blocked' : r.state;
    kind = pill === 'connected' ? 'ok' : pill === 'failed' ? 'bad' : pill === 'blocked' ? 'warn' : 'wait';
    label = t('conn.local');
    sub = t(`relay.pill.${pill}`);
  } else if (mode === 'remote') {
    kind = ms === null ? 'ok' : ms >= VERY_SLOW_MS ? 'bad' : ms >= SLOW_MS ? 'warn' : 'ok';
    label = t('conn.remote');
    sub = ms === null ? null : t('nav.remote_ms', { ms });
  } else {
    label = t('lan.badge');
  }
  return (
    <span className="relay-pill-wrap" ref={ref}>
      <button
        type="button"
        className={`conn-chip conn-${kind}`}
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={mode === 'remote' && ms !== null ? t('nav.remote_latency_hint', { ms }) : undefined}
      >
        <span className={`dot ${kind === 'ok' ? 'on' : kind}`} aria-hidden="true" />
        <span>{label}</span>
        {sub && <span className="conn-sub">· {sub}</span>}
      </button>
      {open && (
        <div className="relay-pop" role="dialog" aria-label={t('conn.title')}>
          {mode === 'local' && <LocalPop />}
          {mode === 'remote' && <RemotePop ms={ms} name={name} />}
          {mode === 'lan' && (
            <>
              <b className="conn-pop-title">{t('conn.lan_title', { name })}</b>
              <p className="muted small" style={{ margin: 0 }}>{t('lan.badge_hint')}</p>
            </>
          )}
        </div>
      )}
    </span>
  );
}
