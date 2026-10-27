// 对话事件怎么收（gateway/chat_mux.rs）：
// - 本机明文 http 打开的管理台（本机 App、局域网）：整个页面一条多路 WebSocket（/api/chat/ws），所有对话按终端订阅、退订。
//   明文 http 浏览器只用 HTTP/1.1，同一地址最多 6 条连接、所有窗口共用；每个对话一条 SSE 时，挂着的对话一多，
//   发消息、停止、新开这些请求都会在浏览器里排队，界面点不动。WebSocket 不占这 6 条。
// - 远程打开（平台 https 的 HTTP/2、WebRTC 经 Service Worker）：没有这个限制，每个对话一条 SSE（和以前一样）。
// - WebSocket 一直连不上（连续几次一次都没连上）：退回 SSE，但只给显示着的对话开（mode 为 'fallback'），保证占不满。

import { rtcMode, terminalDevice } from './api.js';

const RETRY = [1000, 2000, 5000, 10000];
/** 订阅出错、结束后过多久重新订阅 */
const RESUB = 5000;
/** 一次都没连上过、连续失败几次后退回 SSE */
const GIVE_UP = 3;

const subs = new Map(); // id -> Set<{ event(type, data), down() }>
const resub = new Map(); // id -> timer
const modeListeners = new Set();
let ws = null;
let open = false;
let everOpened = false;
let failures = 0;
let retryTimer = null;
let mode = (() => {
  try {
    return location.protocol === 'http:' && !rtcMode && typeof WebSocket === 'function' ? 'mux' : 'sse';
  } catch {
    return 'sse';
  }
})();

/** 'mux'：多路 WebSocket；'sse'：每个对话一条 SSE；'fallback'：WebSocket 用不了，只给显示着的对话开 SSE */
export function chatEventsMode() {
  return mode;
}

export function onChatEventsMode(fn) {
  modeListeners.add(fn);
  return () => modeListeners.delete(fn);
}

function send(msg) {
  if (open) ws.send(JSON.stringify(msg));
}

function subMsg(id) {
  const device = terminalDevice(id);
  return device ? { op: 'sub', id, device } : { op: 'sub', id };
}

function each(id, fn) {
  for (const h of subs.get(id) ?? []) fn(h);
}

function connect() {
  clearTimeout(retryTimer);
  retryTimer = null;
  ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/chat/ws`);
  ws.onopen = () => {
    open = true;
    everOpened = true;
    failures = 0;
    for (const id of subs.keys()) send(subMsg(id));
  };
  ws.onmessage = (e) => {
    let m;
    try {
      m = JSON.parse(e.data);
    } catch {
      return;
    }
    if (!m || typeof m.id !== 'string' || !subs.has(m.id)) return;
    if (m.event === 'error') {
      // 订阅不成（终端没了、电脑离线……）或者结束了：界面去取一次快照拿到原因，过一会儿重新订阅
      each(m.id, (h) => h.down());
      clearTimeout(resub.get(m.id));
      resub.set(
        m.id,
        setTimeout(() => {
          resub.delete(m.id);
          if (subs.has(m.id)) send(subMsg(m.id));
        }, RESUB),
      );
      return;
    }
    each(m.id, (h) => h.event(m.event, m.data));
  };
  ws.onclose = () => {
    const was = open;
    open = false;
    ws = null;
    if (was) for (const id of subs.keys()) each(id, (h) => h.down());
    if (!subs.size) return;
    failures += 1;
    if (!everOpened && failures >= GIVE_UP) {
      mode = 'fallback';
      for (const fn of modeListeners) fn(mode);
      return;
    }
    retryTimer = setTimeout(connect, RETRY[Math.min(failures - 1, RETRY.length - 1)]);
  };
}

/**
 * 订阅一个对话的事件（mode 为 'mux' 时用）。`h.event(type, data)`：snapshot / status / items / reset；
 * `h.down()`：暂时收不到了（断线、订阅出错），会自己重连、重新订阅，重新订阅后先来一份 snapshot。返回退订函数。
 */
export function subscribeChat(id, h) {
  let set = subs.get(id);
  if (!set) {
    set = new Set();
    subs.set(id, set);
  }
  set.add(h);
  if (set.size === 1) {
    if (ws) send(subMsg(id));
    else if (!retryTimer) connect();
  } else if (open) {
    // 同一个对话第二个订阅者（同一页面里开了两处）：重新订阅，大家都拿到一份快照
    send(subMsg(id));
  }
  return () => {
    set.delete(h);
    if (set.size) return;
    subs.delete(id);
    clearTimeout(resub.get(id));
    resub.delete(id);
    send({ op: 'unsub', id });
  };
}
