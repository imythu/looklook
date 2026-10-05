// 终端的“文件”面板（docs/FILE_TRANSFER.md §3）：标题栏回形针按钮、PC 浮层 / 手机底部抽屉、拖放遮罩、rz/sz 模式。
// iframe 里的 term-inject.js 负责粘贴、拖放、rz/sz 横幅检测，通过 postMessage 交给这里（§5）；上传见 shared/uploader.js。
import { Download, Image as ImageIcon, RotateCw, Upload, X } from 'lucide-react';
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { frameOrigin, termFocus, termPaste, termSend } from '../shared/direct.js';
import { errorText } from '../shared/i18n.js';
import { uploadStore } from '../shared/uploader.js';
import { CopyButton, useToast } from '../shared/ui.jsx';
import { agentPastes, formatBytes, isAiInstance, joinPaths, routeFor, transferAction } from '../shared/xfer.js';

const REMOTE_QUIET_MS = 120000; // 关掉“rz 在另一台电脑上”的说明后，这段时间内同类横幅不再弹出
const POLL_MS = 3000;
const CANCEL_ZMODEM = '\x18'.repeat(5);
const FINISHED = ['done', 'error', 'canceled'];

const noFocus = (e) => e.preventDefault(); // 手机上点按钮不抢终端焦点，免得软键盘被收起或弹出

function useFilesText() {
  const { t } = useTranslation();
  const err = (e) => {
    if (!e) return '';
    return t(`files.err.${e.code ?? 'INTERNAL'}`, { ...(e.params ?? {}), defaultValue: errorText(t, e) });
  };
  return { t, err };
}

/** 软键盘弹出时的可见高度（抽屉最高占它的 70%）。 */
function useVisibleHeight(enabled) {
  const [h, setH] = useState(() => window.visualViewport?.height ?? window.innerHeight);
  useEffect(() => {
    if (!enabled) return undefined;
    const vv = window.visualViewport;
    const on = () => setH(vv?.height ?? window.innerHeight);
    on();
    (vv ?? window).addEventListener('resize', on);
    return () => (vv ?? window).removeEventListener('resize', on);
  }, [enabled]);
  return h;
}

/** 浮层位置：图标下方、右对齐，约 360px 宽。 */
function usePopoverPos(anchor, open) {
  const [pos, setPos] = useState(null);
  useLayoutEffect(() => {
    if (!open) return undefined;
    const on = () => {
      const r = anchor.current?.getBoundingClientRect();
      if (!r) return;
      const width = Math.min(360, window.innerWidth - 16);
      const right = Math.max(8, Math.min(window.innerWidth - r.right, window.innerWidth - width - 8));
      const top = r.bottom + 6;
      setPos({ top, right, width, maxHeight: Math.max(200, window.innerHeight - top - 12) });
    };
    on();
    window.addEventListener('resize', on);
    return () => window.removeEventListener('resize', on);
  }, [open]);
  return pos;
}

function ProgressRing({ ratio }) {
  const r = 15;
  const c = 2 * Math.PI * r;
  return (
    <svg className="files-ring" viewBox="0 0 36 36" aria-hidden="true">
      <circle cx="18" cy="18" r={r} className="files-ring-bg" />
      <circle cx="18" cy="18" r={r} className="files-ring-fg" strokeDasharray={c} strokeDashoffset={c * (1 - Math.min(1, ratio))} />
    </svg>
  );
}

// 第一次用终端时，在“上传”按钮下方提示一次上传/拖放/粘贴（只存在本浏览器里，存不了也只是多提示几次）。
const TIP_KEY = 'll-files-tip';
const TIP_MAX_SHOWS = 3; // 没点“知道了”时最多自动出现几次
const TIP_DELAY_MS = 1200;
const TIP_AUTOHIDE_MS = 20000;
let tipShowing = false; // 同一页有多个终端时只提示一个

function tipState() {
  try {
    return JSON.parse(localStorage.getItem(TIP_KEY) || '{}');
  } catch {
    return {};
  }
}
function saveTip(p) {
  try {
    localStorage.setItem(TIP_KEY, JSON.stringify({ ...tipState(), ...p }));
  } catch {
    // 隐私模式等
  }
}
const tipWanted = () => {
  const s = tipState();
  return !s.done && (s.n ?? 0) < TIP_MAX_SHOWS;
};

