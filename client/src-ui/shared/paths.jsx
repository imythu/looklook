// 连着的电脑实际走的路与往返时间（电脑菜单每行右侧的色点）：
// 绿 = 局域网直连，蓝 = 公网直连，黄 = 中转，灰 = 还没连（不会为了显示它去建连接）。
// 网页（WebRTC）由启动页的 PeerConnection 统计给出（server-web/rtc/boot.js 的 __llrtc.paths）；
// 本机 App 连其他电脑由客户端给出（GET /api/rtc/paths，src/rtc/outbound.rs）。本机自己不走 WebRTC，只写“本机”。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { localApi, rtcMode } from './api.js';

const POLL_MS = 10_000;
let data = {};
let timer = 0;
let users = 0;
const subs = new Set();

async function load() {
  try {
    if (rtcMode) data = (await window.top.__llrtc?.paths?.()) ?? {};
    else data = (await localApi.get('/rtc/paths'))?.items ?? {};
  } catch {
    /* 拿不到就保留上一次的 */
  }
  subs.forEach((f) => f());
}

function tick() {
  if (document.visibilityState === 'visible') load();
}

/** 订阅路径数据：有人在看时每 10 秒刷新一次，页面在后台时不刷新。 */
export function usePaths() {
  const [, setN] = useState(0);
  useEffect(() => {
    const f = () => setN((n) => n + 1);
    subs.add(f);
    if (users++ === 0) {
      load();
      timer = setInterval(tick, POLL_MS);
      document.addEventListener('visibilitychange', tick);
    }
    return () => {
      subs.delete(f);
      if (--users === 0) {
        clearInterval(timer);
        document.removeEventListener('visibilitychange', tick);
      }
    };
  }, []);
  return data;
}

/** 一台电脑的路径：`{kind, rtt_ms, loss, local, remote}`；没连着为 null。`loss` 是 0–1 的丢包率，对端是旧版或样本还少时为 null。 */
export function usePath(device) {
  const all = usePaths();
  return (device && all[device]) || null;
}

/** 丢包率的文字（“2%”）；没有数据为 null。 */
export function lossText(loss) {
  return loss == null ? null : `${Math.round(loss * 100)}%`;
}

/** 丢包率的严重程度：5% 起偏高（warn），15% 起很差（bad），其余 ok；没有数据为 null。 */
export function lossLevel(loss) {
  if (loss == null) return null;
  return loss >= 0.15 ? 'bad' : loss >= 0.05 ? 'warn' : 'ok';
}

function pathTip(t, p) {
  if (!p) return t('path.none');
  const loss = lossText(p.loss);
  return [t(`path.${p.kind}`), p.remote?.split(' ')[0], p.rtt_ms != null ? `${p.rtt_ms} ms` : null, loss ? t('path.loss', { loss }) : null].filter(Boolean).join(' · ');
}

/** 电脑菜单每行右侧：色点加延迟；悬停看完整说明（手机上看行里的 [PathLine]）。`self`：本机 App 里的本机，只写“本机”。 */
export function PathDot({ device, self = false }) {
  const { t } = useTranslation();
  const p = usePath(self ? null : device);
  if (self) return <span className="path-dot muted">{t('path.self')}</span>;
  const kind = p?.kind ?? 'none';
  const ms = p?.rtt_ms;
  const tip = pathTip(t, p);
  return (
    <span className="path-dot" title={tip} aria-label={tip}>
      <span className={`dot path-${kind}`} aria-hidden="true" />
      {ms != null ? `${ms} ms` : '—'}
    </span>
  );
}

/** 电脑菜单行里的第三行：走的路、丢包率、对端地址。手机没有悬停，靠这一行看；没连着不显示。 */
export function PathLine({ device }) {
  const { t } = useTranslation();
  const p = usePath(device);
  if (!p) return null;
  const loss = lossText(p.loss);
  const addr = p.remote?.split(' ')[0];
  // 地址可能很长（IPv6），放最后，窄屏截断时先截它
  return (
    <span className="small muted path-line">
      {t(`path.${p.kind}`)}
      {' · '}
      <span className={loss ? `loss-${lossLevel(p.loss)}` : undefined}>{t('path.loss', { loss: loss ?? '—' })}</span>
      {addr && ` · ${addr}`}
    </span>
  );
}

// 网页（WebRTC）里到各台电脑的连接状态（boot.js 的 __llrtc.links）。平台只说电脑在不在线（它与平台的长连接），
// 网页自己连不连得上要看这里：连接中 / 在线 / 重连中 / 连不上。本机 App 没有这一层，只看平台的在线。
const LINK_POLL_MS = 2000;
let links = {};
let linkTimer = 0;
let linkUsers = 0;
const linkSubs = new Set();

function loadLinks() {
  if (document.visibilityState !== 'visible') return;
  let next = {};
  try {
    next = window.top.__llrtc?.links?.() ?? {};
  } catch {
    /* 旧的启动页没有 links */
  }
  if (JSON.stringify(next) === JSON.stringify(links)) return;
  links = next;
  linkSubs.forEach((f) => f());
}

/** 订阅各台电脑的连接状态：`{设备 id: {s, reason, path}}`，有人在看时每 2 秒刷新。 */
export function useLinks() {
  const [, setN] = useState(0);
  useEffect(() => {
    if (!rtcMode) return undefined;
    const f = () => setN((n) => n + 1);
    linkSubs.add(f);
    if (linkUsers++ === 0) {
      loadLinks();
      linkTimer = setInterval(loadLinks, LINK_POLL_MS);
    }
    return () => {
      linkSubs.delete(f);
      if (--linkUsers === 0) clearInterval(linkTimer);
    };
  }, []);
  return links;
}

/**
 * 一台电脑给用户看的状态：`offline`（平台说它不在线）、`connecting`、`reconnecting`、`failed`（连不上），
 * 其余为 `online`（含还没去连的）。
 */
export function linkState(device, all) {
  if (!device?.online) return 'offline';
  return all[device.id]?.s ?? 'online';
}

const STATE_KEY = { offline: 'devices.offline', online: 'devices.online', connecting: 'devices.connecting', reconnecting: 'devices.reconnecting', failed: 'devices.unreachable' };

/** 状态的文字（“在线”“连接中…”“连不上”等）。 */
export function linkLabel(t, device, all) {
  return t(STATE_KEY[linkState(device, all)]);
}

/** 连不上时的原因（error 文案里的 RTC_* 等）与重试。 */
export function linkFailure(device, all) {
  const l = linkState(device, all) === 'failed' ? all[device.id] : null;
  if (!l) return null;
  return { reason: l.reason || '', retry: () => window.top.__llrtc?.retry?.(device.id) };
}
