// 终端页：整页显示一个终端（同源 iframe 打开 /i/{id}/）。手机上底部有按键条（Esc、Tab、Ctrl/Alt/Shift、方向键、组合键、符号、F1–F12…）。
import { ArrowLeft, ChevronDown, ChevronUp, ExternalLink, GripHorizontal, Keyboard, Maximize2, Minimize2, Play, RefreshCw, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import KeyBar, { isTouch, useKeybarShown, useVisibleViewport } from '../shared/KeyBar.jsx';
import { useRouter } from '../shared/router.jsx';
import { Button, Card, useLoad, useToast } from '../shared/ui.jsx';
import { useTermFiles } from './TermFiles.jsx';

/** 终端页面加载期间的提示：跳动的小点 + 一句人话；等得久了给出刷新按钮。 */
function TermConnecting({ onRetry }) {
  const { t } = useTranslation();
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setSlow(true), 8000);
    return () => clearTimeout(timer);
  }, []);
  return (
    <div className="term-connecting" role="status" aria-live="polite">
      <div>
        <span className="dots" aria-hidden="true">
          <i />
          <i />
          <i />
        </span>
        <p>{t('termconn.connecting')}</p>
        <p className="small">{t(slow ? 'termconn.slow' : 'termconn.connecting_hint')}</p>
        {slow && (
          <Button variant="ghost" onClick={onRetry}>
            {t('termconn.retry')}
          </Button>
        )}
      </div>
    </div>
  );
}

const EMBED_MIN_W = 320;
const EMBED_MIN_H = 200;
const fit = (frame) => frame.current?.contentWindow?.postMessage({ type: 'looklook:fit' }, location.origin);
const clamp = (v, lo, hi) => Math.min(Math.max(v, lo), Math.max(lo, hi));
let cascade = 0;

/** 悬浮窗初始位置/大小：每开一个错开一点，避免完全重叠。 */
function initialRect() {
  const w = Math.min(820, window.innerWidth - 24);
  const h = Math.min(520, window.innerHeight - 80);
  const off = (cascade++ % 6) * 28;
  return { x: clamp(Math.round((window.innerWidth - w) / 2) + off, 0, window.innerWidth - w), y: clamp(64 + off, 0, window.innerHeight - h), w, h };
}

/**
 * 悬浮终端：在当前页面上方以可拖动、可缩放的小窗口显示 /i/{id}/，不改变当前标签页地址。
 * 拖动标题栏移动，拖右下角（或按 ↑ ↓ ← → 调整）缩放，支持折叠和全屏。
 */