/** 提示气泡的位置：按钮下方，箭头对准按钮。悬浮小窗可以被拖动，所以显示期间定时重新计算。 */
function useTipPos(anchor, shown) {
  const [pos, setPos] = useState(null);
  useLayoutEffect(() => {
    if (!shown) return undefined;
    const on = () => {
      const r = anchor.current?.getBoundingClientRect();
      if (!r || !r.width) return setPos(null);
      const width = Math.min(300, window.innerWidth - 16);
      const center = r.left + r.width / 2;
      const left = Math.max(8, Math.min(center - width + 36, window.innerWidth - width - 8));
      setPos({ top: r.bottom + 10, left, width, arrow: Math.max(14, Math.min(width - 14, center - left)) });
    };
    on();
    const iv = setInterval(on, 400);
    window.addEventListener('resize', on);
    return () => {
      clearInterval(iv);
      window.removeEventListener('resize', on);
    };
  }, [shown]);
  return pos;
}

function TaskRow({ task, store, touch }) {
  const { t, err } = useFilesText();
  const pct = task.size ? Math.floor((Math.min(task.sent, task.size) / task.size) * 100) : 0;
  const busy = task.state === 'queued' || task.state === 'uploading' || task.state === 'retrying';
  return (
    <li className="files-task" data-state={task.state}>
      <div className="files-task-top">
        <span className="ellipsis files-task-name" title={task.name}>
          {task.name}
        </span>
        <span className="files-task-size">{formatBytes(task.size)}</span>
        {busy && (
          <button type="button" className="btn btn-plain btn-icon files-mini" onPointerDown={touch ? noFocus : undefined} onClick={() => store.cancel(task.key)} aria-label={t('files.cancel')} title={t('files.cancel')}>
            <X size={16} />
          </button>
        )}
        {task.state === 'error' && (
          <button type="button" className="btn btn-plain btn-icon files-mini" onPointerDown={touch ? noFocus : undefined} onClick={() => store.retry(task.key)} aria-label={t('files.retry')} title={t('files.retry')}>
            <RotateCw size={16} />
          </button>
        )}
      </div>
      {busy && (
        <div className="files-task-progress">
          <div className="files-bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={task.name}>
            <i style={{ width: `${pct}%` }} />
          </div>
          <span className="small muted">{task.state === 'queued' ? t('files.queued') : task.state === 'retrying' ? t('files.retrying') : `${pct}%`}</span>
        </div>
      )}
      {task.state === 'done' && (
        <div className="files-task-done">
          <span className="small files-ok">{t('files.saved')}</span>
          <span className="mono small grow" title={task.path}>
            {task.path}
          </span>
          <CopyButton text={task.path} variant="plain" label={false} />
        </div>
      )}
      {task.state === 'error' && <p className="small files-err">{err(task.error)}</p>}
      {task.state === 'canceled' && <p className="small muted">{t('files.canceled')}</p>}
    </li>
  );
}

/**
 * 终端的文件功能。返回 { button, layer }：button 放进标题栏，layer 放进终端区域的容器（position: relative）里，
 * 拖放遮罩和手机抽屉都覆盖在终端上方，不改变终端尺寸（避免 xterm 重排）。
 * frame：终端 iframe 的 ref；enabled：终端在运行且有权限；keybar：手机按键条是否显示（抽屉在它上方，不再留底部安全区）。
 */
