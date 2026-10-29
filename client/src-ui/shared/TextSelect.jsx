// 选择文字：把终端里的文字（tmux 会话含最近约 3000 行回滚）铺成普通网页文字，盖在终端上。
// 终端是画出来的，手机上选不了字，电脑上开了鼠标的程序（tmux、AI 编程助手）里还得按住 Shift 拖。
// 这里用浏览器自己的选择：手机长按出选择把手和系统的“复制”菜单，电脑上直接拖选；另有“复制选中”“全部复制”。
// 打开方式：顶栏的按钮；手机上长按终端（term-inject.js 发来 looklook:select，带上按住的那一行，打开时滚到那里并短暂高亮）。
import { ClipboardCopy, Copy, RefreshCw, X } from 'lucide-react';
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from './api.js';
import { locateLine } from './selectAt.js';
import { frameOrigin } from './termFrame.js';
import { useToast } from './ui.jsx';

/** 没有 tmux 会话时读终端自己的缓冲区：同源直接调用，本机直连（跨源）时发消息要。 */
function frameText(frame) {
  const w = frame.current?.contentWindow;
  if (!w) return Promise.resolve('');
  try {
    if (typeof w.looklookText === 'function') return Promise.resolve(w.looklookText());
  } catch {
    /* 跨源 */
  }
  return new Promise((resolve) => {
    const timer = setTimeout(done, 1500, '');
    function done(text) {
      clearTimeout(timer);
      window.removeEventListener('message', on);
      resolve(text);
    }
    function on(e) {
      if (e.source === w && e.data?.type === 'looklook:text') done(String(e.data.text ?? ''));
    }
    window.addEventListener('message', on);
    w.postMessage({ type: 'looklook:text' }, frameOrigin(frame));
  });
}

async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.cssText = 'position:fixed;top:-1000px;opacity:0';
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

/** “选择文字”开关；手机上长按终端也会打开。返回 [开着吗, 开关, 长按的位置（按钮打开时为 null）]。 */
export function useTextSelect(frame, enabled) {
  const [state, setState] = useState(null); // null：关着；{ at }：开着
  const setOpen = useCallback((open) => setState(open ? { at: null } : null), []);
  useEffect(() => {
    if (!enabled) {
      setState(null);
      return undefined;
    }
    const on = (e) => {
      if (!frame.current || e.source !== frame.current.contentWindow || e.origin !== frameOrigin(frame)) return;
      if (e.data?.type === 'looklook:select') setState({ at: e.data.at ?? null });
    };
    window.addEventListener('message', on);
    return () => window.removeEventListener('message', on);
  }, [enabled]);
  return [state !== null, setOpen, state?.at ?? null];
}

export default function TextSelect({ id, frame, at, onClose }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [text, setText] = useState(null);
  const [busy, setBusy] = useState(false);
  const scroller = useRef(null);
  const mark = useRef(null);
  // 长按的那一行（找不到时 -1，停在最下面）。只在第一次读到文字时定位，“重新读取”后回到最下面。
  const [pin, setPin] = useState(-1);
  const located = useRef(false);
  const load = async () => {
    setBusy(true);
    try {
      const r = await api.get(`/instances/${id}/text`).catch(() => null);
      const value = typeof r?.text === 'string' ? r.text : await frameText(frame);
      setPin(located.current ? -1 : locateLine(value, at));
      located.current = true;
      setText(value);
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    load();
  }, [id]);
  // 长按打开的停在按住的那一行（放在中间）；否则停在最下面（最新的输出），和终端里看到的位置一致
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (mark.current) el.scrollTop = mark.current.offsetTop - el.clientHeight / 2 + mark.current.offsetHeight / 2;
    else el.scrollTop = el.scrollHeight;
  }, [text, pin]);
  useEffect(() => {
    const onKey = (e) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const copy = async (value) => {
    const ok = await writeClipboard(value);
    toast(t(ok ? 'select.copied' : 'select.copy_failed'), ok ? undefined : 'error');
  };
  const copySelection = () => {
    const sel = window.getSelection?.();
    const value = sel && scroller.current?.contains(sel.anchorNode) ? sel.toString() : '';
    if (!value) toast(t('select.nothing'));
    else copy(value);
  };
  const iconBtn = 'btn btn-plain btn-icon';
  return (
    <div className="text-select" role="dialog" aria-label={t('select.title')}>
      <div className="text-select-bar">
        <span className="text-select-title">{t('select.title')}</span>
        <span className="text-select-hint">{t('select.hint')}</span>
        <button type="button" className="btn btn-plain text-select-action" onPointerDown={(e) => e.preventDefault()} onClick={copySelection} title={t('select.copy_selection')}>
          <Copy size={16} aria-hidden="true" />
          <span>{t('select.copy_selection')}</span>
        </button>
        <button type="button" className="btn btn-plain text-select-action" onClick={() => copy(text ?? '')} disabled={!text} title={t('select.copy_all')}>
          <ClipboardCopy size={16} aria-hidden="true" />
          <span>{t('select.copy_all')}</span>
        </button>
        <button type="button" className={iconBtn} onClick={load} disabled={busy} aria-label={t('select.reload')} title={t('select.reload')}>
          <RefreshCw size={16} />
        </button>
        <button type="button" className={iconBtn} onClick={onClose} aria-label={t('select.close')} title={t('select.close')}>
          <X size={18} />
        </button>
      </div>
      <div className="text-select-scroll" ref={scroller}>
        {text === null ? <p className="text-select-empty">{t('select.loading')}</p> : text ? <Lines text={text} pin={pin} mark={mark} /> : <p className="text-select-empty">{t('select.empty')}</p>}
      </div>
    </div>
  );
}

/**
 * 面板里的文字；pin 那一行包一层 <mark>（撑满一行宽，短暂高亮）。文字和换行原样不动：
 * 一行一块的话，复制时浏览器会把空行丢掉。
 */
function Lines({ text, pin, mark }) {
  if (pin < 0) return <pre>{text}</pre>;
  const lines = text.split('\n');
  return (
    <pre>
      {lines.slice(0, pin).map((l) => l + '\n').join('')}
      <mark ref={mark} className="text-select-at">
        {lines[pin]}
      </mark>
      {lines.slice(pin + 1).map((l) => '\n' + l).join('')}
    </pre>
  );
}
