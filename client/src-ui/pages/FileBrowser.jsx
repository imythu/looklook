// 文件浏览：终端页的“文件”标签（传 id：从终端的当前目录开始）和管理台的“文件”页（不传 id：从主目录开始）。
// 浏览这台电脑上的文件，预览文本、图片、Markdown 和办公文件（PDF、Word、Excel、CSV，OfficePreview.jsx），下载、把路径填进终端；新建文件夹、重命名、删除、上传（拖进列表也行）。
// 终端标签的浏览、预览、下载挂在 /api/instances/{id}/… 下面，多台电脑时按终端路由到它所在的电脑（服务端 edge/route.rs）；
// “文件”页与文件管理用 /api/fs/…，多台电脑时发给选中的那台（X-LL-Device）。
import { ArrowUp, BookOpen, ChevronRight, Code2, Copy, Download, Eye, EyeOff, File, FileSpreadsheet, FileText, Folder, FolderPlus, Home, Image as ImageIcon, Link2, LocateFixed, PenLine, RefreshCw, TextCursorInput, Trash2, Upload, WrapText, X } from 'lucide-react';
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi, fetchRaw, plainLinksWork, selectedDevice, terminalDevice } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import Markdown, { isMarkdown } from '../shared/Markdown.jsx';
import { OFFICE_MAX, officeKind, parseCsv, WHOLE } from '../shared/office.js';
import { relativeTime } from '../shared/time.js';
import { Button, ErrorNote, Field, Modal, useConfirm, useToast } from '../shared/ui.jsx';
import { dirUploadStore } from '../shared/uploader.js';
import { formatBytes, quotePath } from '../shared/xfer.js';
import { renderOffice, SheetView } from './OfficePreview.jsx';

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|ico|avif)$/i;
const PREVIEW_MAX = 1024 * 1024; // 与 transfer::PREVIEW_MAX 一致
const ACTIVE = ['queued', 'uploading', 'retrying'];

/** 终端标签（id）或“文件”页（无 id）用的接口地址。 */
function endpoints(id) {
  const q = (path) => `?path=${encodeURIComponent(path)}`;
  return id
    ? { list: (path, hidden) => `/instances/${id}/fs${q(path)}&hidden=${hidden ? 1 : 0}`, preview: (p) => `/instances/${id}/preview${q(p)}`, download: (p) => `/instances/${id}/download${q(p)}`, plain: true }
    : { list: (path, hidden) => `/fs/list${q(path)}&files=1&hidden=${hidden ? 1 : 0}`, preview: (p) => `/fs/preview${q(p)}`, download: (p) => `/fs/download${q(p)}`, plain: plainLinksWork() };
}

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

