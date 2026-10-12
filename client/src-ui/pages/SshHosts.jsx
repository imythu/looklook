// SSH 主机：保存常用的服务器，一键打开 SSH 终端；密码 / 私钥口令只在这台电脑上加密保存，连接时由看看交给 ssh，
// 不显示、不进终端输出。私钥可以用这台电脑上的文件，也可以从浏览器上传（同样加密保存）；可以经跳板机（另一台保存的主机）连接，
// 添加主机时也能顺手新建跳板机。标了“允许 AI 使用”的主机，AI 助手可以经 MCP（本机网页 → AI 助手）运行命令，同样拿不到密码。
import { Bot, KeyRound, LockKeyhole, Pencil, Play, Plus, PlugZap, Server, ShieldCheck, Trash2, Upload, Waypoints } from 'lucide-react';
import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link, useRouter } from '../shared/router.jsx';
import { Button, Card, Empty, ErrorNote, Field, FoldNote, Modal, Note, PasswordInput, Skeleton, Switch, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { Seg } from './Settings.jsx';

const EMPTY = { name: '', host: '', port: 22, user: '', auth: 'agent', key_path: '~/.ssh/id_ed25519', key_stored: false, jump: '', allow_ai: false };
const EMPTY_JUMP = { name: '', host: '', port: 22, user: '', auth: 'agent', key_path: '~/.ssh/id_ed25519', key_stored: false };

/** 主机 id 顺着跳板机往上会不会走到 self（选了就成环）。 */
function reaches(hosts, from, self) {
  let cur = from;
  for (let n = 0; cur && n < 8; n += 1) {
    if (cur === self) return true;
    cur = hosts.find((h) => h.id === cur)?.jump;
  }
  return false;
}

/** 读浏览器里选的私钥文件（只读文本，私钥一般几 KB）。 */
function readKeyFile(file) {
  return new Promise((resolve, reject) => {
    if (file.size > 64 * 1024) return reject(new Error('too_big'));
    const r = new FileReader();
    r.onload = () => resolve(String(r.result ?? ''));
    r.onerror = () => reject(r.error);
    r.readAsText(file);
  });
}

/**
 * 登录方式：系统密钥 / 私钥 / 密码。私钥可以是这台电脑上的文件（只记路径；列出 ~/.ssh 里找到的私钥快捷选择），
 * 也可以从浏览器上传（加密存进看看的数据库）。主机和新建跳板机共用。
 * f / set：表单；secret、keyText：密码 / 口令、上传的私钥内容（只在要修改时有值）；saved：已保存的主机（编辑时）。
 */
function AuthFields({ f, set, secret, setSecret, keyText, setKeyText, saved, keys, idPrefix }) {
  const { t } = useTranslation();
  const file = useRef(null);
  const [fileName, setFileName] = useState('');
  const [fileError, setFileError] = useState('');
  const [paste, setPaste] = useState(false);
  const hasKey = Boolean(saved?.has_key && saved.auth === 'key' && saved.key_stored);
  const [replace, setReplace] = useState(!hasKey);
  const hasSecret = Boolean(saved?.has_secret && saved.auth === f.auth);
  const secretLabel = f.auth === 'password' ? t('ssh.password') : t('ssh.passphrase');
  const pick = async (e) => {
    const fl = e.target.files?.[0];
    e.target.value = '';
    if (!fl) return;
    setFileError('');
    try {
      const text = await readKeyFile(fl);
      if (!/^-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----/.test(text.trim())) {
        setFileError(t(text.includes('PuTTY-User-Key-File') ? 'error:rule.ppk' : 'error:rule.private_key'));
        return;
      }
      setFileName(fl.name);
      setKeyText(text);
    } catch {
      setFileError(t('ssh.key_read_failed'));
    }
  };
  return (
    <>
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
        <div className="ssh-key">
          <Seg
            value={f.key_stored ? 'stored' : 'path'}
            label={t('ssh.key_source')}
            onChange={(v) => set('key_stored')(v === 'stored')}
            options={[
              ['path', t('ssh.key_local')],
              ['stored', t('ssh.key_upload')],
            ]}
          />
          {!f.key_stored ? (
            <Field label={t('ssh.key_path')} hint={t('ssh.key_path_hint')}>
              <input id={`${idPrefix}key-path`} value={f.key_path} onChange={(e) => set('key_path')(e.target.value)} spellCheck={false} autoCapitalize="none" className="mono" />
              {keys?.length > 0 && (
                <div className="chips ssh-key-chips" role="group" aria-label={t('ssh.key_found')}>
                  <span className="small muted">{t('ssh.key_found')}</span>
                  {keys.map((k) => (
                    <button key={k} type="button" className="chip mono" aria-pressed={f.key_path === k} onClick={() => set('key_path')(k)}>
                      {k.replace(/^~\/\.ssh\//, '')}
                    </button>
                  ))}
                </div>
              )}
            </Field>
          ) : !replace ? (
            <div className="ssh-key-saved">
              <KeyRound size={16} aria-hidden="true" />
              <span className="grow">{t('ssh.key_saved')}</span>
              <Button variant="ghost" onClick={() => setReplace(true)}>
                {t('ssh.key_replace')}
              </Button>
            </div>
          ) : (
            <Field label={t('ssh.key_file')} hint={t('ssh.key_upload_hint')} error={fileError}>
              <div className="ssh-key-upload">
                <Button variant="secondary" icon={Upload} onClick={() => file.current?.click()}>
                  {t('ssh.key_choose')}
                </Button>
                <span className={`grow small ellipsis ${keyText ? '' : 'muted'}`}>
                  {keyText ? (fileName ? t('ssh.key_chosen', { name: fileName }) : t('ssh.key_pasted')) : t('ssh.key_none')}
                </span>
                {hasKey && (
                  <Button
                    variant="ghost"
                    onClick={() => {
                      setReplace(false);
                      setKeyText('');
                      setFileName('');
                    }}
                  >
                    {t('action.cancel')}
                  </Button>
                )}
              </div>
              <input ref={file} type="file" hidden onChange={pick} />
              {!paste ? (
                <button type="button" className="link-btn ssh-key-paste" onClick={() => setPaste(true)}>
                  {t('ssh.key_paste')}
                </button>
              ) : (
                <textarea
                  className="mono ssh-key-text"
                  value={fileName ? '' : keyText}
                  onChange={(e) => {
                    setFileName('');
                    setKeyText(e.target.value);
                  }}
                  placeholder="-----BEGIN OPENSSH …"
                  spellCheck={false}
                  autoCapitalize="none"
                  autoComplete="off"
                  aria-label={t('ssh.key_paste')}
                />
              )}
            </Field>
          )}
        </div>
      )}
      {f.auth !== 'agent' && (
        <Field label={f.auth === 'key' ? `${secretLabel} ${t('ssh.optional')}` : secretLabel} hint={hasSecret ? t('ssh.secret_saved') : t('ssh.secret_hint')}>
          <PasswordInput value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="new-password" placeholder={hasSecret ? '••••••••' : ''} required={f.auth === 'password' && !hasSecret} />
        </Field>
      )}
    </>
  );
}

function HostDialog({ initial, hosts, onClose, onSaved }) {
  const { t } = useTranslation();
  const toast = useToast();
  const editing = Boolean(initial?.id);
  const [f, setF] = useState(() => ({ ...EMPTY, ...(initial ?? {}), key_path: initial?.key_path || EMPTY.key_path }));
  const [secret, setSecret] = useState('');
  const [keyText, setKeyText] = useState('');
  // 跳板机：不用 / 选已有的主机 / 新建一台
  const [jumpMode, setJumpMode] = useState(initial?.jump ? 'existing' : 'none');
  const [j, setJ] = useState(EMPTY_JUMP);
  const [jSecret, setJSecret] = useState('');
  const [jKeyText, setJKeyText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const [keys] = useLoad(() => api.get('/ssh/keys').catch(() => ({ items: [] })));
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }));
  const setJump = (k) => (v) => setJ((x) => ({ ...x, [k]: v }));
  // 能当跳板机的：除了自己，以及会绕回自己的
  const candidates = hosts.filter((h) => h.id !== initial?.id && !reaches(hosts, h.id, initial?.id));
  const jumpId = jumpMode === 'existing' ? (candidates.some((h) => h.id === f.jump) ? f.jump : '') : '';
  const submit = async (e) => {
    e.preventDefault();
    if (jumpMode === 'existing' && !jumpId) {
      setError({ code: 'VALIDATION_FAILED', params: { field: 'jump', rule: 'required' } });
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const body = {
        ...f,
        port: Number(f.port) || 22,
        secret: secret || undefined,
        key_text: f.auth === 'key' && f.key_stored && keyText ? keyText : undefined,
        jump: jumpId,
        jump_new:
          jumpMode === 'new'
            ? { ...j, port: Number(j.port) || 22, secret: jSecret || undefined, key_text: j.auth === 'key' && j.key_stored && jKeyText ? jKeyText : undefined }
            : undefined,
      };
      const r = editing ? await api.put(`/ssh/hosts/${initial.id}`, body) : await api.post('/ssh/hosts', body);
      if (r.created_jump) toast(t('ssh.jump_created', { name: r.created_jump.name, host: r.name }));
      onSaved(r);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const jumpName = j.name.trim() || t('ssh.jump_new_unnamed');
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
        <AuthFields f={f} set={set} secret={secret} setSecret={setSecret} keyText={keyText} setKeyText={setKeyText} saved={initial} keys={keys?.items} idPrefix="h-" />
        <section className="ssh-jump" aria-labelledby="ssh-jump-title">
          <div className="ssh-jump-head">
            <Waypoints size={16} aria-hidden="true" />
            <b id="ssh-jump-title">{t('ssh.jump')}</b>
          </div>
          <p className="muted small ssh-jump-hint">{t('ssh.jump_hint')}</p>
          <Seg
            value={jumpMode}
            label={t('ssh.jump')}
            onChange={setJumpMode}
            options={[
              ['none', t('ssh.jump_none')],
              ['existing', t('ssh.jump_existing')],
              ['new', t('ssh.jump_new')],
            ]}
          />
          {jumpMode === 'existing' &&
            (candidates.length === 0 ? (
              <p className="small muted ssh-jump-body">{t('ssh.jump_no_hosts')}</p>
            ) : (
              <div className="ssh-jump-body">
                <select value={jumpId} onChange={(e) => set('jump')(e.target.value)} aria-label={t('ssh.jump_pick')} required>
                  <option value="" disabled>
                    {t('ssh.jump_pick')}
                  </option>
                  {candidates.map((h) => (
                    <option key={h.id} value={h.id}>
                      {h.name} · {h.user}@{h.host}
                      {h.port !== 22 ? `:${h.port}` : ''}
                    </option>
                  ))}
                </select>
              </div>
            ))}
          {jumpMode === 'new' && (
            <div className="ssh-jump-body ssh-jump-new">
              <Note kind="info">{t('ssh.jump_new_note', { name: jumpName })}</Note>
              <Field label={t('ssh.jump_name')}>
                <input value={j.name} onChange={(e) => setJump('name')(e.target.value)} maxLength={40} required placeholder={t('ssh.jump_name_ph')} />
              </Field>
              <div className="ssh-grid">
                <Field label={t('ssh.host')}>
                  <input value={j.host} onChange={(e) => setJump('host')(e.target.value)} required placeholder="bastion.example.com" spellCheck={false} autoCapitalize="none" />
                </Field>
                <Field label={t('ssh.port')}>
                  <input type="number" min={1} max={65535} value={j.port} onChange={(e) => setJump('port')(e.target.value)} />
                </Field>
                <Field label={t('ssh.user')}>
                  <input value={j.user} onChange={(e) => setJump('user')(e.target.value)} required placeholder="jump" spellCheck={false} autoCapitalize="none" />
                </Field>
              </div>
              <AuthFields f={j} set={setJump} secret={jSecret} setSecret={setJSecret} keyText={jKeyText} setKeyText={setJKeyText} keys={keys?.items} idPrefix="j-" />
            </div>
          )}
        </section>
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
            {jumpMode === 'new' ? t('ssh.save_both') : t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

function HostCard({ h, hosts, onEdit, onChanged }) {
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
          {h.auth === 'key' && h.key_stored && ` · ${t('ssh.key_uploaded')}`}
          {h.auth !== 'agent' && h.has_secret && ` · ${t('ssh.saved')}`}
        </span>
        {h.jump && (
          <span className="badge" title={t('ssh.jump_via_hint')}>
            <Waypoints size={12} aria-hidden="true" /> {t('ssh.jump_via', { name: hosts.find((x) => x.id === h.jump)?.name ?? '?' })}
          </span>
        )}
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
            <HostCard key={h.id} h={h} hosts={items} onEdit={setDialog} onChanged={reload} />
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
          hosts={items}
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