export function EmbeddedTerminal({ id, inst, onClose, onStart }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const wrap = useRef(null);
  const frame = useRef(null);
  const drag = useRef(null);
  const [rect, setRect] = useState(initialRect);
  const [collapsed, setCollapsed] = useState(false);
  const [full, setFull] = useState(false);
  const [moving, setMoving] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [starting, setStarting] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [touch] = useState(isTouch);
  useEffect(() => {
    const on = () => {
      if (!document.fullscreenElement) setFull(false);
    };
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  useEffect(() => {
    if (!full) return undefined;
    // 浏览器不支持原生全屏时（如 iPhone）退回页面内全屏，Esc 退出。
    const esc = (e) => e.key === 'Escape' && !document.fullscreenElement && setFull(false);
    document.addEventListener('keydown', esc);
    return () => document.removeEventListener('keydown', esc);
  }, [full]);
  // 浏览器窗口变小时把悬浮窗拉回可见范围。
  useEffect(() => {
    const on = () =>
      setRect((r) => {
        const w = Math.min(r.w, window.innerWidth);
        const h = Math.min(r.h, window.innerHeight);
        return { w, h, x: clamp(r.x, 0, window.innerWidth - w), y: clamp(r.y, 0, window.innerHeight - h) };
      });
    window.addEventListener('resize', on);
    return () => window.removeEventListener('resize', on);
  }, []);
  // 尺寸变化后让终端重新适配。
  useEffect(() => {
    const timer = setTimeout(() => fit(frame), 50);
    return () => clearTimeout(timer);
  }, [full, collapsed]);
  const toggleFull = () => {
    if (full) {
      setFull(false);
      if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
    } else {
      setFull(true);
      wrap.current?.requestFullscreen?.().catch(() => {});
    }
  };
  const begin = (mode) => (e) => {
    if (full || e.button > 0 || e.target.closest?.('button')) return;
    drag.current = { mode, px: e.clientX, py: e.clientY, r: rect };
    e.currentTarget.setPointerCapture?.(e.pointerId);
    setMoving(true);
  };
  const onMove = (e) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.px;
    const dy = e.clientY - d.py;
    if (d.mode === 'move') {
      setRect({ ...d.r, x: clamp(d.r.x + dx, -d.r.w + 80, window.innerWidth - 80), y: clamp(d.r.y + dy, 0, window.innerHeight - 40) });
    } else {
      setRect({ ...d.r, w: clamp(d.r.w + dx, EMBED_MIN_W, window.innerWidth - d.r.x), h: clamp(d.r.h + dy, EMBED_MIN_H, window.innerHeight - d.r.y) });
    }
  };
  const onUp = () => {
    if (drag.current?.mode === 'size') fit(frame);
    drag.current = null;
    setMoving(false);
  };
  const onKey = (e) => {
    const step = { ArrowUp: [0, -40], ArrowDown: [0, 40], ArrowLeft: [-40, 0], ArrowRight: [40, 0] }[e.key];
    if (!step) return;
    e.preventDefault();
    setRect((r) => ({ ...r, w: clamp(r.w + step[0], EMBED_MIN_W, window.innerWidth - r.x), h: clamp(r.h + step[1], EMBED_MIN_H, window.innerHeight - r.y) }));
    setTimeout(() => fit(frame), 50);
  };
  const start = async () => {
    setStarting(true);
    try {
      await onStart?.();
    } finally {
      setStarting(false);
    }
  };
  const running = inst?.running;
  const viewport = useVisibleViewport(touch && full);
  const files = useTermFiles({ id, inst, frame, enabled: Boolean(running && status.account.allowed), touch, hostOs: status.capabilities.os, keybar: Boolean(running && touch && full) });
  const shell = full
    ? { position: 'fixed', inset: 0, zIndex: 60, display: 'flex', flexDirection: 'column', ...viewport }
    : {
        position: 'fixed',
        left: rect.x,
        top: rect.y,
        width: rect.w,
        zIndex: 55,
        display: 'flex',
        flexDirection: 'column',
        borderRadius: 12,
        overflow: 'hidden',
        border: '1px solid #3a4152',
        boxShadow: '0 12px 40px rgba(0,0,0,.45)',
      };
  const iconBtn = 'btn btn-plain btn-icon';
  return (
    <section ref={wrap} className="embedded-term" style={{ background: '#11131a', ...shell }} aria-label={inst?.name ?? id}>
      <header
        className="term-bar"
        style={full ? undefined : { paddingTop: 4, minHeight: 44, cursor: moving ? 'grabbing' : 'grab', touchAction: 'none', userSelect: 'none' }}
        onPointerDown={begin('move')}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={onUp}
        onDoubleClick={(e) => !full && !e.target.closest('button') && setCollapsed((c) => !c)}
      >
        <span className="term-title">
          <span className={`dot ${running ? 'on' : ''}`} />
          <span className="ellipsis">{inst?.name ?? id}</span>
        </span>
        <button type="button" className={iconBtn} onClick={() => { setLoaded(false); setReloadKey((k) => k + 1); }} aria-label={t('term.reload')} title={t('term.reload')}>
          <RefreshCw size={17} />
        </button>
        {files.button}
        <button type="button" className={iconBtn} onClick={() => window.open(`/t/${id}`, '_blank', 'noopener')} aria-label={t('instance.new_window')} title={t('instance.new_window')}>
          <ExternalLink size={17} />
        </button>
        {!full && (
          <button type="button" className={iconBtn} onClick={() => setCollapsed(!collapsed)} aria-expanded={!collapsed} aria-label={t(collapsed ? 'embed.expand' : 'embed.collapse')} title={t(collapsed ? 'embed.expand' : 'embed.collapse')}>
            {collapsed ? <ChevronDown size={19} /> : <ChevronUp size={19} />}
          </button>
        )}
        <button type="button" className={iconBtn} onClick={toggleFull} aria-label={t(full ? 'term.exit_full' : 'term.full')} title={t(full ? 'term.exit_full' : 'term.full')}>
          {full ? <Minimize2 size={17} /> : <Maximize2 size={17} />}
        </button>
        {!full && (
          <button type="button" className={iconBtn} onClick={onClose} aria-label={t('embed.close')} title={t('embed.close')}>
            <X size={18} />
          </button>
        )}
      </header>
      <div style={{ display: collapsed && !full ? 'none' : 'block', position: 'relative', height: full ? undefined : rect.h - 44, flex: full ? 1 : undefined, minHeight: 0 }}>
        {running && !loaded && <TermConnecting onRetry={() => { setLoaded(false); setReloadKey((k) => k + 1); }} />}
        {running ? (
          <iframe
            key={reloadKey}
            ref={frame}
            src={`/i/${id}/`}
            title={inst.name}
            allow="clipboard-read; clipboard-write; fullscreen"
            // 拖动/缩放时 iframe 会吞掉指针事件，期间先让它穿透。
            style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', border: 0, background: '#11131a', pointerEvents: moving ? 'none' : 'auto' }}
            onLoad={() => {
              setLoaded(true);
              frame.current?.contentWindow?.focus();
            }}
          />
        ) : (
          <div style={{ position: 'absolute', inset: 0, display: 'grid', placeItems: 'center', padding: 16, color: '#c5cad6', textAlign: 'center' }}>
            <div>
              <p>{t('term.stopped_body')}</p>
              <Button icon={Play} busy={starting} onClick={start}>
                {t('instance.start_open')}
              </Button>
            </div>
          </div>
        )}
        {!full && !collapsed && (
          <div
            role="separator"
            aria-label={t('embed.resize')}
            tabIndex={0}
            title={t('embed.resize')}
            onPointerDown={begin('size')}
            onPointerMove={onMove}
            onPointerUp={onUp}
            onPointerCancel={onUp}
            onKeyDown={onKey}
            style={{ position: 'absolute', right: 0, bottom: 0, width: 22, height: 22, display: 'grid', placeItems: 'end', cursor: 'nwse-resize', touchAction: 'none', color: '#8a91a3', zIndex: 2 }}
          >
            <GripHorizontal size={14} aria-hidden="true" style={{ transform: 'rotate(-45deg)' }} />
          </div>
        )}
        {files.layer}
      </div>
      {/* 手机上悬浮小窗太挤，全屏时才显示按键条 */}
      {running && touch && full && <KeyBar frame={frame} os={status.capabilities.os} />}
    </section>
  );
}

export default function TerminalView({ id }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const toast = useToast();
  const frame = useRef(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [touch] = useState(isTouch);
  const [keys, setKeys] = useKeybarShown();
  const viewport = useVisibleViewport(touch);
  const [full, setFull] = useState(false);
  const [starting, setStarting] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [inst, error, reload] = useLoad(() => api.get(`/instances/${id}`), [id]);
  const allowed = status.account.allowed;
  const running = inst?.running;
  const files = useTermFiles({ id, inst, frame, enabled: Boolean(running && allowed), touch, hostOs: status.capabilities.os, keybar: Boolean(touch && keys && running && allowed) });

  useEffect(() => {
    const iv = setInterval(reload, 5000);
    return () => clearInterval(iv);
  }, []);
  useEffect(() => {
    if (inst) document.title = `${inst.name} · ${t('brand')}`;
    return () => {
      document.title = t('brand');
    };
  }, [inst?.name]);
  useEffect(() => {
    const on = () => setFull(Boolean(document.fullscreenElement));
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  // 终端所在页面尺寸变化（按键条显示/隐藏）时让终端重新适配。
  useEffect(() => {
    frame.current?.contentWindow?.postMessage({ type: 'looklook:fit' }, location.origin);
  }, [keys]);

  const start = async () => {
    setStarting(true);
    try {
      await api.post(`/instances/${id}/start`);
      reload();
      setReloadKey((k) => k + 1);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setStarting(false);
    }
  };
  const toggleFull = () => (document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen?.());
  const light = status.settings.theme === 'light';

  let body;
  if (error) {
    body = (
      <div className="term-empty">
        <Card title={t('term.missing_title')}>
          <p className="muted">{errorText(t, error)}</p>
          <Button onClick={() => navigate('/')}>{t('term.back')}</Button>
        </Card>
      </div>
    );
  } else if (!allowed) {
    body = (
      <div className="term-empty">
        <Card title={t('term.locked_title')}>
          <p className="muted">{t(`gate.${status.account.reason}`, { defaultValue: t('gate.other') })}</p>
          <Button onClick={() => navigate('/')}>{t('term.back')}</Button>
        </Card>
      </div>
    );
  } else if (inst && !running) {
    body = (
      <div className="term-empty">
        <Card title={t('term.stopped_title')}>
          <p className="muted">{t('term.stopped_body')}</p>
          <Button icon={Play} busy={starting} onClick={start}>
            {t('instance.start_open')}
          </Button>
        </Card>
      </div>
    );
  } else if (inst) {
    body = (
      <>
        {!loaded && <TermConnecting onRetry={() => { setLoaded(false); setReloadKey((k) => k + 1); }} />}
        <iframe
          key={reloadKey}
          ref={frame}
          src={`/i/${id}/`}
          title={inst.name}
          allow="clipboard-read; clipboard-write; fullscreen"
          onLoad={() => {
            setLoaded(true);
            frame.current?.contentWindow?.focus();
          }}
        />
      </>
    );
  }

  return (
    <div className="term-page" style={{ '--term-bg': light ? '#fbfbfd' : '#11131a', ...viewport }}>
      <header className="term-bar">
        <button type="button" className="btn btn-plain btn-icon" onClick={() => navigate('/')} aria-label={t('term.back')}>
          <ArrowLeft size={20} />
        </button>
        <span className="term-title">
          <span className={`dot ${running && allowed ? 'on' : ''}`} />
          <span className="ellipsis">{inst?.name ?? '…'}</span>
        </span>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => { setLoaded(false); setReloadKey((k) => k + 1); }} aria-label={t('term.reload')} title={t('term.reload')}>
          <RefreshCw size={18} />
        </button>
        {files.button}
        {touch && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => setKeys(!keys)} aria-pressed={keys} aria-label={t('term.keys')} title={t('term.keys')}>
            <Keyboard size={19} />
          </button>
        )}
        <button type="button" className="btn btn-plain btn-icon" onClick={toggleFull} aria-label={t(full ? 'term.exit_full' : 'term.full')} title={t(full ? 'term.exit_full' : 'term.full')}>
          {full ? <Minimize2 size={18} /> : <Maximize2 size={18} />}
        </button>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => window.open(`/t/${id}`, '_blank', 'noopener')} aria-label={t('instance.new_window')} title={t('instance.new_window')}>
          <ExternalLink size={18} />
        </button>
      </header>
      <div className="term-frame">
        {body}
        {files.layer}
      </div>
      {touch && keys && running && allowed && <KeyBar frame={frame} os={status.capabilities.os} />}
    </div>
  );
}
