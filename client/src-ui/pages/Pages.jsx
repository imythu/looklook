// 本机网页：把电脑上正在运行的网页（例如 localhost:5173）放到自己的专属地址下，在外面也能打开。
// 每个账户最多 10 个；只有本人登录后才能打开。数据登记在看看服务端。
import { ExternalLink, Globe, Plus, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import McpCard from './PagesMcp.jsx';
import { errorText } from '../shared/i18n.js';
import { Button, Card, CopyButton, Empty, ErrorNote, Field, Note, Skeleton, Switch, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

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

function PageRow({ p, onChanged }) {
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
                    <PageRow key={p.id} p={p} onChanged={reload} />
                  ))}
                </ul>
              )}
            </Card>
            {status.account.allowed && <AddForm count={items.length} max={data.max} onAdded={reload} />}
            {status.access === 'local' && <McpCard />}
          </>
        )
      )}
      <p className="muted small">{t('pages.privacy')}</p>
    </>
  );
}
