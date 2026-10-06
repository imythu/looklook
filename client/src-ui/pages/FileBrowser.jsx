// 终端页的“文件”标签：从终端的当前目录开始浏览这台电脑上的文件，预览文本和图片、下载、把路径填进终端。
// 接口挂在 /api/instances/{id}/… 下面，多台电脑时按终端路由到它所在的电脑（服务端 edge/route.rs）。
import { ArrowUp, ChevronRight, Copy, Download, Eye, EyeOff, File, FileText, Folder, Home, Image as ImageIcon, Link2, LocateFixed, RefreshCw, TextCursorInput, WrapText, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { relativeTime } from '../shared/time.js';
import { useToast } from '../shared/ui.jsx';
import { formatBytes, quotePath } from '../shared/xfer.js';

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|ico|avif)$/i;
const PREVIEW_MAX = 1024 * 1024; // 与 transfer::PREVIEW_MAX 一致

const fileUrl = (id, kind, path) => `/api/instances/${id}/${kind}?path=${encodeURIComponent(path)}`;

/** 面包屑：`/a/b` → [/, /a, /a/b]；`C:\a\b` → [C:\, C:\a, C:\a\b]。 */
export function crumbs(path, sep) {
  if (!path) return [];
  const win = sep === '\\';
  const parts = path.split(win ? /[\\/]/ : '/').filter(Boolean);
  const out = [];
  let acc = win ? '' : '/';
  if (!win) out.push({ name: '/', path: '/' });
  parts.forEach((p, i) => {
    acc = win ? (i === 0 ? `${p}\\` : `${acc}${acc.endsWith('\\') ? '' : '\\'}${p}`) : `${acc}${acc.endsWith('/') ? '' : '/'}${p}`;
    out.push({ name: p, path: acc });
  });
  return out;
}