/** 下载：能用普通链接时直接打开；远程多台电脑时先取回再保存（普通链接带不了 X-LL-Device）。 */
function DownloadLink({ ep, entry, size = 15 }) {
  const { t } = useTranslation();
  const toast = useToast();
  const label = t('files.download');
  if (ep.plain) {
    return (
      <a className="btn btn-plain btn-icon" href={`/api${ep.download(entry.path)}`} download={entry.name} aria-label={label} title={label}>
        <Download size={size} />
      </a>
    );
  }
  const save = async () => {
    try {
      const blob = await (await fetchRaw(ep.download(entry.path))).blob();
      const url = URL.createObjectURL(blob);
      const a = Object.assign(document.createElement('a'), { href: url, download: entry.name });
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 30000);
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  return (
    <button type="button" className="btn btn-plain btn-icon" onClick={save} aria-label={label} title={label}>
      <Download size={size} />
    </button>
  );
}

function Preview({ ep, entry, onClose, onInsert, sep }) {
  const { t } = useTranslation();
  const [state, setState] = useState({ loading: true });
  const [wrap, setWrap] = useState(true);
  const md = isMarkdown(entry.name);
  const office = officeKind(entry.name);
  const csv = office === 'csv';
  // Markdown 看排版效果 / 源码，CSV 看表格 / 源码
  const [rendered, setRendered] = useState(true);
  const image = IMAGE.test(entry.name);
  const whole = WHOLE.includes(office);
  useEffect(() => {
    let alive = true;
    let url = null;
    // 浏览器不能在页面里显示 PDF（安卓上的 Chrome 等）时不取回来，提示下载
    const noPdf = office === 'pdf' && navigator.pdfViewerEnabled === false;
    if (office === 'other' || noPdf || (whole && entry.size > OFFICE_MAX)) {
      setState({ office: office === 'other' ? 'other' : noPdf ? 'no_pdf' : 'too_big' });
      return undefined;
    }
    setState({ loading: true });
    // 图片、PDF、Word、Excel 取整个文件（预览接口只给前 1 MiB）
    fetchRaw(image || whole ? ep.download(entry.path) : ep.preview(entry.path))
      .then(async (res) => {
        if (image) {
          url = URL.createObjectURL(await res.blob());
          if (alive) setState({ url });
          return;
        }
        if (whole) {
          const view = await renderOffice(office, await res.blob()).catch(() => null);
          url = view?.url ?? null;
          if (alive) setState(view ? { view } : { office: 'failed' });
          return;
        }
        const size = Number(res.headers.get('x-looklook-size') ?? entry.size ?? 0);
        const text = decodeText(await res.arrayBuffer());
        if (alive) setState({ text, truncated: size > PREVIEW_MAX });
      })
      .catch((error) => alive && setState({ error }));
    return () => {
      alive = false;
      if (url) URL.revokeObjectURL(url);
    };
  }, [entry.path]);
  const dir = entry.path.slice(0, Math.max(0, entry.path.lastIndexOf(sep))) || sep;
  const showMd = md && rendered && state.text != null;
  const showSheet = csv && rendered && state.text != null;
  const csvSheets = useMemo(() => (showSheet ? parseCsv(state.text, entry.name) : null), [showSheet, state.text, entry.name]);
  return (
    <div className="fb-preview" role="region" aria-label={entry.name}>
      <div className="fb-preview-head">
        {image ? <ImageIcon size={16} aria-hidden="true" /> : office === 'xlsx' || csv ? <FileSpreadsheet size={16} aria-hidden="true" /> : <FileText size={16} aria-hidden="true" />}
        <b className="ellipsis grow" title={entry.path}>{entry.name}</b>
        {entry.size != null && <span className="muted small">{formatBytes(entry.size)}</span>}
        {(md || csv) && state.text != null && (
          <button
            type="button"
            className="btn btn-plain btn-icon"
            onClick={() => setRendered(!rendered)}
            aria-pressed={!rendered}
            aria-label={t(rendered ? 'fb.md_source' : csv ? 'fb.csv_table' : 'fb.md_rendered')}
            title={t(rendered ? 'fb.md_source' : csv ? 'fb.csv_table' : 'fb.md_rendered')}
          >
            {rendered ? <Code2 size={16} /> : csv ? <FileSpreadsheet size={16} /> : <BookOpen size={16} />}
          </button>
        )}
        {!image && !showMd && !showSheet && state.text != null && (
          <button type="button" className={`btn btn-plain btn-icon ${wrap ? 'on' : ''}`} onClick={() => setWrap(!wrap)} aria-pressed={wrap} aria-label={t('fb.wrap')} title={t('fb.wrap')}>
            <WrapText size={16} />
          </button>
        )}
        {onInsert && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => onInsert(entry.path)} aria-label={t('fb.insert')} title={t('fb.insert')}>
            <TextCursorInput size={16} />
          </button>
        )}
        <DownloadLink ep={ep} entry={entry} size={16} />
        <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('fb.close_preview')} title={t('fb.close_preview')}>
          <X size={17} />
        </button>
      </div>
      <div className="fb-preview-body">
        {state.loading ? (
          <p className="muted small fb-pad">{t('fb.loading')}</p>
        ) : state.error ? (
          <p className="fb-pad small" style={{ color: 'var(--danger)' }}>{t(`files.err.${state.error.code}`, { defaultValue: errorText(t, state.error) })}</p>
        ) : image ? (
          <img src={state.url} alt={entry.name} />
        ) : state.office ? (
          <p className="muted small fb-pad">{t(`fb.office_${state.office}`, { size: formatBytes(OFFICE_MAX) })}</p>
        ) : state.view?.url ? (
          <iframe className="fb-pdf" src={state.view.url} title={entry.name} />
        ) : state.view?.html === '' ? (
          <p className="muted small fb-pad">{t('fb.empty_file')}</p>
        ) : state.view?.html != null ? (
          // eslint-disable-next-line react/no-danger -- 已用 DOMPurify 清洗（OfficePreview.jsx）
          <div className="md" dangerouslySetInnerHTML={{ __html: state.view.html }} />
        ) : state.view?.sheets ? (
          <SheetView sheets={state.view.sheets} />
        ) : state.text == null ? (
          <p className="muted small fb-pad">{t('fb.binary')}</p>
        ) : (
          <>
            {state.truncated && <p className="muted small fb-pad fb-note">{t('fb.truncated', { size: formatBytes(PREVIEW_MAX) })}</p>}
            {showSheet ? (
              <SheetView sheets={csvSheets} />
            ) : showMd ? (
              <Markdown text={state.text} imageUrl={ep.plain ? (src) => `/api${ep.preview(resolve(dir, src, sep))}` : null} />
            ) : (
              <pre className={`fb-text ${wrap ? 'fb-wrap' : ''}`}>{state.text || t('fb.empty_file')}</pre>
            )}
          </>
        )}
      </div>
    </div>
  );
}

