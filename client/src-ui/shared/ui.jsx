// 基础组件：按钮、输入、开关、卡片、提示、弹窗、确认框、轻提示、骨架屏、空状态、时间。
import { AlertTriangle, Check, CheckCircle2, ChevronDown, Copy, Eye, EyeOff, Info, Languages, Loader2, XCircle } from 'lucide-react';
import qrcode from 'qrcode-generator';
import { createContext, useCallback, useContext, useEffect, useId, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { errorText, setLocale } from './i18n.js';
import { formatTime, relativeTime } from './time.js';

// 品牌标志：两只眼睛（看看 / Looklook 的 oo），瞳孔拼成终端提示符 >_，右上角的点是“在线”指示灯。
// 与 assets/icon-client/icon.svg 同源（客户端用绿色，服务端网页用靛蓝，方便一眼区分标签页）；gradient id 用 useId 避免同页多个实例冲突。
export function BrandMark({ size = 32 }) {
  const id = useId();
  return (
    <svg className="brand-mark" width={size} height={size} viewBox="0 0 512 512" aria-hidden="true">
      <defs>
        <linearGradient id={`${id}bg`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#10b981" />
          <stop offset=".6" stopColor="#047857" />
          <stop offset="1" stopColor="#064e3b" />
        </linearGradient>
        <radialGradient id={`${id}gl`} cx=".28" cy=".12" r=".75">
          <stop offset="0" stopColor="#fff" stopOpacity=".26" />
          <stop offset="1" stopColor="#fff" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect width="512" height="512" rx="116" fill={`url(#${id}bg)`} />
      <rect width="512" height="512" rx="116" fill={`url(#${id}gl)`} />
      <circle cx="160" cy="268" r="92" fill="#fff" />
      <circle cx="352" cy="268" r="92" fill="#fff" />
      <path d="M152 232 L192 268 L152 304" fill="none" stroke="#022c22" strokeWidth="30" strokeLinecap="round" strokeLinejoin="round" />
      <rect x="330" y="290" width="68" height="28" rx="14" fill="#022c22" />
      <circle cx="416" cy="118" r="22" fill="#fbbf24" />
    </svg>
  );
}

// 中英文切换：按钮上显示“另一种”语言的名字。
export function LangSwitch() {
  const { i18n } = useTranslation();
  const en = i18n.language?.startsWith('en');
  return (
    <button type="button" className="btn btn-plain lang-switch" onClick={() => setLocale(en ? 'zh-CN' : 'en-US')} lang={en ? 'zh-CN' : 'en'}>
      <Languages size={16} aria-hidden="true" />
      {en ? '中文' : 'EN'}
    </button>
  );
}

/** 二维码（SVG）。始终深色码点、白底，深色模式下手机也能扫。 */
export function QrCode({ text, size = 148, label }) {
  const { n, d } = useMemo(() => {
    const qr = qrcode(0, 'M');
    qr.addData(text);
    qr.make();
    const count = qr.getModuleCount();
    let path = '';
    for (let r = 0; r < count; r += 1) {
      for (let c = 0; c < count; c += 1) if (qr.isDark(r, c)) path += `M${c + 2} ${r + 2}h1v1h-1z`;
    }
    return { n: count + 4, d: path };
  }, [text]);
  return (
    <svg className="qr" width={size} height={size} viewBox={`0 0 ${n} ${n}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect width={n} height={n} fill="#fff" />
      <path d={d} fill="#12141c" />
    </svg>
  );
}

export function Spinner({ size = 18 }) {
  return <Loader2 size={size} className="spin" aria-hidden="true" />;
}

export function Button({ variant = 'primary', size, block, busy, icon: Icon, className = '', children, ...rest }) {
  const cls = ['btn', `btn-${variant}`, size === 'big' && 'btn-big', block && 'btn-block', !children && 'btn-icon', className].filter(Boolean).join(' ');
  return (
    <button type="button" className={cls} {...rest} disabled={busy || rest.disabled} aria-busy={busy || undefined}>
      {busy ? <Spinner /> : Icon && <Icon size={18} aria-hidden="true" />}
      {children}
    </button>
  );
}

export function Field({ label, hint, error, children }) {
  return (
    <label className="field">
      {label && <span className="field-label">{label}</span>}
      {children}
      {error ? <span className="field-error">{error}</span> : hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

export function PasswordInput(props) {
  const { t } = useTranslation();
  const [show, setShow] = useState(false);
  return (
    <span className="input-wrap">
      <input type={show ? 'text' : 'password'} {...props} />
      <button type="button" className="input-btn" onClick={() => setShow(!show)} aria-label={show ? t('action.hide') : t('action.show')}>
        {show ? <EyeOff size={18} /> : <Eye size={18} />}
      </button>
    </span>
  );
}

export function Switch({ checked, onChange, disabled, label }) {
  return <button type="button" role="switch" className="switch" aria-checked={checked} aria-label={label} disabled={disabled} onClick={() => onChange(!checked)} />;
}

export function Card({ title, actions, children, className = '' }) {
  return (
    <section className={`card ${className}`}>
      {(title || actions) && (
        <header className="card-head">
          {title && <h2>{title}</h2>}
          {actions && <div className="card-actions">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

const NOTE_ICON = { error: XCircle, warn: AlertTriangle, ok: CheckCircle2, info: Info };

export function Note({ kind = 'info', icon, children }) {
  const Icon = icon ?? NOTE_ICON[kind] ?? Info;
  return (
    <div className={`note note-${kind}`} role={kind === 'error' ? 'alert' : undefined}>
      <Icon className="icon" size={18} aria-hidden="true" />
      <div className="grow">{children}</div>
    </div>
  );
}

/** 一行提示，点开看全文：给次要但需要知道的提醒，不占主要内容的位置。 */
export function FoldNote({ kind = 'info', icon, summary, children }) {
  const Icon = icon ?? NOTE_ICON[kind] ?? Info;
  return (
    <details className={`note note-${kind} note-fold`}>
      <summary>
        <Icon className="icon" size={16} aria-hidden="true" />
        <span className="grow">{summary}</span>
        <ChevronDown className="note-fold-chev" size={16} aria-hidden="true" />
      </summary>
      <div className="note-fold-body">{children}</div>
    </details>
  );
}

export function ErrorNote({ error }) {
  const { t } = useTranslation();
  if (!error) return null;
  return <Note kind="error">{errorText(t, error)}</Note>;
}

export function Time({ value, relative = false, only }) {
  const { t } = useTranslation();
  if (!value) return <span>—</span>;
  if (only === 'relative') return <span title={formatTime(value)}>{relativeTime(t, value)}</span>;
  return (
    <span title={value}>
      {formatTime(value)}
      {relative && <span className="muted"> · {relativeTime(t, value)}</span>}
    </span>
  );
}

export function Skeleton({ height = 20, width = '100%', style }) {
  return <div className="skeleton" style={{ height, width, ...style }} aria-hidden="true" />;
}

export function Empty({ icon: Icon, title, children }) {
  return (
    <div className="empty">
      {Icon && (
        <div className="tile-icon">
          <Icon size={26} aria-hidden="true" />
        </div>
      )}
      {title && <h3>{title}</h3>}
      {children}
    </div>
  );
}

// ---------- 轻提示 ----------
const ToastCtx = createContext(() => {});

export function ToastProvider({ children }) {
  const [items, setItems] = useState([]);
  const push = useCallback((text, kind = 'ok') => {
    const id = Math.random();
    setItems((xs) => [...xs, { id, text, kind }]);
    setTimeout(() => setItems((xs) => xs.filter((x) => x.id !== id)), 2400);
  }, []);
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="toasts" aria-live="polite">
        {items.map((x) => (
          <div key={x.id} className={`toast ${x.kind === 'error' ? 'error' : ''}`}>
            {x.kind === 'error' ? <XCircle size={18} /> : <CheckCircle2 size={18} />}
            {x.text}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}

export function useToast() {
  return useContext(ToastCtx);
}

// ---------- 弹窗与确认框 ----------
// 弹窗可以叠放（例如新建终端里再打开文件夹浏览器）：Esc 只关最上面那个。
const modalStack = [];

export function Modal({ open, onClose, title, wide, className = '', children }) {
  const ref = useRef(null);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    if (!open) return undefined;
    const me = {};
    modalStack.push(me);
    const onKey = (e) => {
      if (e.key !== 'Escape' || modalStack[modalStack.length - 1] !== me) return;
      e.stopPropagation();
      close.current?.();
    };
    window.addEventListener('keydown', onKey);
    ref.current?.querySelector('input, button')?.focus();
    return () => {
      window.removeEventListener('keydown', onKey);
      modalStack.splice(modalStack.indexOf(me), 1);
    };
  }, [open]);
  if (!open) return null;
  // 挂到 body 上：叠放的弹窗不会被外层弹窗的滚动区域或动画（transform）困住。
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose?.()}>
      <div className={`modal ${wide ? 'modal-wide' : ''} ${className}`} role="dialog" aria-modal="true" aria-label={title} ref={ref}>
        {title && <h2>{title}</h2>}
        {children}
      </div>
    </div>,
    document.body,
  );
}

const ConfirmCtx = createContext(async () => false);

export function ConfirmProvider({ children }) {
  const { t } = useTranslation();
  const [state, setState] = useState(null);
  const confirm = useCallback(
    (opts) => new Promise((resolve) => setState({ ...opts, resolve })),
    [],
  );
  const close = (v) => {
    state?.resolve(v);
    setState(null);
  };
  return (
    <ConfirmCtx.Provider value={confirm}>
      {children}
      <Modal open={Boolean(state)} onClose={() => close(false)} title={state?.title}>
        {state?.body && <p className="muted">{state.body}</p>}
        <div className="modal-actions">
          <Button variant="ghost" onClick={() => close(false)}>
            {t('action.cancel')}
          </Button>
          <Button variant={state?.danger ? 'danger' : 'primary'} onClick={() => close(true)}>
            {state?.confirm ?? t('action.confirm')}
          </Button>
        </div>
      </Modal>
    </ConfirmCtx.Provider>
  );
}

/** `await confirm({title, body, confirm, danger})` → boolean */
export function useConfirm() {
  return useContext(ConfirmCtx);
}

export function CopyButton({ text, variant = 'ghost', label }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return undefined;
    const id = setTimeout(() => setDone(false), 1500);
    return () => clearTimeout(id);
  }, [done]);
  return (
    <Button
      variant={variant}
      icon={done ? Check : Copy}
      aria-label={label === false ? t('action.copy') : undefined}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setDone(true);
          toast(t('action.copied'));
        } catch {
          window.prompt('', text);
        }
      }}
    >
      {label === false ? null : (label ?? (done ? t('action.copied') : t('action.copy')))}
    </Button>
  );
}

/** 执行异步操作并管理 busy / error。 */
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  async function run(fn) {
    setBusy(true);
    setError(null);
    try {
      return await fn();
    } catch (e) {
      setError(e);
      return undefined;
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, setError, run };
}

/** 加载数据：返回 [data, error, reload, loading]。 */
export function useLoad(fn, deps = []) {
  const [state, setState] = useState({ data: null, error: null, loading: true });
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let alive = true;
    setState((s) => ({ ...s, loading: true }));
    fn()
      .then((data) => alive && setState({ data, error: null, loading: false }))
      .catch((error) => alive && setState({ data: null, error, loading: false }));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);
  return [state.data, state.error, () => setTick((n) => n + 1), state.loading];
}
