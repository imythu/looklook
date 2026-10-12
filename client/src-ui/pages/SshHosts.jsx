// SSH 主机：保存常用的服务器，一键打开 SSH 终端；密码 / 私钥口令只在这台电脑上加密保存，连接时由看看交给 ssh，
// 不显示、不进终端输出。标了“允许 AI 使用”的主机，AI 助手可以经 MCP（本机网页 → AI 助手）运行命令，同样拿不到密码。
import { Bot, KeyRound, LockKeyhole, Pencil, Play, Plus, PlugZap, Server, ShieldCheck, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link, useRouter } from '../shared/router.jsx';
import { Button, Card, Empty, ErrorNote, Field, FoldNote, Modal, Note, PasswordInput, Skeleton, Switch, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { Seg } from './Settings.jsx';

const EMPTY = { name: '', host: '', port: 22, user: '', auth: 'agent', key_path: '~/.ssh/id_ed25519', allow_ai: false };

function HostDialog({ initial, onClose, onSaved }) {
  const { t } = useTranslation();
  const editing = Boolean(initial?.id);
  const [f, setF] = useState(() => ({ ...EMPTY, ...(initial ?? {}), key_path: initial?.key_path || EMPTY.key_path }));
  const [secret, setSecret] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }));
  const submit = async (e) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body = { ...f, port: Number(f.port) || 22, secret: secret || undefined };
      const r = editing ? await api.put(`/ssh/hosts/${initial.id}`, body) : await api.post('/ssh/hosts', body);
      onSaved(r);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const secretLabel = f.auth === 'password' ? t('ssh.password') : t('ssh.passphrase');
  return (
    <Modal open onClose={onClose} title={editing ? t('ssh.edit_title', { name: initial.name }) : t('ssh.add_title')} wide>
      <form onSubmit={submit}>
        <Field label={t('ssh.name')} hint={t('ssh.name_hint')}>
          <input value={f.name} onChange={(e) => set('name')(e.target.value)} maxLength={40} required placeholder={t('ssh.name_ph')} autoFocus />
        </Field>
        <div className="ssh-grid">
          <Field label={t('ssh.host')}>
            <input value={f.host} onChange={(e) => set('host')(e.target.value)} required placeholder="203.0.113.10 / db.example.com" spellCheck={false} autoCapitalize="none" />
          </Field>
          <Field label={t('ssh.port')}>
            <input type="number" min={1} max={65535} value={f.port} onChange={(e) => set('port')(e.target.value)} />
          </Field>
          <Field label={t('ssh.user')}>
            <input value={f.user} onChange={(e) => set('user')(e.target.value)} required placeholder="root" spellCheck={false} autoCapitalize="none" />
          </Field>
        </div>
        <Field label={t('ssh.auth')} hint={t(`ssh.auth_hint.${f.auth}`)}>
          <Seg
            value={f.auth}
            label={t('ssh.auth')}
            onChange={set('auth')}
            options={[
              ['agent', t('ssh.auth_agent')],
              ['key', t('ssh.auth_key')],
              ['password', t('ssh.auth_password')],
            ]}
          />
        </Field>
        {f.auth === 'key' && (
          <Field label={t('ssh.key_path')} hint={t('ssh.key_path_hint')}>
            <input value={f.key_path} onChange={(e) => set('key_path')(e.target.value)} spellCheck={false} autoCapitalize="none" className="mono" />
          </Field>
        )}
        {f.auth !== 'agent' && (
          <Field label={f.auth === 'key' ? `${secretLabel} ${t('ssh.optional')}` : secretLabel} hint={initial?.has_secret && initial.auth === f.auth ? t('ssh.secret_saved') : t('ssh.secret_hint')}>
            <PasswordInput value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="new-password" placeholder={initial?.has_secret && initial.auth === f.auth ? '••••••••' : ''} required={f.auth === 'password' && !(initial?.has_secret && initial.auth === 'password')} />
          </Field>
        )}
        <div className="ssh-ai">
          <div className="grow">
            <b>
              <Bot size={16} aria-hidden="true" /> {t('ssh.allow_ai')}
            </b>
            <p className="muted small">{t('ssh.allow_ai_hint')}</p>
          </div>
          <Switch checked={f.allow_ai} onChange={set('allow_ai')} label={t('ssh.allow_ai')} />
        </div>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button type="button" variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy}>
            {t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

function HostCard({ h, onEdit, onChanged }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const toast = useToast();
  const confirm = useConfirm();
  const [busy, setBusy] = useState(null);
  const [test, setTest] = useState(null);
  const open = async () => {
    setBusy('open');
    try {
      const inst = await api.post(`/ssh/hosts/${h.id}/open`);
      navigate(`/t/${inst.row?.id ?? inst.id}`);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  const doTest = async () => {
    setBusy('test');
    setTest(null);
    try {
      setTest(await api.post(`/ssh/hosts/${h.id}/test`));
    } catch (e) {
      setTest({ ok: false, error: e });
    } finally {
      setBusy(null);
    }
  };
  const remove = async () => {
    if (!(await confirm({ title: t('ssh.delete_title', { name: h.name }), body: t('ssh.delete_body'), confirm: t('ssh.delete'), danger: true }))) return;
    try {
      await api.del(`/ssh/hosts/${h.id}`);
      onChanged();
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  const AuthIcon = h.auth === 'password' ? LockKeyhole : KeyRound;
  return (
    <div className="card ssh-card">
      <div className="ssh-card-head">
        <div className="tile-icon">
          <Server size={20} aria-hidden="true" />
        </div>
        <div className="grow" style={{ minWidth: 0 }}>
          <h3 className="ellipsis">{h.name}</h3>
          <p className="mono small muted ellipsis">
            {h.user}@{h.host}
            {h.port !== 22 && `:${h.port}`}
          </p>
        </div>
        <span className="ssh-card-tools">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => onEdit(h)} aria-label={`${t('ssh.edit')} ${h.name}`} title={t('ssh.edit')}>
            <Pencil size={16} />
          </button>
          <button type="button" className="btn btn-plain btn-icon" onClick={remove} aria-label={`${t('ssh.delete')} ${h.name}`} title={t('ssh.delete')}>
            <Trash2 size={16} />
          </button>
        </span>
      </div>
      <div className="ssh-tags">
        <span className="badge">
          <AuthIcon size={12} aria-hidden="true" /> {t(`ssh.auth_${h.auth}`)}
          {h.auth !== 'agent' && h.has_secret && ` · ${t('ssh.saved')}`}
        </span>
        {h.allow_ai && (
          <span className="badge badge-ok" title={t('ssh.allow_ai_hint')}>
            <Bot size={12} aria-hidden="true" /> {t('ssh.ai_on')}
          </span>
        )}
      </div>
      {test && (
        <div className={`ssh-test ${test.ok ? 'ok' : 'bad'}`}>
          {test.ok ? (
            <>{t('ssh.test_ok')} <span className="mono">{test.result?.stdout?.split('\n')[1] ?? ''}</span></>
          ) : (
            <pre>{test.error ? errorText(t, test.error) : test.result?.timed_out ? t('ssh.test_timeout') : (test.result?.stderr || test.result?.stdout || '').trim().split('\n').slice(-4).join('\n') || t('ssh.test_failed')}</pre>
          )}
        </div>
      )}
      <div className="ssh-actions">
        <Button icon={Play} busy={busy === 'open'} disabled={!status.account.allowed || Boolean(busy)} onClick={open}>
          {t('ssh.connect')}
        </Button>
        <Button variant="ghost" icon={PlugZap} busy={busy === 'test'} disabled={Boolean(busy)} onClick={doTest}>
          {t('ssh.test')}
        </Button>
      </div>
    </div>
  );
}

export default function SshHosts() {
  const { t } = useTranslation();
  const [data, error, reload] = useLoad(() => api.get('/ssh/hosts'));
  const [dialog, setDialog] = useState(null);
  const items = data?.items ?? [];
  return (
    <>
      <div className="page-head">
        <h1>{t('ssh.title')}</h1>
        {items.length > 0 && (
          <Button icon={Plus} onClick={() => setDialog({})}>
            {t('ssh.add')}
          </Button>
        )}
      </div>
      <ErrorNote error={error} />
      {data && !data.ssh && <Note kind="warn">{t('ssh.no_ssh')}</Note>}
      {!data && !error ? (
        <div className="instances">
          <Skeleton height={150} />
          <Skeleton height={150} />
        </div>
      ) : items.length === 0 ? (
        <Card>
          <Empty icon={Server} title={t('ssh.empty_title')}>
            <p className="muted">{t('ssh.empty_body')}</p>
            <Button icon={Plus} onClick={() => setDialog({})}>
              {t('ssh.add')}
            </Button>
          </Empty>
        </Card>
      ) : (
        <div className="instances">
          {items.map((h) => (
            <HostCard key={h.id} h={h} onEdit={setDialog} onChanged={reload} />
          ))}
        </div>
      )}
      <FoldNote icon={ShieldCheck} summary={t('ssh.safety_title')}>
        <ul className="ssh-safety">
          <li>{t('ssh.safety_1')}</li>
          <li>{t('ssh.safety_2')}</li>
          <li>
            {t('ssh.safety_3')} <Link to="/pages">{t('nav.pages')}</Link>
          </li>
          <li>{t('ssh.safety_4')}</li>
        </ul>
      </FoldNote>
      {dialog && (
        <HostDialog
          initial={dialog.id ? dialog : null}
          onClose={() => setDialog(null)}
          onSaved={() => {
            setDialog(null);
            reload();
          }}
        />
      )}
    </>
  );
}
