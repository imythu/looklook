// 本机网页：把电脑上正在运行的网页（例如 localhost:5173）放到自己的专属地址下，在外面也能打开。
// 每个账户最多 10 个；只有本人登录后才能打开，或在有效期内临时分享给指定的看看账户。数据登记在看看服务端。
import { Bot, ExternalLink, Globe, Plus, Trash2, UserPlus, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link } from '../shared/router.jsx';
import { formatTime, relativeTime } from '../shared/time.js';
import { Button, Card, CopyButton, Empty, ErrorNote, Field, Modal, Note, Skeleton, Switch, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

// 分享有效期可选项（小时），服务端上限 7 天
const SHARE_HOURS = [1, 24, 72, 168];

/**
 * 临时分享：在有效期内允许指定的看看账户打开这个网页。对方用自己的账户登录后打开网页地址即可，
 * 到期或取消后马上失效。分享出去等于让对方使用这台电脑上的这个服务，所以先给出醒目的安全提醒、要求确认。
 */
function ShareDialog({ page, onClose }) {
  const { t } = useTranslation();
  const toast = useToast();
  const open = Boolean(page);
  const [list, error, reload] = useLoad(() => (page ? api.get(`/tunnels/${page.id}/shares`) : Promise.resolve(null)), [page?.id]);
  const [username, setUsername] = useState('');
  const [hours, setHours] = useState(24);
  const [ack, setAck] = useState(false);
  const [done, setDone] = useState(null);
  const [revoking, setRevoking] = useState(null);
  const { busy, error: saveError, setError, run } = useAction();
  useEffect(() => {
    if (!open) return;
    setUsername('');
    setHours(24);
    setAck(false);
    setDone(null);
    setError(null);
  }, [open, page?.id]);
  const items = list?.items ?? [];
  const full = list && items.length >= list.max;
  const submit = async (e) => {
    e.preventDefault();
    const r = await run(() => api.post(`/tunnels/${page.id}/shares`, { username: username.trim(), hours }));
    if (r) {
      setDone(r);
      setUsername('');
      setAck(false);
      reload();
    }
  };
  const revoke = async (sh) => {
    setRevoking(sh.id);
    try {
      await api.del(`/tunnels/${page.id}/shares/${sh.id}`);
      toast(t('pages.share.revoked', { username: sh.username }));
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setRevoking(null);
      reload();
    }
  };
  return (
    <Modal open={open} onClose={onClose} title={page ? t('pages.share.title', { name: page.name }) : ''}>
      <Note kind="error">
        <b>{t('pages.share.warn_title')}</b>
        <span style={{ display: 'block', marginTop: 4 }}>{t('pages.share.warn_body', { port: page?.target_port })}</span>
      </Note>
      {done && (
        <Note kind="info">
          <span style={{ display: 'block' }}>{t('pages.share.done', { username: done.username, until: formatTime(done.expires_at) })}</span>
          <span className="inline" style={{ flexWrap: 'nowrap', marginTop: 6 }}>
            <span className="mono small ellipsis grow">{page?.url}</span>
            <CopyButton text={page?.url ?? ''} />
          </span>
        </Note>
      )}
      <ErrorNote error={error} />
      {items.length > 0 && (
        <>
          <p className="field-label" style={{ margin: '0 0 6px' }}>
            {t('pages.share.current')}
          </p>
          <ul className="list share-list">
            {items.map((sh) => (
              <li key={sh.id} className="page-row">
                <span className="grow">
                  <b className="ellipsis">{sh.username}</b>
                  <span className="muted small" title={formatTime(sh.expires_at)}>
                    {t('pages.share.until', { until: formatTime(sh.expires_at), left: relativeTime(t, sh.expires_at) })}
                  </span>
                </span>
                <Button variant="ghost" icon={X} busy={revoking === sh.id} onClick={() => revoke(sh)}>
                  {t('pages.share.revoke')}
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}
      {full ? (
        <Note kind="warn">{t('pages.share.full', { max: list.max })}</Note>
      ) : (
        <form onSubmit={submit}>
          <Field label={t('pages.share.username')} hint={t('pages.share.username_hint')}>
            <input value={username} onChange={(e) => setUsername(e.target.value)} maxLength={32} autoComplete="off" spellCheck={false} required />
          </Field>
          <Field label={t('pages.share.hours')}>
            <select value={hours} onChange={(e) => setHours(Number(e.target.value))}>
              {SHARE_HOURS.map((h) => (
                <option key={h} value={h}>
                  {t(`pages.share.hours_${h}`)}
                </option>
              ))}
            </select>
          </Field>
          <label className="row" style={{ alignItems: 'flex-start', gap: 8, cursor: 'pointer', marginBottom: 12 }}>
            <input type="checkbox" checked={ack} onChange={(e) => setAck(e.target.checked)} style={{ marginTop: 3 }} />
            <span>{t('pages.share.ack')}</span>
          </label>
          <ErrorNote error={saveError} />
          <div className="modal-actions">
            <Button variant="ghost" onClick={onClose}>
              {t('action.close')}
            </Button>
            <Button type="submit" variant="danger" icon={UserPlus} busy={busy} disabled={!ack || !username.trim()}>
              {t('pages.share.submit')}
            </Button>
          </div>
        </form>
      )}
    </Modal>
  );
}

function AddForm({ count, max, onAdded }) {
  const { t } = useTranslation();
  const [name, setName] = useState('');
  const [port, setPort] = useState('');
  const { busy, error, run } = useAction();
  const toast = useToast();
  const full = count >= max;
  const submit = async (e) => {
    e.preventDefault();
    const n = Number(port);
    const r = await run(() => api.post('/tunnels', { name: name.trim() || `localhost:${n}`, target_port: n }));
    if (r) {
      setName('');
      setPort('');
      toast(t('pages.added'));
      onAdded();
    }
  };
  return (
    <Card title={t('pages.add_title')} actions={<span className="muted small">{t('pages.count', { n: count, max })}</span>}>
      {full ? (
        <Note kind="warn">{t('pages.full', { max })}</Note>
      ) : (
        <form onSubmit={submit}>
          <Field
            label={t('pages.port')}
            hint={
              <>
                {t('pages.port_hint')}{' '}
                <span className="port-example">
                  localhost:<mark>5173</mark>
                </span>
              </>
            }
          >
            <input value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, '').slice(0, 5))} inputMode="numeric" placeholder="5173" required />
          </Field>
          <Field label={t('pages.name')}>
            <input value={name} onChange={(e) => setName(e.target.value)} maxLength={32} placeholder={t('pages.name_placeholder')} />
          </Field>
          <ErrorNote error={error} />
          <Button type="submit" icon={Plus} busy={busy} disabled={!port}>
            {t('pages.add')}
          </Button>
        </form>
      )}
    </Card>
  );
}

function PageRow({ p, onChanged, onShare }) {
  const { t } = useTranslation();
  const confirm = useConfirm();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const enabled = p.status === 'enabled';
  const act = async (fn) => {
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  const remove = async () => {
    if (await confirm({ title: t('pages.delete_title', { name: p.name }), body: t('pages.delete_body'), confirm: t('action.delete'), danger: true })) {
      act(() => api.del(`/tunnels/${p.id}`));
    }
  };
  return (
    <li className="page-row">
      <span className="grow">
        <b className="ellipsis">{p.name}</b>
        <span className="muted small">{t('pages.local', { port: p.target_port })}</span>
        <a className="mono small ellipsis" href={p.url} target="_blank" rel="noopener noreferrer">
          {p.url}
        </a>
      </span>
      <span className="inline" style={{ flexWrap: 'nowrap' }}>
        <Switch checked={enabled} disabled={busy} label={t('pages.enabled')} onChange={(v) => act(() => api.patch(`/tunnels/${p.id}`, { status: v ? 'enabled' : 'disabled' }))} />
        <CopyButton text={p.url} label={false} />
        <Button variant="ghost" icon={UserPlus} disabled={busy} aria-label={t('pages.share.button')} title={t('pages.share.button')} onClick={() => onShare(p)} />
        <a className="btn btn-ghost btn-icon" href={p.url} target="_blank" rel="noopener noreferrer" aria-label={t('action.open')}>
          <ExternalLink size={18} />
        </a>
        <Button variant="ghost" icon={Trash2} disabled={busy} aria-label={t('action.delete')} onClick={remove} />
      </span>
    </li>
  );
}

export default function Pages() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const [data, error, reload] = useLoad(() => api.get('/tunnels'));
  const [sharing, setSharing] = useState(null);
  const items = data?.items ?? [];
  return (
    <>
      <div className="page-head">
        <h1>{t('pages.title')}</h1>
      </div>
      <p className="muted">{t('pages.intro')}</p>
      {!status.account.allowed && <Note kind="warn">{t('pages.paused')}</Note>}
      <ErrorNote error={error} />
      {!data && !error ? (
        <Skeleton height={140} />
      ) : (
        data && (
          <>
            <Card title={t('pages.list_title')}>
              {items.length === 0 ? (
                <Empty icon={Globe} title={t('pages.empty_title')}>
                  <p>{t('pages.empty_body')}</p>
                </Empty>
              ) : (
                <ul className="list">
                  {items.map((p) => (
                    <PageRow key={p.id} p={p} onChanged={reload} onShare={status.account.allowed ? setSharing : () => {}} />
                  ))}
                </ul>
              )}
            </Card>
            {status.account.allowed && <AddForm count={items.length} max={data.max} onAdded={reload} />}
            <Note kind="info" icon={Bot}>
              {t('pages.mcp_moved')} <Link to="/ai">{t('nav.ai')}</Link>
            </Note>
          </>
        )
      )}
      <p className="muted small">{t('pages.privacy')}</p>
      <ShareDialog page={sharing} onClose={() => setSharing(null)} />
    </>
  );
}
