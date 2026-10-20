// 手机上的下拉刷新：浏览器自带的在这里常常用不了（页面是固定高度、加到主屏幕的网页没有），自己做一个，所有页面一样。
// - 列表页：页面和手指下面能滚动的区域都在最上面时，往下拉超过一段距离、松开就刷新整页；
// - 终端页（整页终端 .term-page）不处理：终端正常时下拉是翻看终端；连不上、正在连接时由终端里注入的脚本处理，
//   它发来 looklook:pull-refresh，这里刷新整页（term-inject.js 的 setupPullRefresh）。
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { isTouch } from './KeyBar.jsx';

const LIMIT = 72;

/** 手指下面、往上能滚动的区域（不在最上面就不算下拉刷新）。 */
function scrolledAbove(el) {
  for (let n = el; n && n !== document.body && n !== document.documentElement; n = n.parentElement) {
    if (n.scrollTop > 0) {
      const oy = getComputedStyle(n).overflowY;
      if (oy === 'auto' || oy === 'scroll') return true;
    }
  }
  return window.scrollY > 0;
}

export default function PullRefresh() {
  const { t } = useTranslation();
  const [dist, setDist] = useState(0);
  const start = useRef(null);
  const distRef = useRef(0);
  useEffect(() => {
    const onMsg = (e) => {
      if (e.data?.type !== 'looklook:pull-refresh') return;
      if (![...document.querySelectorAll('iframe')].some((f) => f.contentWindow === e.source)) return;
      window.location.reload();
    };
    window.addEventListener('message', onMsg);
    if (!isTouch()) return () => window.removeEventListener('message', onMsg);
    const set = (v) => {
      distRef.current = v;
      setDist(v);
    };
    const down = (e) => {
      start.current = null;
      if (e.touches.length !== 1) return;
      const el = e.target;
      // 终端页、侧栏、弹窗、输入框里不处理
      if (el.closest?.('.term-page, .side.open, .modal-backdrop, input, textarea, [contenteditable="true"], .keybar') || scrolledAbove(el)) return;
      start.current = e.touches[0].clientY;
    };
    const move = (e) => {
      if (start.current === null) return;
      const d = e.touches[0].clientY - start.current;
      if (d <= 0) {
        if (distRef.current) set(0);
        return;
      }
      set(Math.min(d * 0.55, LIMIT * 1.4));
    };
    const up = () => {
      const go = start.current !== null && distRef.current >= LIMIT;
      start.current = null;
      set(0);
      if (go) window.location.reload();
    };
    document.addEventListener('touchstart', down, { passive: true });
    document.addEventListener('touchmove', move, { passive: true });
    document.addEventListener('touchend', up, { passive: true });
    document.addEventListener('touchcancel', up, { passive: true });
    return () => {
      window.removeEventListener('message', onMsg);
      document.removeEventListener('touchstart', down);
      document.removeEventListener('touchmove', move);
      document.removeEventListener('touchend', up);
      document.removeEventListener('touchcancel', up);
    };
  }, []);
  if (dist < 8) return null;
  const ready = dist >= LIMIT;
  return (
    <div className="pull" style={{ transform: `translate(-50%, ${dist / 2}px)`, opacity: Math.min(1, dist / LIMIT) }} role="status">
      <span className={`pull-arrow ${ready ? 'ready' : ''}`} aria-hidden="true">
        ↓
      </span>
      {t(ready ? 'pull.release' : 'pull.pull')}
    </div>
  );
}
