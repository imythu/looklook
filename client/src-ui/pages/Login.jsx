// 登录：用看看账户登录这台电脑。每个账户同时登录的电脑数有上限（平台判定，默认 7 台）。
import { LogIn } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { SafetyNote, useStatus, webUrl } from '../App.jsx';
import { api } from '../shared/api.js';
import { osLabel } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { Button, ErrorNote, Field, Note, Spinner, Time, useAction } from '../shared/ui.jsx';

function AlreadyLoggedIn({ params }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  return (
    <Note kind="warn">
      <b>{t('login.busy_title', { name: params.device_name || t('login.other_computer') })}</b>
      <p style={{ margin: '4px 0 8px' }}>{t('login.busy_body')}</p>
      {params.last_seen_at && (
        <p className="small" style={{ margin: '0 0 8px' }}>
          {t('login.busy_seen')} <Time value={params.last_seen_at} relative />
        </p>
      )}
      <a className="btn btn-ghost" href={webUrl(status, '/devices')} target="_blank" rel="noopener noreferrer">
        {t('login.busy_action')}
      </a>
    </Note>
  );
}

/** 已经有上限台数的电脑登录了：列出它们，请用户到网页上让一台退出登录后再登录。 */
function DeviceLimit({ params }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const list = Array.isArray(params.devices) ? params.devices : [];
  return (
    <Note kind="warn">
      <b>{t('devices.limit_title', { max: params.max ?? list.length })}</b>
      <p style={{ margin: '4px 0 8px' }}>{t('devices.limit_body')}</p>
      {list.length > 0 && (
        <ul className="device-limit-list">
          {list.map((d, i) => (
            <li key={d.id ?? i}>
              <span className="grow ellipsis">
                <b>{d.name || t('devices.unnamed')}</b>
                {d.os && <span className="muted small"> · {osLabel(d.os)}</span>}
              </span>
              {d.online ? (
                <span className="badge badge-ok">{t('devices.online')}</span>
              ) : d.last_seen_at ? (
                <span className="muted small">
                  {t('devices.last_seen')} <Time value={d.last_seen_at} only="relative" />
                </span>
              ) : (
                <span className="badge">{t('devices.offline')}</span>
              )}
            </li>
          ))}
        </ul>
      )}
      <a className="btn btn-ghost" href={webUrl(status, '/devices')} target="_blank" rel="noopener noreferrer">
        {t('devices.limit_action')}
      </a>
    </Note>
  );
}

