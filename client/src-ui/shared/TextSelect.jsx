// 选择文字：把终端里的文字（tmux 会话含最近约 3000 行回滚）铺成普通网页文字，盖在终端上。
// 终端是画出来的，手机上选不了字，电脑上开了鼠标的程序（tmux、AI 编程助手）里还得按住 Shift 拖。
// 这里用浏览器自己的选择：手机长按出选择把手和系统的“复制”菜单，电脑上直接拖选；另有“复制选中”“全部复制”。
// 打开方式：顶栏的按钮；手机上长按终端（term-inject.js 发来 looklook:select）。
import { ClipboardCopy, Copy, RefreshCw, X } from 'lucide-react';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from './api.js';
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

/** “选择文字”开关；手机上长按终端也会打开。 */
export function useTextSelect(frame, enabled) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!enabled) {
      setOpen(false);
      return undefined;
    }
    const on = (e) => {
      if (!frame.current || e.source !== frame.current.contentWindow || e.origin !== frameOrigin(frame)) return;
      if (e.data?.type === 'looklook:select') setOpen(true);
    };
    window.addEventListener('message', on);
    return () => window.removeEventListener('message', on);
  }, [enabled]);
  return [open, setOpen];
}

export default function TextSelect({ id, frame, onClose }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [text, setText] = useState(null);
  const [busy, setBusy] = useState(false);
  const scroller = useRef(null);
  const load = async () => {
    setBusy(true);
    try {
      const r = await api.get(`/instances/${id}/text`).catch(() => null);
      setText(typeof r?.text === 'string' ? r.text : await frameText(frame));
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    load();
  }, [id]);
  // 打开时停在最下面（最新的输出），和终端里看到的位置一致
  useLayoutEffect(() => {
    if (scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
  }, [text]);
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
        {text === null ? <p className="text-select-empty">{t('select.loading')}</p> : text ? <pre>{text}</pre> : <p className="text-select-empty">{t('select.empty')}</p>}
      </div>
    </div>
  );
}