export function useTermFiles({ id, inst, frame, enabled, touch, hostOs, keybar }) {
  const { t, err } = useFilesText();
  const toast = useToast();
  const store = uploadStore(id);
  const tasks = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState(null); // null | { mode: 'rz' | 'sz' | 'remote', key, transfer }
  const [info, setInfo] = useState(null);
  const [infoError, setInfoError] = useState(null);
  const [dragging, setDragging] = useState(false);
  const [aiMode, setAiMode] = useState('ai');
  const [insert, setInsert] = useState(false);
  const [clicked, setClicked] = useState(() => new Set());
  const clickedRef = useRef(clicked);
  const btn = useRef(null);
  const panel = useRef(null);
  const fileInput = useRef(null);
  const photoInput = useRef(null);
  const cache = useRef({ at: 0, p: null });
  const handled = useRef(new Set());
  const remoteQuietUntil = useRef(0);
  const modeRef = useRef(mode);
  modeRef.current = mode;
  const ai = isAiInstance(inst);
  const visibleH = useVisibleHeight(touch && open);
  const pos = usePopoverPos(btn, open && !touch);
  const [tip, setTip] = useState(false);
  const tipPos = useTipPos(btn, tip);

  // 第一次提示：终端可用后稍等再出现，到时自动收起（不算“知道了”，下次还会再提示，最多 TIP_MAX_SHOWS 次）。
  useEffect(() => {
    if (!enabled || !tipWanted()) return undefined;
    let hide;
    const show = setTimeout(() => {
      if (tipShowing || !tipWanted()) return;
      tipShowing = true;
      saveTip({ n: (tipState().n ?? 0) + 1 });
      setTip(true);
      hide = setTimeout(() => setTip(false), TIP_AUTOHIDE_MS);
    }, TIP_DELAY_MS);
    return () => {
      clearTimeout(show);
      clearTimeout(hide);
    };
  }, [enabled]);
  useEffect(() => {
    if (!tip) return undefined;
    return () => {
      tipShowing = false;
    };
  }, [tip]);
  const tipDone = () => {
    saveTip({ done: true });
    setTip(false);
  };

  // 告诉 term-inject.js：由管理台接管粘贴/拖放的文件。iframe 重新加载后元素会换，所以每次渲染都设置。
  useEffect(() => {
    if (frame.current) frame.current.looklookFiles = Boolean(enabled);
  });

  const loadInfo = useCallback(
    (force) => {
      const c = cache.current;
      if (!force && c.p && Date.now() - c.at < 5000) return c.p;
      const p = api.get(`/instances/${id}/files`).then((d) => {
        setInfo(d);
        setInfoError(null);
        return d;
      });
      cache.current = { at: Date.now(), p };
      p.catch((e) => {
        cache.current = { at: 0, p: null };
        setInfoError(e);
      });
      return p;
    },
    [id],
  );

  const quoteOs = (d) => (d?.sep === '\\' || d?.os === 'windows' || (!d && hostOs === 'windows') ? 'windows' : 'posix');
  const paste = (text) => termPaste(frame, text, { focus: !touch });

  const upload = (files, { dest, insert: fill }, after) => {
    if (!files.length) return;
    tipDone(); // 已经会用了，不用再提示
    store.add(files, {
      dest,
      onDone: async (fresh, all) => {
        const failed = all.filter((x) => x.state === 'error');
        if (failed.length) toast(t('files.failed', { name: failed[0].name, reason: err(failed[0].error) }), 'error');
        if (fresh.length && fill) {
          const d = await loadInfo().catch(() => null);
          const paths = fresh.map((x) => x.path);
          if (dest === 'attach' && ai) for (const text of agentPastes(paths, inst.launch, quoteOs(d))) paste(text);
          else paste(joinPaths(paths, quoteOs(d)));
        }
        after?.(fresh, all);
      },
    });
  };

  const close = useCallback(() => {
    const m = modeRef.current;
    if (m) {
      handled.current.add(m.key);
      if (m.mode === 'rz') {
        store.cancelWhere((x) => x.dest === 'rz' && !FINISHED.includes(x.state));
        api.post(`/instances/${id}/transfer/cancel`).catch(() => {});
      } else if (m.mode === 'sz') {
        api.post(`/instances/${id}/transfer/finish`).catch(() => {});
      } else {
        remoteQuietUntil.current = Date.now() + REMOTE_QUIET_MS;
      }
      setMode(null);
    }
    setOpen(false);
    if (!touch) termFocus(frame);
  }, [id, touch]);

  const endTransfer = (m) => {
    handled.current.add(m.key);
    if (modeRef.current?.key === m.key) {
      setMode(null);
      setOpen(false);
    }
  };

  const openPanel = () => {
    tipDone();
    setOpen(true);
    loadInfo(true).catch(() => {});
  };

  // 来自终端 iframe 的消息（§5）。
  const live = useRef(null);
  live.current = {
    files(files, source) {
      if (!enabled) return;
      upload(files, routeFor(source, { ai }));
    },
    async transfer(kind) {
      if (!enabled || hostOs === 'windows') return; // psmux 没有 rz/sz
      const d = await loadInfo(true).catch(() => null);
      const act = d && transferAction(kind, d.transfer);
      if (!act || handled.current.has(act.key) || modeRef.current?.key === act.key) return;
      if (act.mode === 'remote' && Date.now() < remoteQuietUntil.current) return;
      clickedRef.current = new Set();
      setClicked(clickedRef.current);
      setMode(act);
      setOpen(true);
    },
  };
  useEffect(() => {
    const on = (e) => {
      if (!frame.current || e.source !== frame.current.contentWindow) return; // 同页可能有多个终端
      if (e.origin !== frameOrigin(frame) || !e.data || typeof e.data !== 'object') return; // 本机直连时终端跨源
      const { type } = e.data;
      if (type === 'looklook:files' && Array.isArray(e.data.files)) live.current.files(e.data.files, e.data.source);
      else if (type === 'looklook:dragging') {
        setDragging(Boolean(e.data.on));
        if (e.data.on && !ai) loadInfo().catch(() => {});
      } else if (type === 'looklook:transfer') live.current.transfer(e.data.kind);
    };
    window.addEventListener('message', on);
    return () => window.removeEventListener('message', on);
  }, [ai]);

  // rz/sz 模式下定期确认进程还在（用户可能在终端里按了 Ctrl+C）。
  useEffect(() => {
    if (!mode || mode.mode === 'remote') return undefined;
    const iv = setInterval(async () => {
      if (mode.mode === 'rz' && store.getSnapshot().some((x) => x.dest === 'rz' && !FINISHED.includes(x.state))) return;
      const d = await loadInfo(true).catch(() => null);
      if (d && transferAction(mode.mode, d.transfer)?.key !== mode.key) endTransfer(mode);
    }, POLL_MS);
    return () => clearInterval(iv);
  }, [mode]);

  // PC：Esc 关闭；普通模式下点到外面（包括点回终端）也关闭。rz/sz 模式关闭等于取消，只认 Esc 和 ✕。
  useEffect(() => {
    if (!open || touch) return undefined;
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        close();
      }
    };
    const outside = (e) => {
      if (modeRef.current) return;
      if (panel.current?.contains(e.target) || btn.current?.contains(e.target)) return;
      close();
    };
    const blur = () =>
      setTimeout(() => {
        if (!modeRef.current && frame.current && document.activeElement === frame.current) close();
      });
    window.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', outside, true);
    window.addEventListener('blur', blur);
    return () => {
      window.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('blur', blur);
    };
  }, [open, touch, close]);

  // PC 上打开后把焦点放到第一个按钮上（键盘可以直接回车选文件）；手机上不动焦点。
  useEffect(() => {
    if (open && !touch) setTimeout(() => panel.current?.querySelector('.files-body button:not(:disabled)')?.focus(), 0);
  }, [open, mode?.key]);

  const pick = (list) => {
    const files = Array.from(list ?? []);
    if (!files.length) return;
    const m = modeRef.current;
    if (m?.mode === 'rz') {
      upload(files, routeFor('rz'), (fresh, all) => {
        if (all.some((x) => x.error?.code === 'TRANSFER_GONE')) {
          toast(t('files.err.TRANSFER_GONE'), 'error');
          return endTransfer(m);
        }
        if (!fresh.length) return;
        api
          .post(`/instances/${id}/transfer/finish`)
          .catch(() => {})
          .finally(() => {
            toast(t('files.rz_saved', { n: fresh.length, dir: m.transfer.cwd ?? '' }));
            endTransfer(m);
          });
      });
      return;
    }
    upload(files, routeFor('pick', { ai, aiMode, insertChecked: insert }));
  };

  const downloadUrl = (path) => `/api/instances/${id}/download?path=${encodeURIComponent(path)}`;
  const markDownloaded = (path) => {
    const m = modeRef.current;
    const next = new Set(clickedRef.current).add(path);
    clickedRef.current = next;
    setClicked(next);
    const files = m?.transfer?.files ?? [];
    if (m?.mode === 'sz' && files.length && files.every((f) => next.has(f.path)) && !handled.current.has(m.key)) {
      // 全部点过下载后结束 sz（下载由后端直接读文件，不依赖 sz 进程）。
      handled.current.add(m.key);
      setTimeout(() => {
        api.post(`/instances/${id}/transfer/finish`).catch(() => {});
        endTransfer(m);
      }, 1500);
    }
  };
  const downloadAll = () => {
    (mode?.transfer?.files ?? []).forEach((f, i) =>
      setTimeout(() => {
        const a = document.createElement('a');
        a.href = downloadUrl(f.path);
        a.download = f.name;
        document.body.appendChild(a);
        a.click();
        a.remove();
        markDownloaded(f.path);
      }, i * 400),
    );
  };

  const cancelRemote = () => {
    termSend(frame, CANCEL_ZMODEM, { focus: !touch });
    toast(t('files.remote_canceled'));
    remoteQuietUntil.current = Date.now() + REMOTE_QUIET_MS;
    setMode(null);
    setOpen(false);
  };

  // ---------- 渲染 ----------
  const { active, ratio } = store.progress();
  const tap = touch ? noFocus : undefined;
  const button = enabled ? (
    <button
      ref={btn}
      type="button"
      className="btn btn-plain files-btn"
      onPointerDown={tap}
      onClick={() => (open ? close() : openPanel())}
      aria-expanded={open}
      aria-haspopup="dialog"
      aria-label={active ? t('files.uploading_n', { n: active }) : t('files.button_hint')}
      title={active ? t('files.uploading_n', { n: active }) : t('files.button_hint')}
    >
      <span className="files-ico">
        <Upload size={17} />
        {active > 0 && <ProgressRing ratio={ratio} />}
      </span>
      <span className="files-btn-label">{t('files.button')}</span>
      {active > 1 && <span className="files-count">{active}</span>}
    </button>
  ) : null;

  const cwdNote = info?.cwd_source === 'workdir' ? t('files.cwd_workdir') : t('files.cwd_pane');
  const toAi = ai && aiMode === 'ai';
  let title = t('files.title');
  let body;
  if (mode?.mode === 'remote') {
    title = t(mode.transfer.kind === 'sz' ? 'files.remote_title_sz' : 'files.remote_title_rz');
    body = (
      <>
        <p className="files-text">{t('files.remote_body')}</p>
        <div className="inline">
          <button type="button" className="btn btn-danger" onPointerDown={tap} onClick={cancelRemote}>
            {t(mode.transfer.kind === 'sz' ? 'files.remote_cancel_sz' : 'files.remote_cancel_rz')}
          </button>
        </div>
      </>
    );
  } else if (mode?.mode === 'sz') {
    const files = mode.transfer.files ?? [];
    title = t('files.sz_title', { n: files.length });
    body = (
      <>
        <ul className="files-list">
          {files.map((f) => (
            <li key={f.path} className="files-task">
              <div className="files-task-top">
                <span className="ellipsis files-task-name" title={f.path}>
                  {f.name}
                </span>
                <span className="files-task-size">{formatBytes(f.size)}</span>
                <a className="btn btn-secondary files-dl" href={downloadUrl(f.path)} download={f.name} onPointerDown={tap} onClick={() => markDownloaded(f.path)}>
                  <Download size={16} aria-hidden="true" />
                  {clicked.has(f.path) ? t('files.downloaded') : t('files.download')}
                </a>
              </div>
            </li>
          ))}
        </ul>
        {files.length > 1 && (
          <button type="button" className="btn btn-primary" onPointerDown={tap} onClick={downloadAll}>
            <Download size={18} aria-hidden="true" />
            {t('files.download_all')}
          </button>
        )}
        <p className="small muted">{t('files.sz_hint')}</p>
      </>
    );
  } else {
    const rz = mode?.mode === 'rz';
    if (rz) title = t('files.rz_title');
    body = (
      <>
        {ai && !rz && (
          <div className="seg files-seg" role="group" aria-label={t('files.where')}>
            <button type="button" aria-pressed={toAi} onPointerDown={tap} onClick={() => setAiMode('ai')}>
              {t('files.to_ai')}
            </button>
            <button type="button" aria-pressed={!toAi} onPointerDown={tap} onClick={() => setAiMode('cwd')}>
              {t('files.to_cwd')}
            </button>
          </div>
        )}
        <p className="files-dest">
          {rz ? (
            <>
              {t('files.rz_body')} <span className="mono">{mode.transfer.cwd}</span>
            </>
          ) : toAi ? (
            <>
              {t('files.ai_dest')} <span className="muted small">{t('files.ai_dest_hint')}</span>
            </>
          ) : (
            <>
              {t('files.save_to')} <span className="mono">{info?.cwd ?? (infoError ? '—' : '…')}</span> <span className="muted small">{cwdNote}</span>
            </>
          )}
        </p>
        {infoError && !rz && <p className="small files-err">{err(infoError)}</p>}
        <div className="inline">
          <button type="button" className="btn btn-primary" onPointerDown={tap} onClick={() => fileInput.current?.click()}>
            <Upload size={18} aria-hidden="true" />
            {t('files.pick')}
          </button>
          {touch && (
            <button type="button" className="btn btn-secondary" onPointerDown={tap} onClick={() => photoInput.current?.click()}>
              <ImageIcon size={18} aria-hidden="true" />
              {t('files.photos')}
            </button>
          )}
        </div>
        <input ref={fileInput} type="file" multiple hidden onChange={(e) => { pick(e.target.files); e.target.value = ''; }} />
        <input ref={photoInput} type="file" accept="image/*" multiple hidden onChange={(e) => { pick(e.target.files); e.target.value = ''; }} />
        {!rz && !toAi && (
          <label className="check-row files-check">
            <input type="checkbox" checked={insert} onChange={(e) => setInsert(e.target.checked)} onPointerDown={tap} />
            <span>{t('files.insert')}</span>
          </label>
        )}
        {rz ? <p className="small muted">{t('files.rz_hint')}</p> : !touch && <p className="small muted">{t('files.hint_pc')}</p>}
      </>
    );
  }

  const finished = tasks.some((x) => FINISHED.includes(x.state));
  const content = (
    <>
      <header className="files-head">
        <h2>{title}</h2>
        <button type="button" className="btn btn-plain btn-icon" onPointerDown={tap} onClick={close} aria-label={t('files.close')} title={t('files.close')}>
          <X size={18} />
        </button>
      </header>
      <div className="files-body">
        {body}
        {tasks.length > 0 && (
          <>
            <ul className="files-list" aria-live="polite">
              {tasks.map((task) => (
                <TaskRow key={task.key} task={task} store={store} touch={touch} />
              ))}
            </ul>
            {finished && (
              <button type="button" className="btn btn-plain files-clear" onPointerDown={tap} onClick={() => store.clearFinished()}>
                {t('files.clear')}
              </button>
            )}
          </>
        )}
      </div>
    </>
  );

  const dropDir = ai ? t('files.drop_ai') : info?.cwd ? t('files.drop_to', { dir: info.cwd }) : t('files.drop_here');
  const portalTarget = typeof document !== 'undefined' ? (document.fullscreenElement ?? document.body) : null;
  const layer = (
    <>
      {dragging && enabled && (
        <div className="files-drop" aria-hidden="true">
          <div>
            <Upload size={28} />
            <p>{dropDir}</p>
          </div>
        </div>
      )}
      {open && enabled && touch && (
        <section
          ref={panel}
          className={`files-panel files-sheet ${keybar ? '' : 'files-sheet-safe'}`}
          style={{ '--files-max-h': `${Math.round(visibleH * 0.7)}px` }}
          role="dialog"
          aria-label={title}
        >
          {content}
        </section>
      )}
      {open && enabled && !touch && pos && portalTarget &&
        createPortal(
          <section ref={panel} className="files-panel files-pop" style={pos} role="dialog" aria-label={title}>
            {content}
          </section>,
          portalTarget,
        )}
      {tip && enabled && !open && tipPos && portalTarget &&
        createPortal(
          <div className="files-tip" style={{ top: tipPos.top, left: tipPos.left, width: tipPos.width, '--arrow': `${tipPos.arrow}px` }} role="status">
            <p className="files-tip-title">{t('files.tip_title')}</p>
            <p>{t(`files.tip_${touch ? 'touch' : 'pc'}${ai ? '_ai' : ''}`)}</p>
            <div className="files-tip-actions">
              <button type="button" className="btn btn-plain" onPointerDown={tap} onClick={tipDone}>
                {t('files.tip_ok')}
              </button>
              <button type="button" className="btn btn-primary" onPointerDown={tap} onClick={openPanel}>
                <Upload size={15} />
                {t('files.tip_try')}
              </button>
            </div>
          </div>,
          portalTarget,
        )}
    </>
  );
  return { button, layer };
}
