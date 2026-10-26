// 检查更新与一键更新。后台自动检查（客户端定期向平台查询），有新版本时菜单上出现角标、页面顶部出现横幅；
// 用户关掉横幅后同一个版本不再提醒（记在客户端本机数据库里，手机上关掉电脑上也不再提醒），有更新的版本时再提醒。
// 下载、替换文件、重启在用户点“立即更新”后进行；开了“自动更新”（默认开）时，客户端也会在没有打开的终端 10 分钟后自己更新。
// 更新在后台进行：弹窗可以随时关掉（“在后台更新”），进度改在顶部横幅里显示，页面照常可以操作。
// 重启后页面自动刷新到新版本，并提示“已更新到 x.y.z”（可查看更新说明）。
import { ArrowRight, CheckCircle2, Download, RefreshCw, Sparkles, X } from 'lucide-react';
import { createContext, useContext, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';
import { api } from './api.js';
import { deviceName, useDevices } from './devices.jsx';
import { errorText } from './i18n.js';
import { formatTime } from './time.js';
import { Button, ErrorNote, Modal, Note, Switch, useAction, useToast } from './ui.jsx';

const ACTIVE = ['downloading', 'installing', 'restarting'];
/** 重启超过这么久还没换成新版本：提示刷新页面看看（可能新版本没能自己启动） */
const SLOW_RESTART_MS = 60_000;

/**
 * 这个浏览器里关掉过的提示（`dismiss:<版本>`、`seen:<版本>`）。关掉时先记在这里、马上藏起来，
 * 不等服务端：关之前发出的状态请求晚回来、或者关的请求没送到时，提示不会又冒出来。服务端记的仍然是准的（别的设备也不再提醒）。
 */
const CLOSED_KEY = 'looklook.update-closed';
function closedSet() {
  try {
    return new Set(JSON.parse(localStorage.getItem(CLOSED_KEY) || '[]'));
  } catch {
    return new Set();
  }
}
function markClosed(key) {
  try {
    localStorage.setItem(CLOSED_KEY, JSON.stringify([...closedSet(), key].slice(-20)));
  } catch {
    /* 隐私模式等存不了：只靠服务端 */
  }
  window.dispatchEvent(new Event(CLOSED_KEY));
}
/** 关掉提示后让用到它的组件重新渲染 */
function useClosed() {
  const [, bump] = useState(0);
  useEffect(() => {
    const on = () => bump((n) => n + 1);
    window.addEventListener(CLOSED_KEY, on);
    window.addEventListener('storage', on);
    return () => {
      window.removeEventListener(CLOSED_KEY, on);
      window.removeEventListener('storage', on);
    };
  }, []);
}

/** 要不要提醒（角标、横幅）：有新版本且没被关掉；必须更新的版本关不掉。 */
export function shouldRemind(u) {
  return Boolean(u?.available && (u.required || (!u.dismissed && !closedSet().has(`dismiss:${u.version}`))));
}

/** 关掉“发现新版本”：先在本机藏起来，再告诉服务端；没送到时提示 */
async function dismissVersion(version, toast, t) {
  markClosed(`dismiss:${version}`);
  await api.post('/update/dismiss', { version }).catch((e) => toast?.(errorText(t, e), 'error'));
}

/**
 * 远程打开（看看网页、远程域名）或能切换电脑时，提醒说的是哪台电脑的看看：横幅前面带上它的名字。
 * 在这台电脑上本机打开时不带（就是这台）。
 */
function DeviceLabel() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  if (status.access === 'local' && !devices.multi) return null;
  const d = devices.multi ? devices.byId(devices.selected) : devices.items[0];
  const name = d ? deviceName(t, d) : status.settings?.device_name;
  return name ? <b>{t('update.on_device', { name })}</b> : null;
}

const DialogCtx = createContext(() => {});

/** 打开更新弹窗：`const openUpdate = useUpdateDialog(); openUpdate()` */
export function useUpdateDialog() {
  return useContext(DialogCtx);
}

export function UpdateProvider({ children }) {
  const [open, setOpen] = useState(false);
  useUpdateProgress();
  return (
    <DialogCtx.Provider value={() => setOpen(true)}>
      {children}
      <UpdateDialog open={open} onClose={() => setOpen(false)} />
    </DialogCtx.Provider>
  );
}