const join = (dir, name, sep) => (dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`);

/** 文本预览：按 UTF-8 解码，含 NUL 或解不出来就当二进制。 */
function decodeText(buf) {
  const bytes = new Uint8Array(buf);
  if (bytes.subarray(0, 8192).includes(0)) return null;
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    // 截断可能切在多字节字符中间：去掉末尾几个字节再试
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, Math.max(0, bytes.length - 3)));
    } catch {
      return null;
    }
  }
}

function Preview({ id, entry, onClose, onInsert }) {
  const { t } = useTranslation();
  const [state, setState] = useState({ loading: true });
  const [wrap, setWrap] = useState(true);
  const image = IMAGE.test(entry.name);
  useEffect(() => {
    if (image) return undefined;
    let alive = true;
    setState({ loading: true });
    fetch(fileUrl(id, 'preview', entry.path), { credentials: 'same-origin' })
      .then(async (res) => {
        if (!res.ok) {
          const e = (await res.json().catch(() => null))?.error;
          throw Object.assign(new Error(e?.code ?? 'INTERNAL'), { code: e?.code ?? 'INTERNAL', params: e?.params ?? {} });
        }
        const size = Number(res.headers.get('x-looklook-size') ?? entry.size ?? 0);
        const text = decodeText(await res.arrayBuffer());
        if (alive) setState({ text, truncated: size > PREVIEW_MAX });
      })
      .catch((error) => alive && setState({ error }));
    return () => {
      alive = false;
    };
  }, [id, entry.path]);
  return (
    <div className="fb-preview" role="region" aria-label={entry.name}>
      <div className="fb-preview-head">
        {image ? <ImageIcon size={16} aria-hidden="true" /> : <FileText size={16} aria-hidden="true" />}
        <b className="ellipsis grow" title={entry.path}>{entry.name}</b>
        {entry.size != null && <span className="muted small">{formatBytes(entry.size)}</span>}
        {!image && state.text != null && (
          <button type="button" className={`btn btn-plain btn-icon ${wrap ? 'on' : ''}`} onClick={() => setWrap(!wrap)} aria-pressed={wrap} aria-label={t('fb.wrap')} title={t('fb.wrap')}>
            <WrapText size={16} />
          </button>
        )}
        <button type="button" className="btn btn-plain btn-icon" onClick={() => onInsert(entry.path)} aria-label={t('fb.insert')} title={t('fb.insert')}>
          <TextCursorInput size={16} />
        </button>
        <a className="btn btn-plain btn-icon" href={fileUrl(id, 'download', entry.path)} download={entry.name} aria-label={t('files.download')} title={t('files.download')}>
          <Download size={16} />
        </a>
        <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('fb.close_preview')} title={t('fb.close_preview')}>
          <X size={17} />
        </button>
      </div>
      <div className="fb-preview-body">
        {image ? (
          <img src={fileUrl(id, 'preview', entry.path)} alt={entry.name} />
        ) : state.loading ? (
          <p className="muted small fb-pad">{t('fb.loading')}</p>
        ) : state.error ? (
          <p className="fb-pad small" style={{ color: 'var(--danger)' }}>{t(`files.err.${state.error.code}`, { defaultValue: errorText(t, state.error) })}</p>
        ) : state.text == null ? (
          <p className="muted small fb-pad">{t('fb.binary')}</p>
        ) : (
          <>
            {state.truncated && <p className="muted small fb-pad fb-note">{t('fb.truncated', { size: formatBytes(PREVIEW_MAX) })}</p>}
            <pre className={`fb-text ${wrap ? 'fb-wrap' : ''}`}>{state.text || t('fb.empty_file')}</pre>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * `id` 终端；`active` 标签是否可见（不可见时不轮询）；`onInsert(text)` 把路径填进终端。
 */
export default function FileBrowser({ id, active, onInsert }) {
  const { t } = useTranslation();
  const [path, setPath] = useState(''); // 空 = 终端的当前目录
  const [hidden, setHidden] = useState(false);
  const [tick, setTick] = useState(0);
  const [state, setState] = useState({ loading: true });
  const [open, setOpen] = useState(null);
  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    setState((s) => ({ ...s, loading: true }));
    api
      .get(`/instances/${id}/fs?path=${encodeURIComponent(path)}&hidden=${hidden ? 1 : 0}`)
      .then((data) => alive && setState({ data }))
      .catch((error) => alive && setState((s) => ({ data: s.data, error })));
    return () => {
      alive = false;
    };
  }, [id, path, hidden, tick, active]);
  const data = state.data;
  const sep = data?.os === 'windows' ? '\\' : '/';
  // 路径很长（手机上）时把面包屑滚到最右，露出当前文件夹
  const crumbsRef = useRef(null);
  useEffect(() => {
    const el = crumbsRef.current;
    if (el) el.scrollLeft = el.scrollWidth;
  }, [data?.path]);
  const go = (p) => {
    setOpen(null);
    setPath(p);
  };
  const insert = (p) => onInsert(`${quotePath(p, data?.os)} `);
  const toast = useToast();
  const copy = () =>
    navigator.clipboard?.writeText(data.path).then(
      () => toast(t('fb.copied')),
      () => toast(t('fb.copy_failed'), 'error'),
    );
  const now = Date.now();
  return (
    <div className={`fb ${open ? 'fb-has-preview' : ''}`}>
      <div className="fb-bar">
        <button type="button" className="btn btn-plain btn-icon" disabled={!data?.parent} onClick={() => go(data.parent)} aria-label={t('fb.up')} title={t('fb.up')}>
          <ArrowUp size={17} />
        </button>
        <nav className="fb-crumbs" ref={crumbsRef} aria-label={t('fb.location')}>
          {crumbs(data?.path, sep).map((c, i, all) => (
            <span key={c.path} className="fb-crumb">
              {i > 0 && c.name !== '/' && all[i - 1].name !== '/' && <ChevronRight size={13} aria-hidden="true" className="muted" />}
              <button type="button" className="fb-crumb-btn" onClick={() => go(c.path)} aria-current={i === all.length - 1 ? 'location' : undefined}>
                {c.name}
              </button>
            </span>
          ))}
        </nav>
        {data?.path && (
          <button type="button" className="btn btn-plain btn-icon" onClick={copy} aria-label={t('fb.copy_path')} title={t('fb.copy_path')}>
            <Copy size={16} />
          </button>
        )}
        <button type="button" className="btn btn-plain btn-icon" onClick={() => go('')} aria-label={t('fb.cwd')} title={t('fb.cwd')}>
          <LocateFixed size={17} />
        </button>
        {data?.home && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => go(data.home)} aria-label={t('fb.home')} title={t('fb.home')}>
            <Home size={17} />
          </button>
        )}
        <button type="button" className={`btn btn-plain btn-icon ${hidden ? 'on' : ''}`} onClick={() => setHidden(!hidden)} aria-pressed={hidden} aria-label={t('fb.hidden')} title={t('fb.hidden')}>
          {hidden ? <Eye size={17} /> : <EyeOff size={17} />}
        </button>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => setTick((n) => n + 1)} aria-label={t('term.reload')} title={t('term.reload')}>
          <RefreshCw size={17} className={state.loading ? 'spin' : ''} />
        </button>
      </div>
      <div className="fb-main">
        <div className="fb-list" role="list" aria-busy={state.loading}>
          {state.error && <p className="fb-pad small" style={{ color: 'var(--danger)' }}>{errorText(t, state.error)}</p>}
          {data?.denied && <p className="fb-pad small muted">{t('fb.denied')}</p>}
          {data && !data.exists && <p className="fb-pad small muted">{t('fb.moved')}</p>}
          {data && !data.denied && data.entries.length === 0 && <p className="fb-pad small muted">{t('fb.empty')}</p>}
          {data?.entries.map((e) => {
            const full = join(data.path, e.name, sep);
            const Icon = e.is_dir ? Folder : IMAGE.test(e.name) ? ImageIcon : File;
            const selected = open?.path === full;
            return (
              <div key={e.name} role="listitem" className={`fb-row ${selected ? 'selected' : ''}`}>
                <button type="button" className="fb-name" onClick={() => (e.is_dir ? go(full) : setOpen({ ...e, path: full }))} title={full}>
                  <Icon size={16} aria-hidden="true" className={e.is_dir ? 'fb-dir' : 'muted'} />
                  <span className="ellipsis">{e.name}</span>
                  {e.link && <Link2 size={12} aria-label={t('fb.link')} className="muted" />}
                </button>
                <span className="fb-meta muted small">{e.is_dir ? '' : formatBytes(e.size ?? NaN)}</span>
                <span className="fb-meta fb-time muted small">{e.mtime_ms ? relativeTime(t, new Date(e.mtime_ms).toISOString(), now) : ''}</span>
                <span className="fb-acts">
                  <button type="button" className="btn btn-plain btn-icon" onClick={() => insert(full)} aria-label={t('fb.insert')} title={t('fb.insert')}>
                    <TextCursorInput size={15} />
                  </button>
                  {!e.is_dir && (
                    <a className="btn btn-plain btn-icon" href={fileUrl(id, 'download', full)} download={e.name} aria-label={t('files.download')} title={t('files.download')}>
                      <Download size={15} />
                    </a>
                  )}
                </span>
              </div>
            );
          })}
        </div>
        {open && <Preview id={id} entry={open} onClose={() => setOpen(null)} onInsert={insert} />}
      </div>
    </div>
  );
}
