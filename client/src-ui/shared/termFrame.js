// 终端 iframe：地址与通信。终端始终同源打开（/i/{id}/）；远程打开时经 WebRTC，多台电脑时由启动页或本机客户端按 __lldev 转给终端所在的电脑。
import { useEffect, useState } from 'react';

import { selectedDevice, terminalDevice } from './api.js';

/** 终端 iframe 的地址。多台电脑时带上终端所在的电脑（__lldev，启动页 / 本机客户端自己去掉；ttyd 的 WebSocket 地址也会带上它）。 */
export function termSrc(id) {
  const device = terminalDevice(id) ?? selectedDevice();
  return device ? `/i/${id}/?__lldev=${encodeURIComponent(device)}` : `/i/${id}/`;
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

/**
 * 终端所在的电脑连不上了（`on`）/ 又能连上了：连接断着时终端不接受输入、不再显示它自己的“正在重新连接”（外面已经说明了）；
 * 能连上时马上重连，不用等退避。见 term-inject.js 的 setOffline。
 */
export function termOffline(frame, on) {
  frame.current?.contentWindow?.postMessage({ type: 'looklook:offline', data: Boolean(on) }, frameOrigin(frame));
}

/** 终端的连接状态（term-inject.js 发的 `looklook:conn`）：连着为 true，断开、重连中为 false。 */
export function useTermConn(frame) {
  const [ok, setOk] = useState(true);
  useEffect(() => {
    const on = (e) => {
      if (e.data?.type !== 'looklook:conn' || !frame.current || e.source !== frame.current.contentWindow) return;
      setOk(Boolean(e.data.ok));
    };
    window.addEventListener('message', on);
    return () => window.removeEventListener('message', on);
  }, []);
  return ok;
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
