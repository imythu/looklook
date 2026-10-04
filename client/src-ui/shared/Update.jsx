// 检查更新与一键更新。后台自动检查（客户端定期向平台查询），有新版本时菜单上出现角标、页面顶部出现横幅；
// 用户关掉横幅后同一个版本不再提醒（记在客户端本机数据库里，手机上关掉电脑上也不再提醒），有更新的版本时再提醒。
// 下载、替换文件、重启在用户点“立即更新”后进行；开了“自动更新”（默认开）时，客户端也会在没有打开的终端 10 分钟后自己更新。
// 重启后页面自动刷新到新版本，并提示“已更新到 x.y.z”（可查看更新说明）。
import { ArrowRight, CheckCircle2, Download, RefreshCw, Sparkles, X } from 'lucide-react';
import { createContext, useContext, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';
import { api } from './api.js';
import { errorText } from './i18n.js';
import { formatTime } from './time.js';
import { Button, ErrorNote, Modal, Note, Switch, useAction, useToast } from './ui.jsx';

const ACTIVE = ['downloading', 'installing', 'restarting'];

/** 要不要提醒（角标、横幅）：有新版本且没被关掉；必须更新的版本关不掉。 */
export function shouldRemind(u) {
  return Boolean(u?.available && (u.required || !u.dismissed));
}

const DialogCtx = createContext(() => {});

/** 打开更新弹窗：`const openUpdate = useUpdateDialog(); openUpdate()` */
export function useUpdateDialog() {
  return useContext(DialogCtx);
}

export function UpdateProvider({ children }) {
  const [open, setOpen] = useState(false);
  return (
    <DialogCtx.Provider value={() => setOpen(true)}>
      {children}
      <UpdateDialog open={open} onClose={() => setOpen(false)} />
    </DialogCtx.Provider>
  );
}

function fmtSize(n) {
  if (!n) return '';
  return n > 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${Math.round(n / 1024)} KB`;
}

function Progress({ job }) {
  const { t } = useTranslation();
  const pct = job.state === 'downloading' && job.total ? Math.min(100, Math.round((job.done / job.total) * 100)) : null;
  const label =
    job.state === 'downloading'
      ? t('update.downloading', { done: fmtSize(job.done) || '0 KB', total: fmtSize(job.total) || '?' })
      : t(`update.${job.state}`);
  return (
    <div className="update-progress" role="status" aria-live="polite">
      <div className="update-bar" data-indeterminate={pct == null || undefined}>
        <span style={pct == null ? undefined : { width: `${pct}%` }} />
      </div>
      <p className="small muted">{label}</p>
    </div>
  );
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
  // 更新进行中：每秒刷新进度；重启期间连不上是正常的，版本变了就刷新页面
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
  const install = () => run(() => api.post('/update/install')).then((r) => r && reload());
  const later = async () => {
    if (u.available && !u.required && u.version) await api.post('/update/dismiss', { version: u.version }).catch(() => {});
    reload();
    onClose();
  };
  return (
    <Modal open={open} onClose={active ? undefined : onClose} title={t('update.title')}>
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
            <Progress job={job} />
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
        ) : active ? null : (
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

/** 顶部横幅：有新版本时提醒；× 关掉后这个版本不再提醒。 */
export function UpdateBanner() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const openUpdate = useUpdateDialog();
  const u = status.update;
  if (!shouldRemind(u)) return null;
  const dismiss = async () => {
    await api.post('/update/dismiss', { version: u.version }).catch(() => {});
    reload();
  };
  return (
    <div className={`banner ${u.required ? 'banner-warn' : 'banner-info'}`}>
      <Sparkles size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">{t(u.required ? 'gate.update_required' : 'gate.update', { version: u.version })}</span>
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
  const [open, setOpen] = useState(false);
  const j = status.update?.just_updated;
  if (!j) return null;
  const seen = async () => {
    setOpen(false);
    await api.post('/update/seen').catch(() => {});
    reload();
  };
  return (
    <div className="banner banner-ok" role="status">
      <CheckCircle2 size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">{t(j.auto ? 'update.just_updated_auto' : 'update.just_updated', { from: j.from, version: status.version })}</span>
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
    const r = await run(() => api.put('/settings', { ...status.settings, auto_update: v }));
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

/** 关于里的版本与检查更新。 */
export function UpdateCheck() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const openUpdate = useUpdateDialog();
  const u = status.update ?? {};
  return (
    <>
    <AutoUpdate />
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
