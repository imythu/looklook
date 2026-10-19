// 本机直连（客户端 src/gateway/direct.rs）：远程打开管理台时，如果浏览器就开在终端所在的那台电脑上，
// 终端不绕中转，直接连 http://127.0.0.1:{端口}（回环地址不算混合内容，往返不到 1 毫秒）。
//
// 后台探测：经中转 POST /api/direct 拿一次性口令，再请求本机 /direct/ping；口令只有那台电脑自己认得，
// 所以通了就一定是同一台电脑。比经中转快才用（几乎总是）。
// 新版 Chrome 访问本机前会询问“是否允许访问本机/局域网设备”，为了不打扰别的电脑和手机上的用户，
// 只在电脑浏览器、且浏览器与终端所在电脑是同一种系统时才探测；用户拒绝过就不再探测。
//
// 局域网地址（192.168.x.x 等）做不到：https 页面里的 http 局域网请求会被浏览器当作混合内容拦下，
// 既探测不了也嵌不进 iframe。
import { useEffect, useState } from 'react';

import { deviceApi, selectedDevice, terminalDevice } from './api.js';
import i18n from './i18n.js';
import { isTouch } from './KeyBar.jsx';

const PING_TIMEOUT_MS = 1500;
/** 打开终端时最多等后台探测这么久，超时就先走中转。 */
const WAIT_MS = 1500;
/** 结果的有效期：过了以后窗口回到前台、打开终端时重新探测（笔记本可能换了网络、客户端可能重启了）。 */
const FRESH_MS = 60_000;

const config = { remote: false, oses: new Map() }; // 设备 → 系统（单设备时键为 ''）
const results = new Map(); // 设备 → { base, relayMs, directMs, at, pending }

const keyOf = (device) => device ?? '';

function osFamily(s) {
  const v = (s || '').toLowerCase();
  if (v.includes('android') || v.includes('ios')) return 'mobile';
  if (v.startsWith('win')) return 'windows';
  if (v === 'darwin' || v.startsWith('mac')) return 'macos';
  if (v.includes('cros') || v.includes('chrome os')) return 'chromeos';
  if (v.includes('linux')) return 'linux';
  return '';
}

function browserOs() {
  const p = navigator.userAgentData?.platform;
  if (p) return osFamily(p);
  const ua = navigator.userAgent || '';
  if (/Android|iPhone|iPad|iPod/i.test(ua)) return 'mobile';
  if (/CrOS/.test(ua)) return 'chromeos';
  if (/Windows/.test(ua)) return 'windows';
  if (/Macintosh|Mac OS X/.test(ua)) return 'macos';
  if (/Linux/.test(ua)) return 'linux';
  return '';
}

/** 这个浏览器有没有可能就开在那台电脑上（不确定时也试一试）。 */
function worthTrying(device) {
  if (!config.remote || isTouch()) return false;
  const b = browserOs();
  const d = osFamily(config.oses.get(keyOf(device)));
  if (b === 'mobile' || b === 'chromeos') return false;
  return !b || !d || b === d;
}

/** 用户在浏览器里拒绝过“访问本机网络”就不再探测（不支持这项权限的浏览器不会询问）。 */
async function permissionDenied() {
  for (const name of ['loopback-network', 'local-network-access']) {
    try {
      const s = await navigator.permissions.query({ name });
      return s.state === 'denied';
    } catch {
      /* 不认识这个权限名 */
    }
  }
  return false;
}