/** 给第一次打开、还不知道看看是做什么的用户：几个最常见的问题。 */
function WhatIsThis() {
  const { t } = useTranslation();
  return (
    <details className="card qa">
      <summary>{t('login.qa_title')}</summary>
      <dl>
        {['what', 'vs', 'better', 'install'].map((k) => (
          <div key={k}>
            <dt>{t(`login.qa.${k}.q`)}</dt>
            <dd>{t(`login.qa.${k}.a`)}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}

function ServerSetting() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const [editing, setEditing] = useState(false);
  const [url, setUrl] = useState(status.account.server);
  const { busy, error, run } = useAction();
  if (status.access !== 'local') return null;
  if (!editing) {
    return (
      <p className="auth-foot">
        {t('login.server', { server: status.account.server.replace(/^https?:\/\//, '') })}{' '}
        <button type="button" className="btn btn-plain" style={{ minHeight: 32, padding: '0 6px' }} onClick={() => setEditing(true)}>
          {t('login.server_change')}
        </button>
      </p>
    );
  }
  return (
    <form
      className="card"
      style={{ marginTop: 16 }}
      onSubmit={async (e) => {
        e.preventDefault();
        const ok = await run(() => api.put('/server', { url }));
        if (ok !== undefined) {
          setEditing(false);
          reload();
        }
      }}
    >
      <Field label={t('login.server_label')} hint={t('login.server_hint')}>
        <input value={url} onChange={(e) => setUrl(e.target.value)} inputMode="url" autoComplete="off" spellCheck={false} />
      </Field>
      <ErrorNote error={error} />
      <div className="modal-actions" style={{ marginTop: 0 }}>
        <Button variant="ghost" onClick={() => setEditing(false)}>
          {t('action.cancel')}
        </Button>
        <Button type="submit" busy={busy}>
          {t('action.save')}
        </Button>
      </div>
    </form>
  );
}

export default function Login() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const notice = status.account.notice;
  // 授权进度：null = 还没开始；否则是本机 `/auth/status` 的返回。刷新页面时从 status.account.login 接着显示。
  const initial = status.account.login;
  const [flow, setFlow] = useState(initial && initial.state !== 'idle' ? initial : null);
  const { busy, error, run } = useAction();
  const pending = flow?.state === 'pending';

  useEffect(() => {
    if (!pending) return undefined;
    let alive = true;
    const id = setInterval(async () => {
      try {
        const s = await api.get('/auth/status');
        if (!alive) return;
        if (s.state === 'approved') reload();
        else if (s.state === 'idle') setFlow(null);
        else setFlow(s);
      } catch {
        /* 本机接口暂时不可达时继续等 */
      }
    }, 2000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [pending, reload]);

  const start = async () => {
    const r = await run(() => api.post('/auth/login'));
    if (r) setFlow({ state: 'pending', ...r });
  };
  const cancel = async () => {
    await api.post('/auth/login/cancel').catch(() => {});
    setFlow(null);
  };
  const failure = flow?.state === 'failed' ? flow.error : null;
  const shownError = error ?? failure;

  return (
    <div className="auth">
      <div className="auth-head">
        <h1>{t('login.title')}</h1>
        <p className="muted">{t('login.sub')}</p>
      </div>
      {notice && <Note kind="warn">{t(`login.notice.${notice.code}`, { defaultValue: t('login.notice.other') })}</Note>}
      <div className="card">
        {pending && (
          <>
            <p className="muted" style={{ margin: '0 0 8px' }}>
              {t('login.code_label')}
            </p>
            <p style={{ fontSize: 32, fontWeight: 700, letterSpacing: '0.12em', textAlign: 'center', margin: '0 0 12px', fontFamily: 'monospace' }}>
              {flow.user_code}
            </p>
            <Note kind="info">
              {t('login.opened')}{' '}
              <a href={flow.verification_url} target="_blank" rel="noopener noreferrer">
                {t('login.open_link')}
              </a>
            </Note>
            <p className="muted small" style={{ display: 'flex', alignItems: 'center', gap: 8, margin: '12px 0' }}>
              <Spinner /> {t('login.waiting')}
            </p>
            <Button variant="ghost" block onClick={cancel}>
              {t('action.cancel')}
            </Button>
          </>
        )}
        {!pending && (
          <>
            {shownError?.code === 'DEVICE_LIMIT_REACHED' ? (
              <DeviceLimit params={shownError.params ?? {}} />
            ) : shownError?.code === 'DEVICE_ALREADY_LOGGED_IN' ? (
              <AlreadyLoggedIn params={shownError.params ?? {}} />
            ) : shownError?.code === 'MEMBERSHIP_EXPIRED' ? (
              <Note kind="error">
                {errorText(t, shownError)}{' '}
                <a href={webUrl(status, '/membership')} target="_blank" rel="noopener noreferrer">
                  {t('gate.renew')}
                </a>
              </Note>
            ) : shownError?.code === 'AUTHORIZATION_EXPIRED' ? (
              <Note kind="warn">{t('login.expired')}</Note>
            ) : shownError?.code === 'AUTHORIZATION_DENIED' ? (
              <Note kind="warn">{t('login.denied')}</Note>
            ) : (
              <ErrorNote error={shownError} />
            )}
            <Button block size="big" busy={busy} icon={LogIn} onClick={start}>
              {failure ? t('login.retry') : t('login.start')}
            </Button>
          </>
        )}
      </div>
      <p className="auth-foot">
        <a href={webUrl(status, '/register')} target="_blank" rel="noopener noreferrer">
          {t('login.register')}
        </a>
      </p>
      <WhatIsThis />
      <SafetyNote />
      <ServerSetting />
    </div>
  );
}