/** Markdown 里的相对图片路径 → 这台电脑上的绝对路径。 */
function resolve(dir, rel, sep) {
  const parts = dir.split(/[\\/]/);
  for (const seg of rel.split(/[\\/]/)) {
    if (seg === '..') parts.length > 1 && parts.pop();
    else if (seg && seg !== '.') parts.push(seg);
  }
  const out = parts.join(sep);
  return out || sep;
}

/** 新建文件夹 / 重命名：一个输入框。 */
function NameDialog({ dialog, onClose }) {
  const { t } = useTranslation();
  const [name, setName] = useState(dialog?.initial ?? '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const input = useRef(null);
  useEffect(() => {
    setName(dialog?.initial ?? '');
    setError(null);
    // 重命名时只选中文件名、不选扩展名
    setTimeout(() => {
      const el = input.current;
      if (!el) return;
      el.focus();
      const dot = dialog?.isDir ? -1 : (dialog?.initial ?? '').lastIndexOf('.');
      el.setSelectionRange(0, dot > 0 ? dot : el.value.length);
    }, 0);
  }, [dialog]);
  if (!dialog) return null;
  const submit = async (e) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await dialog.run(name.trim());
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal open onClose={onClose} title={dialog.title}>
      <form onSubmit={submit}>
        <Field label={t('fb.name')}>
          <input ref={input} value={name} onChange={(e) => setName(e.target.value)} maxLength={255} autoComplete="off" spellCheck={false} />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button type="button" variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!name.trim() || name.trim() === dialog.initial}>
            {dialog.confirm}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/**
 * `id` 终端（不传 = 管理台“文件”页，浏览选中的那台电脑）；`active` 是否可见（不可见时不刷新）；
 * `onInsert(text)` 把路径填进终端（没有时不显示这个按钮）。
 */
export default function FileBrowser({ id, active = true, onInsert }) {
  const { t } = useTranslation();
  const toast = useToast();
  const confirm = useConfirm();
  const [path, setPath] = useState(''); // 空 = 终端的当前目录（“文件”页：主目录）
  const [hidden, setHidden] = useState(false);
  const [tick, setTick] = useState(0);
  const [state, setState] = useState({ loading: true });
  const [open, setOpen] = useState(null);
  const [dialog, setDialog] = useState(null);
  const [dropping, setDropping] = useState(false);
  const fileInput = useRef(null);
  const ep = endpoints(id);
  const device = id ? terminalDevice(id) : selectedDevice();
  const ops = deviceApi(device ?? undefined);
  const store = dirUploadStore(device);
  const tasks = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const reload = () => setTick((n) => n + 1);
  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    setState((s) => ({ ...s, loading: true }));
    ops
      .get(ep.list(path, hidden))
      .then((data) => alive && setState({ data }))
      .catch((error) => alive && setState((s) => ({ data: s.data, error })));
    return () => {
      alive = false;
    };
  }, [id, device, path, hidden, tick, active]);
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
  const insert = onInsert && ((p) => onInsert(`${quotePath(p, data?.os)} `));
  const copy = () =>
    navigator.clipboard?.writeText(data.path).then(
      () => toast(t('fb.copied')),
      () => toast(t('fb.copy_failed'), 'error'),
    );
  const failed = (e) => toast(errorText(t, e), 'error');

  // ---------- 文件管理 ----------
  const mkdir = () =>
    setDialog({
      title: t('fb.new_folder'),
      confirm: t('fb.create'),
      initial: '',
      run: async (name) => {
        await ops.post('/fs/mkdir', { path: data.path, name });
        reload();
      },
    });
  const rename = (entry) =>
    setDialog({
      title: t('fb.rename_title', { name: entry.name }),
      confirm: t('fb.rename'),
      initial: entry.name,
      isDir: entry.is_dir,
      run: async (name) => {
        const r = await ops.post('/fs/rename', { path: entry.path, name });
        if (open?.path === entry.path) setOpen(null);
        reload();
        return r;
      },
    });
  const remove = async (entry) => {
    const ok = await confirm({
      title: t(entry.is_dir ? 'fb.delete_dir_title' : 'fb.delete_title', { name: entry.name }),
      body: t(entry.is_dir ? 'fb.delete_dir_body' : 'fb.delete_body', { path: entry.path }),
      confirm: t('fb.delete'),
      danger: true,
    });
    if (!ok) return;
    try {
      await ops.post('/fs/delete', { path: entry.path });
      if (open?.path === entry.path) setOpen(null);
      toast(t('fb.deleted', { name: entry.name }));
    } catch (e) {
      failed(e);
    }
    reload();
  };
  const upload = (list) => {
    const files = Array.from(list ?? []);
    if (!files.length || !data?.path) return;
    const dir = data.path;
    store.add(files, {
      dest: dir,
      onDone: (fresh, all) => {
        const bad = all.filter((x) => x.state === 'error');
        if (bad.length) toast(t('files.failed', { name: bad[0].name, reason: t(`files.err.${bad[0].error?.code}`, { defaultValue: errorText(t, bad[0].error) }) }), 'error');
        if (fresh.length) toast(t('fb.uploaded', { n: fresh.length }));
        reload();
      },
    });
  };
  const busy = tasks.filter((x) => ACTIVE.includes(x.state));
  const pct = busy.length ? Math.floor((busy.reduce((n, x) => n + Math.min(x.sent, x.size), 0) / Math.max(1, busy.reduce((n, x) => n + x.size, 0))) * 100) : 0;
  const hasFiles = (e) => Array.from(e.dataTransfer?.types ?? []).includes('Files');
  const canWrite = Boolean(data?.path && data.exists !== false && !data.denied);

  const now = Date.now();
  return (
    <div
      className={`fb ${open ? 'fb-has-preview' : ''} ${dropping ? 'fb-dropping' : ''}`}
      onDragOver={(e) => {
        if (!canWrite || !hasFiles(e)) return;
        e.preventDefault();
        setDropping(true);
      }}
      onDragLeave={(e) => !e.currentTarget.contains(e.relatedTarget) && setDropping(false)}
      onDrop={(e) => {
        if (!canWrite || !hasFiles(e)) return;
        e.preventDefault();
        setDropping(false);
        upload(e.dataTransfer.files);
      }}
    >
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
        {id && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => go('')} aria-label={t('fb.cwd')} title={t('fb.cwd')}>
            <LocateFixed size={17} />
          </button>
        )}
        {data?.home && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => go(data.home)} aria-label={t('fb.home')} title={t('fb.home')}>
            <Home size={17} />
          </button>
        )}
        <button type="button" className={`btn btn-plain btn-icon ${hidden ? 'on' : ''}`} onClick={() => setHidden(!hidden)} aria-pressed={hidden} aria-label={t('fb.hidden')} title={t('fb.hidden')}>
          {hidden ? <Eye size={17} /> : <EyeOff size={17} />}
        </button>
        <button type="button" className="btn btn-plain btn-icon" onClick={reload} aria-label={t('term.reload')} title={t('term.reload')}>
          <RefreshCw size={17} className={state.loading ? 'spin' : ''} />
        </button>
        <span className="fb-bar-sep" aria-hidden="true" />
        <button type="button" className="btn btn-plain btn-icon" disabled={!canWrite} onClick={mkdir} aria-label={t('fb.new_folder')} title={t('fb.new_folder')}>
          <FolderPlus size={17} />
        </button>
        <button type="button" className="btn btn-plain btn-icon" disabled={!canWrite} onClick={() => fileInput.current?.click()} aria-label={t('fb.upload')} title={t('fb.upload_hint')}>
          <Upload size={17} />
        </button>
        <input ref={fileInput} type="file" multiple hidden onChange={(e) => { upload(e.target.files); e.target.value = ''; }} />
      </div>
      {busy.length > 0 && (
        <div className="fb-progress" role="status">
          <span className="small">{t('fb.uploading', { n: busy.length, pct })}</span>
          <div className="files-bar grow" aria-hidden="true">
            <i style={{ width: `${pct}%` }} />
          </div>
          <button type="button" className="btn btn-plain btn-icon" onClick={() => store.cancelWhere((x) => ACTIVE.includes(x.state))} aria-label={t('files.cancel')} title={t('files.cancel')}>
            <X size={15} />
          </button>
        </div>
      )}
      <div className="fb-main">
        <div className="fb-list" role="list" aria-busy={state.loading}>
          {state.error && <p className="fb-pad small" style={{ color: 'var(--danger)' }}>{errorText(t, state.error)}</p>}
          {data?.denied && <p className="fb-pad small muted">{t('fb.denied')}</p>}
          {data && !data.exists && <p className="fb-pad small muted">{t('fb.moved')}</p>}
          {data && !data.denied && data.entries.length === 0 && <p className="fb-pad small muted">{t('fb.empty')}</p>}
          {data?.entries.map((e) => {
            const full = join(data.path, e.name, sep);
            const entry = { ...e, path: full };
            const Icon = e.is_dir ? Folder : IMAGE.test(e.name) ? ImageIcon : isMarkdown(e.name) ? BookOpen : File;
            const selected = open?.path === full;
            return (
              <div key={e.name} role="listitem" className={`fb-row ${selected ? 'selected' : ''}`}>
                <button type="button" className="fb-name" onClick={() => (e.is_dir ? go(full) : setOpen(entry))} title={full}>
                  <Icon size={16} aria-hidden="true" className={e.is_dir ? 'fb-dir' : 'muted'} />
                  <span className="ellipsis">{e.name}</span>
                  {e.link && <Link2 size={12} aria-label={t('fb.link')} className="muted" />}
                </button>
                <span className="fb-meta muted small">{e.is_dir ? '' : formatBytes(e.size ?? NaN)}</span>
                <span className="fb-meta fb-time muted small">{e.mtime_ms ? relativeTime(t, new Date(e.mtime_ms).toISOString(), now) : ''}</span>
                <span className="fb-acts">
                  {insert && (
                    <button type="button" className="btn btn-plain btn-icon" onClick={() => insert(full)} aria-label={t('fb.insert')} title={t('fb.insert')}>
                      <TextCursorInput size={15} />
                    </button>
                  )}
                  {!e.is_dir && <DownloadLink ep={ep} entry={entry} />}
                  <button type="button" className="btn btn-plain btn-icon fb-more" onClick={() => rename(entry)} aria-label={`${t('fb.rename')} ${e.name}`} title={t('fb.rename')}>
                    <PenLine size={15} />
                  </button>
                  <button type="button" className="btn btn-plain btn-icon fb-more fb-danger" onClick={() => remove(entry)} aria-label={`${t('fb.delete')} ${e.name}`} title={t('fb.delete')}>
                    <Trash2 size={15} />
                  </button>
                </span>
              </div>
            );
          })}
        </div>
        {open && <Preview ep={ep} entry={open} sep={sep} onClose={() => setOpen(null)} onInsert={insert} />}
      </div>
      {dropping && (
        <div className="fb-drop" aria-hidden="true">
          <Upload size={26} />
          <p>{t('fb.drop', { dir: data?.path ?? '' })}</p>
        </div>
      )}
      <NameDialog dialog={dialog} onClose={() => setDialog(null)} />
    </div>
  );
}
