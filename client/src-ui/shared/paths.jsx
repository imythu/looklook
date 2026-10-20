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

/** 一台电脑的路径：`{kind, rtt_ms, local, remote}`；没连着为 null。 */
export function usePath(device) {
  const all = usePaths();
  return (device && all[device]) || null;
}

/** 电脑菜单每行右侧：色点加延迟；悬停（手机上长按）看完整说明。`self`：本机 App 里的本机，只写“本机”。 */
export function PathDot({ device, self = false }) {
  const { t } = useTranslation();
  const p = usePath(self ? null : device);
  if (self) return <span className="path-dot muted">{t('path.self')}</span>;
  const kind = p?.kind ?? 'none';
  const ms = p?.rtt_ms;
  const tip = p ? [t(`path.${kind}`), p.remote?.split(' ')[0], ms != null ? `${ms} ms` : null].filter(Boolean).join(' · ') : t('path.none');
  return (
    <span className="path-dot" title={tip} aria-label={tip}>
      <span className={`dot path-${kind}`} aria-hidden="true" />
      {ms != null ? `${ms} ms` : '—'}
    </span>
  );
}