async function ping(url) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), PING_TIMEOUT_MS);
  const t0 = performance.now();
  try {
    const res = await fetch(url, { mode: 'cors', credentials: 'omit', cache: 'no-store', signal: ctl.signal });
    return res.status === 204 ? performance.now() - t0 : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

async function probe(device) {
  const t0 = performance.now();
  // 旧版客户端没有这个接口（404），当作不能直连。
  const r = await deviceApi(device).post('/direct').catch(() => null);
  const relayMs = Math.round(performance.now() - t0);
  if (!r?.available || !worthTrying(device) || (await permissionDenied())) return { base: null, relayMs: r ? relayMs : null, directMs: null };
  const base = `http://127.0.0.1:${r.port}`;
  const url = `${base}/direct/ping?n=${encodeURIComponent(r.nonce)}`;
  // 第一次含建连，测两次取快的。
  const a = await ping(url);
  const b = a === null ? null : await ping(url);
  const directMs = a === null ? null : Math.min(a, b ?? a);
  return { base: directMs !== null && directMs < relayMs ? base : null, relayMs, directMs: directMs === null ? null : Math.round(directMs) };
}

const subs = new Set();

/** 探测（或复用还新鲜的结果）；返回结果的 Promise。`force`：不管新不新鲜都重新探测（终端里点了刷新）。
 * 不可能直连的浏览器（手机等）也会经中转请求一次，只为测出中转的往返时间。 */
export function ensureDirect(device, { force = false } = {}) {
  const k = keyOf(device);
  const cur = results.get(k);
  if (cur?.pending) return cur.pending;
  if (!config.remote) return Promise.resolve(null);
  if (!force && cur && Date.now() - cur.at < FRESH_MS) return Promise.resolve(cur);
  const pending = probe(device)
    .catch(() => ({ base: null, relayMs: null }))
    .then((r) => {
      const done = { ...r, at: Date.now(), pending: null };
      results.set(k, done);
      subs.forEach((f) => f());
      return done;
    });
  results.set(k, { ...(cur ?? { base: null, relayMs: null, at: 0 }), pending });
  return pending;
}

/**
 * 由 App 在渲染时同步调用（幂等；子组件的 effect 先于 App 的运行，终端打开时就要知道是不是远程）。
 * `devices`：多设备时为 [{id, os, online}]；单设备时传 null，用 `os`。
 */
export function configureDirect({ remote, os, devices }) {
  config.remote = Boolean(remote);
  config.oses = new Map(devices ? devices.map((d) => [d.id, d.os]) : [['', os]]);
}

/** 由 App 在 effect 里调用：远程打开时在后台探测各台在线的电脑。 */
export function probeDirect(devices) {
  if (!config.remote) return;
  if (devices) devices.filter((d) => d.online).forEach((d) => ensureDirect(d.id));
  else ensureDirect(null);
}

function deviceOf(id) {
  return terminalDevice(id) ?? selectedDevice();
}

function build(id, r) {
  if (r?.base) {
    const hash = new URLSearchParams({ 'll-parent': location.origin, 'll-files': '1', 'll-lang': i18n.language || '' });
    return { src: `${r.base}/i/${id}/#${hash}`, direct: true };
  }
  // 多台电脑：告诉启动页（WebRTC 模式）或本机客户端（App）这个终端在哪台电脑（__lldev，它们自己去掉；ttyd 的 WebSocket 地址也会带上它）
  const device = deviceOf(id);
  return { src: device ? `/i/${id}/?__lldev=${encodeURIComponent(device)}` : `/i/${id}/`, direct: false };
}

/** 不用等探测就能决定的地址；要等时为 null。 */
function immediate(id) {
  const device = deviceOf(id);
  if (!config.remote || !worthTrying(device)) return build(id, null);
  const r = results.get(keyOf(device));
  return r && !r.pending && Date.now() - r.at < FRESH_MS ? build(id, r) : null;
}

/**
 * 终端 iframe 的地址：本机直连可用时是 http://127.0.0.1:{端口}/i/{id}/#…，否则同源 /i/{id}/。
 * 只在打开、刷新（reloadKey 变化）时决定，用着用着不会自己切换（会打断终端）；刷新时重新探测，
 * 所以本机客户端停了，点一下刷新就回到中转。还在探测时 src 为 null（最多等 WAIT_MS）。
 */
export function useTermSrc(id, reloadKey) {
  const [state, setState] = useState(() => immediate(id) ?? { src: null, direct: false });
  useEffect(() => {
    let alive = true;
    const device = deviceOf(id);
    const done = (r) => alive && setState(build(id, r));
    const p = ensureDirect(device, { force: reloadKey > 0 });
    const cur = results.get(keyOf(device));
    if (!config.remote || !worthTrying(device)) done(null);
    else if (!cur?.pending) done(cur);
    else {
      setState({ src: null, direct: false });
      const wait = new Promise((resolve) => setTimeout(resolve, WAIT_MS));
      Promise.race([p, wait]).then(() => {
        const r = results.get(keyOf(device));
        done(r?.pending ? null : r);
      });
    }
    return () => {
      alive = false;
    };
  }, [id, reloadKey]);
  return state;
}

/** 这个终端所在电脑的往返时间（毫秒）：经中转、直连本机；没测过为 null。 */
export function useLatency(id) {
  const [, tick] = useState(0);
  useEffect(() => {
    const f = () => tick((n) => n + 1);
    subs.add(f);
    return () => subs.delete(f);
  }, []);
  const r = config.remote ? results.get(keyOf(deviceOf(id))) : null;
  return { relayMs: r?.relayMs ?? null, directMs: r?.directMs ?? null };
}

// ---------------- 与终端 iframe 通信 ----------------
// 同源时直接调用 term-inject.js 提供的函数；本机直连时 iframe 跨源，改发消息。

export function frameOrigin(frame) {
  try {
    return new URL(frame.current?.getAttribute('src') ?? '', location.href).origin;
  } catch {
    return location.origin;
  }
}

function call(frame, fn, type, data, opts) {
  const w = frame.current?.contentWindow;
  if (!w) return;
  let f;
  try {
    f = w[fn];
  } catch {
    f = undefined; // 跨源
  }
  if (typeof f === 'function') f(data, opts);
  else w.postMessage({ type, data, opts }, frameOrigin(frame));
}

/** 向终端发送按键 / 文字。 */
export const termSend = (frame, data, opts) => call(frame, 'looklookSend', 'looklook:send', data, opts);
/** 往终端粘贴（上传完成后填路径）。 */
export const termPaste = (frame, data, opts) => call(frame, 'looklookPaste', 'looklook:paste', data, opts);

/**
 * 让终端拿到键盘焦点。光聚焦 iframe 不够：终端被隐藏过（工作台切到别的终端）时浏览器已把焦点从里面的输入框拿走，
 * 切回来后 iframe 有焦点、打字却没反应，所以还要聚焦终端本身（同源直接调用，跨源发消息）。
 */
export function termFocus(frame) {
  const w = frame.current?.contentWindow;
  if (!w) return;
  // 先把焦点放到 iframe 元素上：跨源（本机直连）时只调 contentWindow.focus() 有的浏览器不移动焦点
  frame.current.focus({ preventScroll: true });
  w.focus();
  let term = null;
  try {
    term = w.term;
  } catch {
    term = null; // 跨源
  }
  if (term?.focus) term.focus();
  else w.postMessage({ type: 'looklook:focus' }, frameOrigin(frame));
}

/**
 * 终端准备好时（term-inject.js 发 `looklook:ready`）聚焦它。iframe 的 load 事件比终端准备好早，那时聚焦可能落空；
 * 进入会话页、刷新终端后，当前显示的终端都能直接打字。`enabled`：工作台里只管当前显示的那个。
 */
export function useFocusWhenReady(frame, enabled = true) {
  useEffect(() => {
    if (!enabled) return undefined;
    const on = (e) => {
      if (e.data?.type !== 'looklook:ready' || !frame.current || e.source !== frame.current.contentWindow) return;
      // 别的输入框（对话框、搜索框）正在用时不抢焦点
      const el = document.activeElement;
      if (el && el !== frame.current && el.matches?.('input, textarea, select, [contenteditable="true"]')) return;
      termFocus(frame);
    };
    window.addEventListener('message', on);
    return () => window.removeEventListener('message', on);
  }, [enabled]);
}

export function termFit(frame) {
  frame.current?.contentWindow?.postMessage({ type: 'looklook:fit' }, frameOrigin(frame));
}

/**
 * 切到别的窗口 / 应用再回来时让终端重新适配、整屏重画（不然可能只画了一部分，见 term-inject.js 的 refitOnReturn）。
 * 终端页自己也会处理；这里补上焦点留在管理台（没点进终端）的情况。`enabled`：工作台里只管当前显示的那个。
 */
export function useRefitOnReturn(frame, enabled = true) {
  useEffect(() => {
    if (!enabled) return undefined;
    let timers = [];
    const later = () => {
      if (document.visibilityState !== 'visible') return;
      timers.forEach(clearTimeout);
      timers = [60, 400].map((ms) => setTimeout(() => termFit(frame), ms));
    };
    document.addEventListener('visibilitychange', later);
    window.addEventListener('pageshow', later);
    window.addEventListener('focus', later);
    return () => {
      timers.forEach(clearTimeout);
      document.removeEventListener('visibilitychange', later);
      window.removeEventListener('pageshow', later);
      window.removeEventListener('focus', later);
    };
  }, [frame, enabled]);
}
