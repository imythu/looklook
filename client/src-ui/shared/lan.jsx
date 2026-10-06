// 远程打开 ⇄ 局域网（docs/LAN_SWITCH.md，客户端 gateway/lan.rs）。
//
// - 远程页面（https，经中转）：问这台电脑能不能改走局域网、浏览器是不是在同一网络（公网 IP 相同）。
//   在同一网络时顶部提示“改用局域网”，终端页标题栏有“局域网”按钮：拿一次性票据，在新标签页打开
//   http://{局域网IP}:{端口}/…。用户勾了“自动切换”（记在这个浏览器里）时，以后打开远程页面直接跳过去。
// - 局域网页面（http://192.168.x.x，从远程页面切过来的）：每 5 秒探一次这台电脑，连续两次失败
//   （例如手机离开了家里的 Wi-Fi）就提示回到远程地址；自动切换模式下直接回去。
// https 页面不能嵌入或探测 http 的局域网地址（混合内容），所以是整页切换，不是 iframe。
import { Wifi, WifiOff, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { deviceApi } from './api.js';
import { Button, useToast } from './ui.jsx';
import { errorText } from './i18n.js';

const AUTO_KEY = 'looklook.lan-auto';
const STAY_KEY = 'looklook.lan-stay';
const HIDE_KEY = 'looklook.lan-hint-hidden';
const AUTO_HASH = '#ll-lan-auto';
const STAY_HASH = '#ll-stay';

function storage(kind) {
  try {
    return kind === 'session' ? window.sessionStorage : window.localStorage;
  } catch {
    return null;
  }
}
const read = (kind, k) => {
  try {
    return storage(kind)?.getItem(k) ?? null;
  } catch {
    return null;
  }
};
const write = (kind, k, v) => {
  try {
    if (v == null) storage(kind)?.removeItem(k);
    else storage(kind)?.setItem(k, v);
  } catch {
    /* 无痕模式：不记住 */
  }
};

export const lanAuto = () => read('local', AUTO_KEY) === '1';
export const setLanAuto = (on) => write('local', AUTO_KEY, on ? '1' : null);

// 从局域网页面退回来（#ll-stay）：这个标签页里不再自动跳去局域网，免得来回跳。
if (typeof location !== 'undefined' && location.hash === STAY_HASH) {
  write('session', STAY_KEY, '1');
  history.replaceState(null, '', location.pathname + location.search);
}

/** 远程打开时：这台（或指定的那台）电脑的局域网信息 `{enabled, same_network, urls}`；本机打开时为 null。 */
export function useLan(device) {
  const { status } = useStatus();
  const remote = status.access === 'remote';
  const [info, setInfo] = useState(null);
  useEffect(() => {
    if (!remote) return undefined;
    let alive = true;
    const load = () =>
      deviceApi(device)
        .get('/lan')
        .then((r) => alive && setInfo(r))
        // 旧版客户端没有这个接口
        .catch(() => alive && setInfo(null));
    load();
    const id = setInterval(load, 60000);
    window.addEventListener('focus', load);
    return () => {
      alive = false;
      clearInterval(id);
      window.removeEventListener('focus', load);
    };
  }, [remote, device]);
  return remote ? info : null;
}

/** 拿票据并打开局域网地址。`win`：点击时同步打开的空白标签页（避免被拦截）；没有则在当前页跳转。 */
async function go({ device, path, url, win, auto }) {
  const r = await deviceApi(device).post('/lan/ticket');
  const base = url && r.urls.includes(url) ? url : r.urls[0];
  const to = `${path}${auto ? AUTO_HASH : ''}`;
  const target = `${base}${r.path}?t=${encodeURIComponent(r.ticket)}&to=${encodeURIComponent(to)}`;
  if (win) win.location.href = target;
  else window.location.href = target;
}

/** 点“改用局域网”：新标签页打开。 */
export function useOpenLan(device) {
  const { t } = useTranslation();
  const toast = useToast();
  return async (path, url) => {
    const win = window.open('about:blank', '_blank');
    if (win) win.opener = null;
    try {
      await go({ device, path, url, win, auto: lanAuto() });
    } catch (e) {
      win?.close();
      toast(errorText(t, e), 'error');
    }
  };
}

/** 远程页面在同一网络时：开了自动切换就直接跳到局域网（当前页面）。只跳一次。 */
function useAutoSwitch(info, device, path) {
  const done = useRef(false);
  useEffect(() => {
    if (done.current || !info?.enabled || info.same_network !== true || !lanAuto() || read('session', STAY_KEY)) return;
    done.current = true;
    go({ device, path, auto: true }).catch(() => {});
  }, [info, device, path]);
}

/** 管理台顶部：在同一网络时提示改用局域网（可以关掉，可以勾“以后自动切换”）。 */
export function LanBanner({ device }) {
  const { t } = useTranslation();
  const info = useLan(device);
  const open = useOpenLan(device);
  const [hidden, setHidden] = useState(() => read('session', HIDE_KEY) === '1');
  const [auto, setAuto] = useState(lanAuto);
  useAutoSwitch(info, device, location.pathname);
  if (!info?.enabled || info.same_network !== true || hidden) return null;
  return (
    <div className="banner banner-info lan-banner" role="status">
      <Wifi size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">
        {t('lan.hint')}
        <label className="lan-auto">
          <input
            type="checkbox"
            checked={auto}
            onChange={(e) => {
              setLanAuto(e.target.checked);
              setAuto(e.target.checked);
            }}
          />
          {t('lan.auto')}
        </label>
      </span>
      <Button onClick={() => open(location.pathname)}>{t('lan.open')}</Button>
      <button
        type="button"
        className="btn btn-plain banner-x"
        aria-label={t('lan.dismiss')}
        title={t('lan.dismiss')}
        onClick={() => {
          write('session', HIDE_KEY, '1');
          setHidden(true);
        }}
      >
        <X size={16} />
      </button>
    </div>
  );
}

/** 终端页标题栏：同一网络（或不确定）时的“局域网”按钮，在新标签页里经局域网打开这个终端。 */
export function LanButton({ id, device }) {
  const { t } = useTranslation();
  const info = useLan(device);
  const open = useOpenLan(device);
  useAutoSwitch(info, device, `/t/${id}`);
  if (!info?.enabled || info.same_network === false) return null;
  return (
    <button type="button" className="btn btn-plain btn-icon" onClick={() => open(`/t/${id}`)} aria-label={t('lan.open_term')} title={t(info.same_network ? 'lan.open_term' : 'lan.open_term_unsure')}>
      <Wifi size={18} />
    </button>
  );
}

const LOOPBACK = /^(localhost|127\.|\[?::1\]?$)/i;

/** 这是从远程页面切过来的局域网页面吗（用局域网地址打开，且登录了看看账号、知道远程地址）。 */
export function useLanPage() {
  const { status } = useStatus();
  const remoteUrl = status.account.session?.user_host_url;
  const lan = status.access === 'local' && !LOOPBACK.test(location.hostname) && /^[\d.:[\]a-f]+$/i.test(location.hostname);
  return lan && remoteUrl ? remoteUrl.replace(/\/$/, '') : null;
}

/** 局域网页面：连不上这台电脑时提示回到远程地址；自动切换模式下直接回去。 */
export function LanFallback() {
  const { t } = useTranslation();
  const remote = useLanPage();
  const [lost, setLost] = useState(false);
  useEffect(() => {
    if (location.hash === AUTO_HASH) {
      setLanAuto(true);
      history.replaceState(null, '', location.pathname + location.search);
    }
  }, []);
  useEffect(() => {
    if (!remote) return undefined;
    let fails = 0;
    let alive = true;
    const check = async () => {
      if (document.hidden) return;
      const ctl = new AbortController();
      const timer = setTimeout(() => ctl.abort(), 3000);
      try {
        const r = await fetch('/api/ping', { signal: ctl.signal, cache: 'no-store' });
        if (!r.ok && r.status !== 404) throw new Error(String(r.status));
        fails = 0;
        if (alive) setLost(false);
      } catch {
        fails += 1;
        if (fails >= 2 && alive) {
          setLost(true);
          if (lanAuto() && navigator.onLine !== false) window.location.href = `${remote}${location.pathname}${STAY_HASH}`;
        }
      } finally {
        clearTimeout(timer);
      }
    };
    const id = setInterval(check, 5000);
    window.addEventListener('online', check);
    return () => {
      alive = false;
      clearInterval(id);
      window.removeEventListener('online', check);
    };
  }, [remote]);
  if (!remote || !lost) return null;
  return (
    <div className="banner banner-warn lan-lost" role="alert">
      <WifiOff size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">{t('lan.lost')}</span>
      <a className="btn btn-primary" href={`${remote}${location.pathname}${STAY_HASH}`}>
        {t('lan.back_remote')}
      </a>
    </div>
  );
}