/** 带 `-` 的版本号（`1.38.0-beta.1`）是测试版 */
const isBeta = (v) => Boolean(v?.includes('-'));

function fmtSize(n) {
  if (!n) return '';
  return n > 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${Math.round(n / 1024)} KB`;
}

function Progress({ job }) {
  const { t } = useTranslation();
  const pct = job.state === 'downloading' && job.total ? Math.min(100, Math.round((job.done / job.total) * 100)) : null;
  const label = progressText(t, job);
  return (
    <div className="update-progress" role="status" aria-live="polite">
      <div className="update-bar" data-indeterminate={pct == null || undefined}>
        <span style={pct == null ? undefined : { width: `${pct}%` }} />
      </div>
      <p className="small muted">{label}</p>
    </div>
  );
}

/**
 * 更新进行中：每秒刷新进度；重启期间连不上是正常的，版本变了就刷新页面。
 * 放在 Provider 里而不是弹窗里：弹窗关掉（在后台更新）后进度照样走，换了版本照样自动刷新。
 */
function useUpdateProgress() {
  const { status, reload } = useStatus();
  const active = ACTIVE.includes(status.update?.job?.state);
  useEffect(() => {
    if (!active) return undefined;
    const from = status.version;
    const id = setInterval(async () => {
      try {
        const s = await api.get('/status');
        if (s.version !== from) window.location.reload();
        else reload();
      } catch {
        /* 正在重启 */
      }
    }, 1000);
    return () => clearInterval(id);
  }, [active]);
}

/** 进度的一句话：“正在下载 3.2 MB / 12 MB（27%）”“正在安装…”“正在重启…” */
function progressText(t, job) {
  if (job.state !== 'downloading') return t(`update.${job.state}`);
  const pct = job.total ? Math.min(100, Math.round((job.done / job.total) * 100)) : null;
  const text = t('update.downloading', { done: fmtSize(job.done) || '0 KB', total: fmtSize(job.total) || '?' });
  return pct == null ? text : `${text} · ${pct}%`;
}

/** 正在重启：过了一分钟还没换成新版本，给出“刷新页面”（新版本可能没能自己启动，或者页面没收到） */
function useSlowRestart(job) {
  const [slow, setSlow] = useState(false);
  const restarting = job.state === 'restarting';
  useEffect(() => {
    setSlow(false);
    if (!restarting) return undefined;
    const timer = setTimeout(() => setSlow(true), SLOW_RESTART_MS);
    return () => clearTimeout(timer);
  }, [restarting]);
  return slow;
}

function UpdateDialog({ open, onClose }) {
  const { t, i18n } = useTranslation();
  const { status, reload } = useStatus();
  const { busy, error, setError, run } = useAction();
  const u = status.update ?? {};
  const job = u.job ?? { state: 'idle' };
  const active = ACTIVE.includes(job.state);
  // 打开时查一次，拿到当前语言的更新说明
  useEffect(() => {
    if (!open) return;
    setError(null);
    run(() => api.get(`/update?locale=${i18n.language}`)).then((r) => r && reload());
  }, [open]);
  const slow = useSlowRestart(job);
  const install = () => run(() => api.post('/update/install')).then((r) => r && reload());
  const later = async () => {
    if (u.available && !u.required && u.version) await dismissVersion(u.version);
    reload();
    onClose();
  };
  return (
    <Modal open={open} onClose={onClose} title={t('update.title')}>
      {!u.available ? (
        <>
          <Note kind={u.check_error ? 'warn' : 'ok'}>
            {u.check_error ? t('update.check_failed') : t('update.latest', { version: u.current })}
          </Note>
          {u.checked_at && <p className="small muted">{t('update.checked_at', { when: formatTime(u.checked_at) })}</p>}
        </>
      ) : (
        <>
          <div className="update-versions">
            <span className="mono">{u.current}</span>
            <ArrowRight size={16} aria-hidden="true" />
            <b className="mono">{u.version}</b>
            {isBeta(u.version) && <span className="badge badge-brand">{t('update.beta_badge')}</span>}
            {u.required && <span className="badge badge-warn">{t('update.required_badge')}</span>}
          </div>
          {u.published_at && <p className="small muted">{t('update.published', { when: formatTime(u.published_at) })}</p>}
          {u.notes && <div className="update-notes">{u.notes}</div>}
          {job.state === 'failed' && (
            <Note kind="error">
              {errorText(t, { code: job.code, params: { detail: job.detail } })}
            </Note>
          )}
          {active ? (
            <>
              <Progress job={job} />
              <p className="small muted">{t('update.background_hint')}</p>
              {slow && <Note kind="warn">{t('update.slow_restart')}</Note>}
            </>
          ) : !u.can_install ? (
            <Note>{t(`update.manual.${u.cannot_reason ?? 'unsupported'}`)}</Note>
          ) : (
            <>
              <p className="small muted">{t('update.keeps_tasks')}</p>
              {u.auto && <p className="small muted">{t('update.auto_pending')}</p>}
            </>
          )}
        </>
      )}
      <ErrorNote error={error} />
      <div className="modal-actions">
        {!u.available ? (
          <>
            <Button variant="ghost" onClick={onClose}>
              {t('action.close')}
            </Button>
            <Button variant="secondary" icon={RefreshCw} busy={busy} onClick={() => run(() => api.get(`/update?locale=${i18n.language}`)).then((r) => r && reload())}>
              {t('settings.check_update')}
            </Button>
          </>
        ) : active ? (
          <>
            {slow && (
              <Button variant="secondary" icon={RefreshCw} onClick={() => window.location.reload()}>
                {t('update.reload_page')}
              </Button>
            )}
            <Button variant="ghost" onClick={onClose}>
              {t('update.background')}
            </Button>
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={later}>
              {t(u.required ? 'action.close' : 'update.later')}
            </Button>
            {u.can_install ? (
              <Button icon={Download} busy={busy} onClick={install}>
                {t(job.state === 'failed' ? 'update.retry' : 'update.install')}
              </Button>
            ) : (
              <a className="btn btn-primary" href={webUrl(status, '/download')} target="_blank" rel="noopener noreferrer">
                <Download size={18} aria-hidden="true" />
                {t('gate.download')}
              </a>
            )}
          </>
        )}
      </div>
    </Modal>
  );
}

/** 顶部横幅：有新版本时提醒；× 关掉后这个版本不再提醒。正在更新时显示进度（弹窗关掉后也看得到）。 */
export function UpdateBanner() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  useClosed();
  const openUpdate = useUpdateDialog();
  const u = status.update;
  const job = u?.job ?? { state: 'idle' };
  const slow = useSlowRestart(job);
  if (ACTIVE.includes(job.state)) {
    return (
      <div className="banner banner-info" role="status" aria-live="polite">
        <RefreshCw size={18} aria-hidden="true" className="spin" style={{ flex: 'none' }} />
        <span className="grow">
          <DeviceLabel />
          {t('update.banner_progress', { version: job.version || u.version || '' })} · {progressText(t, job)}
          {slow && <span style={{ display: 'block' }}>{t('update.slow_restart')}</span>}
        </span>
        {slow ? (
          <Button variant="ghost" onClick={() => window.location.reload()}>
            {t('update.reload_page')}
          </Button>
        ) : (
          <Button variant="ghost" onClick={openUpdate}>
            {t('update.view')}
          </Button>
        )}
      </div>
    );
  }
  if (!shouldRemind(u)) return null;
  const dismiss = async () => {
    await dismissVersion(u.version, toast, t);
    reload();
  };
  return (
    <div className={`banner ${u.required ? 'banner-warn' : 'banner-info'}`}>
      <Sparkles size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">
        <DeviceLabel />
        {t(u.required ? 'gate.update_required' : 'gate.update', { version: u.version })}
      </span>
      <Button variant="ghost" onClick={openUpdate}>
        {t('update.view')}
      </Button>
      {!u.required && (
        <button type="button" className="btn btn-plain btn-icon banner-x" onClick={dismiss} aria-label={t('update.dismiss')} title={t('update.dismiss')}>
          <X size={18} />
        </button>
      )}
    </div>
  );
}

/** 更新完成后的提示：“已更新到 x.y.z”，可查看这次的更新说明；× 关掉后不再出现。 */
export function JustUpdatedBanner() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  useClosed();
  const j = status.update?.just_updated;
  const key = `seen:${status.version}`;
  if (!j || closedSet().has(key)) return null;
  const seen = async () => {
    setOpen(false);
    markClosed(key);
    await api.post('/update/seen').catch((e) => toast(errorText(t, e), 'error'));
    reload();
  };
  return (
    <div className="banner banner-ok" role="status">
      <CheckCircle2 size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">
        <DeviceLabel />
        {t(j.auto ? 'update.just_updated_auto' : 'update.just_updated', { from: j.from, version: status.version })}
      </span>
      {j.notes && (
        <Button variant="ghost" onClick={() => setOpen(true)}>
          {t('update.whats_new')}
        </Button>
      )}
      <button type="button" className="btn btn-plain btn-icon banner-x" onClick={seen} aria-label={t('action.close')} title={t('action.close')}>
        <X size={18} />
      </button>
      <Modal open={open} onClose={() => setOpen(false)} title={t('update.whats_new_title', { version: status.version })}>
        <div className="update-notes">{j.notes}</div>
        <div className="modal-actions">
          <Button onClick={seen}>{t('update.got_it')}</Button>
        </div>
      </Modal>
    </div>
  );
}

/** 自动更新开关：立即保存。 */
function AutoUpdate() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const { busy, error, run } = useAction();
  const on = status.settings.auto_update !== false;
  const reason = status.update?.cannot_reason;
  const set = async (v) => {
    const r = await run(() => api.put('/settings', { auto_update: v }));
    if (r) {
      toast(t('common.saved'));
      reload();
    }
  };
  return (
    <div className="stack" style={{ gap: 6, marginBottom: 12 }}>
      <div className="between" style={{ flexWrap: 'nowrap' }}>
        <span>
          <b>{t('update.auto')}</b>
          <span className="small muted" style={{ display: 'block' }}>
            {t('update.auto_hint')}
          </span>
        </span>
        <Switch checked={on} disabled={busy} onChange={set} label={t('update.auto')} />
      </div>
      {on && ['dev', 'unsupported', 'readonly'].includes(reason) && <Note kind="warn">{t(`update.manual.${reason}`)}</Note>}
      <ErrorNote error={error} />
    </div>
  );
}

/** 接收测试版：立即保存并按新通道重查一次。 */
function BetaChannel() {
  const { t, i18n } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const { busy, error, run } = useAction();
  const on = status.settings.update_channel === 'beta';
  const set = async (v) => {
    const r = await run(async () => {
      await api.put('/settings', { update_channel: v ? 'beta' : 'stable' });
      return api.get(`/update?locale=${i18n.language}`);
    });
    if (r) {
      toast(t('common.saved'));
      reload();
    }
  };
  return (
    <div className="stack" style={{ gap: 6, marginBottom: 12 }}>
      <div className="between" style={{ flexWrap: 'nowrap' }}>
        <span>
          <b>{t('update.beta')}</b>
          <span className="small muted" style={{ display: 'block' }}>
            {t('update.beta_hint')}
          </span>
        </span>
        <Switch checked={on} disabled={busy} onChange={set} label={t('update.beta')} />
      </div>
      {!on && isBeta(status.version) && <Note>{t('update.beta_off_note', { version: status.version })}</Note>}
      <ErrorNote error={error} />
    </div>
  );
}

/** 关于里的版本与检查更新。 */
export function UpdateCheck() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const openUpdate = useUpdateDialog();
  const u = status.update ?? {};
  return (
    <>
    <AutoUpdate />
    <BetaChannel />
    <div className="between" style={{ flexWrap: 'wrap', gap: 10 }}>
      <span className="small">
        {u.available ? (
          <b className="update-dot-text">{t('gate.update', { version: u.version })}</b>
        ) : (
          <span className="muted">{u.checked_at ? t('update.checked_at', { when: formatTime(u.checked_at) }) : t('update.auto_check')}</span>
        )}
      </span>
      <Button variant={u.available ? 'primary' : 'ghost'} icon={u.available ? Download : RefreshCw} onClick={openUpdate}>
        {t(u.available ? 'update.view' : 'settings.check_update')}
      </Button>
    </div>
    </>
  );
}
