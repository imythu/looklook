// SSH 主机：保存常用的服务器，一键打开 SSH 终端；密码 / 私钥口令只在这台电脑上加密保存，连接时由看看交给 ssh，
// 不显示、不进终端输出。私钥可以用这台电脑上的文件（输入路径或在弹窗里点选），也可以从浏览器上传（同样加密保存）；
// 选好后显示脱敏的概况（算法、指纹、首尾几个字符），“系统密钥”也列出 ssh 会试的钥匙。可以经跳板机（另一台保存的主机）连接，
// 添加主机时也能顺手新建跳板机；目标机的密钥配在跳板机上的，登录方式选“跳板机代登”，这台不用填任何密码 / 私钥。标了“允许 AI 使用”的主机，AI 助手可以经 MCP（“AI 助手”页）运行命令，同样拿不到密码。
// 测试连接用 `ssh -v`：添加 / 修改时保存前就能测（新主机保存后也自动测一次），失败时说清卡在哪一步、为什么、怎么办，附完整日志。
import { ArrowDownToLine, Bot, CheckCircle2, FolderOpen, KeyRound, LockKeyhole, Pencil, Play, Plus, PlugZap, Server, ShieldCheck, Trash2, Upload, Waypoints } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link, useRouter } from '../shared/router.jsx';
import { Button, Card, CopyButton, Empty, ErrorNote, Field, FoldNote, Modal, Note, PasswordInput, Skeleton, Switch, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { HelpLink } from '../shared/help.jsx';
import { Seg } from './Settings.jsx';
import { ImportDialog, TransferPanel } from './SshTransfer.jsx';
import { FolderPicker } from './Terminals.jsx';

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

const ALGO = { 'ssh-ed25519': 'Ed25519', 'ssh-rsa': 'RSA', 'ssh-dss': 'DSA', 'sk-ssh-ed25519@openssh.com': 'Ed25519-SK', 'sk-ecdsa-sha2-nistp256@openssh.com': 'ECDSA-SK' };
const algoName = (a) => ALGO[a] ?? (a.startsWith('ecdsa') ? 'ECDSA' : a);

/** 私钥概况接口的错误：表单字段已经有标题，只说规则（“不存在”“不是 OpenSSH / PEM 格式的私钥”…）。 */
const keyError = (t, e) => (e?.code === 'VALIDATION_FAILED' && e.params?.rule ? t(`error:rule.${e.params.rule}`) : errorText(t, e));

/** 防抖地取私钥的脱敏概况：body 是 `{path}` / `{text}` / `{host}`，null 表示不取。 */
function useKeyInfo(body) {
  const key = body ? JSON.stringify(body) : '';
  const [state, setState] = useState({ key: '', info: null, error: null });
  useEffect(() => {
    if (!key) return undefined;
    let alive = true;
    const timer = setTimeout(() => {
      api
        .post('/ssh/keys/info', JSON.parse(key))
        .then((info) => alive && setState({ key, info, error: null }))
        .catch((error) => alive && setState({ key, info: null, error }));
    }, 300);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [key]);
  return state.key === key ? state : { info: null, error: null };
}

/** 一把钥匙的脱敏概况：算法、口令保护、指纹，内容只显示首尾几个字符。label：路径或 ssh-agent。 */
function KeyCard({ info, label }) {
  const { t } = useTranslation();
  const m = info.masked;
  const pub = info.format === 'agent';
  const dots = m.hidden > 0 && <span className="ssh-ki-dots">{t('ssh.ki_hidden', { n: m.hidden })}</span>;
  return (
    <div className="ssh-ki">
      <div className="ssh-ki-head">
        <KeyRound size={15} aria-hidden="true" />
        <b>{info.algo ? `${algoName(info.algo)}${info.bits ? ` ${info.bits}` : ''}` : t('ssh.ki_unknown')}</b>
        {!pub && <span className={`badge ${info.encrypted ? 'badge-ok' : ''}`}>{t(info.encrypted ? 'ssh.ki_encrypted' : 'ssh.ki_plain')}</span>}
        {info.comment && <span className="small muted ellipsis">{info.comment}</span>}
        {label && <span className="mono small muted ellipsis ssh-ki-label">{label}</span>}
      </div>
      <div className="mono small ssh-ki-fp">{info.fingerprint ?? t('ssh.ki_no_fp')}</div>
      <pre className="mono ssh-ki-masked" aria-label={t('ssh.ki_masked')}>
        {pub ? (
          <>
            {m.head} {m.start}
            {dots}
            {m.end}
          </>
        ) : (
          <>
            {m.head}
            {'\n'}
            {m.start}
            {dots}
            {m.end}
            {'\n'}
            {m.foot}
          </>
        )}
      </pre>
    </div>
  );
}

/** 选好 / 填好私钥后的概况；出错时显示原因（路径不对、不是私钥 …）。 */
function KeyPreview({ state }) {
  const { t } = useTranslation();
  if (state.error) return <p className="small ssh-ki-error">{keyError(t, state.error)}</p>;
  return state.info ? <KeyCard info={state.info} /> : null;
}

/** “系统密钥”登录时 ssh 会试的钥匙：ssh-agent 里的，以及 ~/.ssh 的默认私钥，脱敏显示。 */
function SystemKeys() {
  const { t } = useTranslation();
  const [data, error] = useLoad(() => api.get('/ssh/keys/system'));
  return (
    <div className="ssh-key ssh-sys">
      <p className="small muted">{t('ssh.sys_intro')}</p>
      <ErrorNote error={error} />
      {!data && !error ? (
        <Skeleton height={90} />
      ) : (
        data && (
          <>
            <div className="ssh-sys-group">
              <b className="small">{t('ssh.sys_agent')}</b>
              {data.agent === null ? (
                <p className="small muted">{t('ssh.sys_agent_off')}</p>
              ) : data.agent.length === 0 ? (
                <p className="small muted">{t('ssh.sys_agent_empty')}</p>
              ) : (
                data.agent.map((k) => <KeyCard key={k.fingerprint} info={k} />)
              )}
            </div>
            <div className="ssh-sys-group">
              <b className="small">{t('ssh.sys_files')}</b>
              {data.files.length === 0 ? <p className="small muted">{t('ssh.sys_files_empty')}</p> : data.files.map((k) => <KeyCard key={k.path} info={k} label={k.path} />)}
            </div>
          </>
        )
      )}
    </div>
  );
}

/**
 * 登录方式：系统密钥 / 私钥 / 密码；经跳板机时（viaJump）还有“跳板机代登”：在跳板机上 ssh 过去，用跳板机上配好的密钥。私钥可以是这台电脑上的文件（只记路径；列出 ~/.ssh 里找到的私钥快捷选择），
 * 也可以从浏览器上传（加密存进看看的数据库）。主机和新建跳板机共用。
 * f / set：表单；secret、keyText：密码 / 口令、上传的私钥内容（只在要修改时有值）；saved：已保存的主机（编辑时）。
 */
function AuthFields({ f, set, secret, setSecret, keyText, setKeyText, saved, keys, idPrefix, viaJump }) {
  const { t } = useTranslation();
  const file = useRef(null);
  const [fileName, setFileName] = useState('');
  const [fileError, setFileError] = useState('');
  const [paste, setPaste] = useState(false);
  const [browse, setBrowse] = useState(false);
  const hasKey = Boolean(saved?.has_key && saved.auth === 'key' && saved.key_stored);
  const [replace, setReplace] = useState(!hasKey);
  // 私钥的脱敏概况：本机文件按路径读；上传的看刚选 / 粘贴的内容，没换就看已保存的
  const keyAuth = f.auth === 'key';
  const preview = useKeyInfo(
    !keyAuth ? null : !f.key_stored ? (f.key_path.trim() ? { path: f.key_path.trim() } : null) : keyText.trim() ? { text: keyText } : hasKey && !replace ? { host: saved.id } : null,
  );
  // 粘贴的私钥认出来以后收起输入框，只留脱敏的概况
  const pastedOk = Boolean(paste && keyText && preview.info);
  useEffect(() => {
    if (pastedOk) setPaste(false);
  }, [pastedOk]);
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
            ...(viaJump ? [['jump', t('ssh.auth_jump')]] : []),
          ]}
        />
      </Field>
      {f.auth === 'agent' && <SystemKeys />}
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
              <div className="row ssh-key-path">
                <input id={`${idPrefix}key-path`} value={f.key_path} onChange={(e) => set('key_path')(e.target.value)} spellCheck={false} autoCapitalize="none" className="mono grow" />
                <Button variant="secondary" icon={FolderOpen} aria-haspopup="dialog" onClick={() => setBrowse(true)}>
                  {t('fs.browse')}
                </Button>
              </div>
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
              <KeyPreview state={preview} />
            </Field>
          ) : !replace ? (
            <div className="field">
              <div className="ssh-key-saved">
                <KeyRound size={16} aria-hidden="true" />
                <span className="grow">{t('ssh.key_saved')}</span>
                <Button variant="ghost" onClick={() => setReplace(true)}>
                  {t('ssh.key_replace')}
                </Button>
              </div>
              <KeyPreview state={preview} />
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
              {keyText.trim() && <KeyPreview state={preview} />}
              {!paste ? (
                <button type="button" className="link-btn ssh-key-paste" onClick={() => setPaste(true)}>
                  {keyText && !fileName ? t('ssh.key_paste_again') : t('ssh.key_paste')}
                </button>
              ) : (
                <textarea
                  className="mono ssh-key-text"
                  value={fileName ? '' : keyText}
                  autoFocus
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
          <FolderPicker
            open={browse}
            files
            initial={f.key_path.trim()}
            title={t('ssh.key_pick_title')}
            pickLabel={t('ssh.key_pick')}
            onClose={() => setBrowse(false)}
            onPick={set('key_path')}
          />
        </div>
      )}
      {f.auth !== 'agent' && f.auth !== 'jump' && (
        <Field label={f.auth === 'key' ? `${secretLabel} ${t('ssh.optional')}` : secretLabel} hint={hasSecret ? t('ssh.secret_saved') : t('ssh.secret_hint')}>
          <PasswordInput value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="new-password" placeholder={hasSecret ? '••••••••' : ''} required={f.auth === 'password' && !hasSecret} />
        </Field>
      )}
    </>
  );
}

const STAGES = ['connect', 'handshake', 'auth', 'command'];
/** 这些原因在 Windows 服务器上另有一条提示 */
const WIN_HINT = { auth_password: 'password', auth_key: 'key', auth_agent: 'key', no_keys: 'key', remote_cmd: 'shell', remote_output: 'shell' };

/** 测试连接的结果：成功时说连上了什么；失败时标出卡在哪一步，说明原因和办法，附完整日志。 */
export function TestResult({ result: r, error }) {
  const { t } = useTranslation();
  if (error) {
    return (
      <div className="ssh-test bad" role="alert">
        <b>{t('ssh.test_failed')}</b>
        <p>{errorText(t, error)}</p>
      </div>
    );
  }
  if (r.ok) {
    const parts = [r.server, r.method && t('ssh.test_method', { method: r.method }), r.via && t('ssh.jump_via', { name: r.via })].filter(Boolean);
    return (
      <div className="ssh-test ok" role="status">
        <CheckCircle2 size={16} aria-hidden="true" />
        <b>{t('ssh.test_ok')}</b>
        {parts.length > 0 && <span className="small">{parts.join(' · ')}</span>}
      </div>
    );
  }
  const failed = STAGES.indexOf(r.stage);
  const vars = { target: r.target, via: r.via ?? '', server: r.server ?? '', code: r.exit_code ?? '-', n: r.tried_keys };
  const win = r.windows && WIN_HINT[r.reason];
  return (
    <div className="ssh-test bad" role="alert">
      {failed >= 0 && (
        <ol className="ssh-steps" aria-label={t('ssh.test_steps')}>
          {STAGES.map((s, i) => (
            <li key={s} className={i < failed ? 'done' : i === failed ? 'fail' : ''}>
              {t(`ssh.stage.${s}`)}
            </li>
          ))}
        </ol>
      )}
      <b className="ssh-test-title">{t(`ssh.diag.${r.reason}.title`, vars)}</b>
      <p>{t(`ssh.diag.${r.reason}.hint`, vars)}</p>
      {r.via && ['closed', 'timeout', 'refused', 'unknown'].includes(r.reason) && <p>{t('ssh.diag_via', vars)}</p>}
      {win && <p>{t(`ssh.diag_win.${win}`)}</p>}
      {r.methods?.length > 0 && <p className="small">{t('ssh.test_methods', { list: r.methods.join(', ') })}</p>}
      {r.message && <pre>{r.message}</pre>}
      {r.log && (
        <details className="ssh-test-log">
          <summary>{t('ssh.test_log')}</summary>
          <pre>{r.log}</pre>
          <CopyButton text={r.log} />
        </details>
      )}
    </div>
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
  const [jumpMode, setJumpModeRaw] = useState(initial?.jump ? 'existing' : 'none');
  // 不经跳板机了，“跳板机代登”也就没法用，退回系统密钥
  const setJumpMode = (m) => {
    setJumpModeRaw(m);
    if (m === 'none') setF((x) => (x.auth === 'jump' ? { ...x, auth: 'agent' } : x));
  };
  const [j, setJ] = useState(EMPTY_JUMP);
  const [jSecret, setJSecret] = useState('');
  const [jKeyText, setJKeyText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  // 保存前的测试：结果对应的是哪一版表单（改了就不算测过）
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState(null);
  const formRef = useRef(null);
  const [keys] = useLoad(() => api.get('/ssh/keys').catch(() => ({ items: [] })));
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }));
  const setJump = (k) => (v) => setJ((x) => ({ ...x, [k]: v }));
  // 能当跳板机的：除了自己，以及会绕回自己的
  const candidates = hosts.filter((h) => h.id !== initial?.id && !reaches(hosts, h.id, initial?.id));
  const jumpId = jumpMode === 'existing' ? (candidates.some((h) => h.id === f.jump) ? f.jump : '') : '';
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
  const bodyKey = JSON.stringify(body);
  const testedOk = Boolean(test?.result?.ok && test.key === bodyKey);
  /** 表单本身的检查（必填、端口范围）和跳板机有没有选 */
  const check = () => {
    if (!formRef.current?.reportValidity()) return false;
    if (jumpMode === 'existing' && !jumpId) {
      setError({ code: 'VALIDATION_FAILED', params: { field: 'jump', rule: 'required' } });
      return false;
    }
    return true;
  };
  const runTest = async () => {
    if (!check()) return;
    setTesting(true);
    setError(null);
    setTest(null);
    try {
      setTest({ key: bodyKey, result: await api.post('/ssh/test', { id: initial?.id, host: body }) });
    } catch (err) {
      // 表单填错（字段校验）显示在表单下面；其他错误算测试失败
      if (err?.code === 'VALIDATION_FAILED' || err?.code === 'SSH_NAME_TAKEN') setError(err);
      else setTest({ key: bodyKey, error: err });
    } finally {
      setTesting(false);
    }
  };
  const submit = async (e) => {
    e.preventDefault();
    if (!check()) return;
    setBusy(true);
    setError(null);
    try {
      const r = editing ? await api.put(`/ssh/hosts/${initial.id}`, body) : await api.post('/ssh/hosts', body);
      if (r.created_jump) toast(t('ssh.jump_created', { name: r.created_jump.name, host: r.name }));
      onSaved(r, { tested: testedOk });
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const jumpName = j.name.trim() || t('ssh.jump_new_unnamed');
  return (
    <Modal open onClose={onClose} title={editing ? t('ssh.edit_title', { name: initial.name }) : t('ssh.add_title')} wide>
      <form onSubmit={submit} ref={formRef}>
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
        <AuthFields f={f} set={set} secret={secret} setSecret={setSecret} keyText={keyText} setKeyText={setKeyText} saved={initial} keys={keys?.items} idPrefix="h-" viaJump={jumpMode !== 'none'} />
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
          {jumpMode !== 'none' && f.auth !== 'jump' && (
            <p className="small muted ssh-jump-body">
              {t('ssh.jump_auth_tip')}{' '}
              <button type="button" className="link-btn" onClick={() => set('auth')('jump')}>
                {t('ssh.jump_auth_use')}
              </button>
            </p>
          )}
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
        {test && (
          <div className={test.key === bodyKey ? '' : 'ssh-test-stale'}>
            {test.key !== bodyKey && <p className="small muted">{t('ssh.test_stale')}</p>}
            <TestResult result={test.result} error={test.error} />
          </div>
        )}
        <div className="modal-actions">
          <Button variant="secondary" icon={PlugZap} busy={testing} disabled={busy} onClick={runTest} className="ssh-dialog-test">
            {t('ssh.test')}
          </Button>
          <Button type="button" variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={testing}>
            {jumpMode === 'new' ? t('ssh.save_both') : t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

function HostCard({ h, hosts, ai, autoTest, onEdit, onChanged }) {
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
      setTest({ result: await api.post(`/ssh/hosts/${h.id}/test`) });
    } catch (e) {
      setTest({ error: e });
    } finally {
      setBusy(null);
    }
  };
  // 刚添加、保存前没测过的主机：自动测一次
  const autoRan = useRef(false);
  useEffect(() => {
    if (autoTest && !autoRan.current) {
      autoRan.current = true;
      doTest();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoTest]);
  const [aiBusy, setAiBusy] = useState(false);
  const toggleAi = async (allow) => {
    setAiBusy(true);
    try {
      await api.put(`/ssh/hosts/${h.id}/ai`, { allow });
      onChanged();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setAiBusy(false);
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
  const AuthIcon = h.auth === 'password' ? LockKeyhole : h.auth === 'jump' ? Waypoints : KeyRound;
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
          {h.auth !== 'agent' && h.auth !== 'jump' && h.has_secret && ` · ${t('ssh.saved')}`}
        </span>
        {h.jump && (
          <span className="badge" title={t('ssh.jump_via_hint')}>
            <Waypoints size={12} aria-hidden="true" /> {t('ssh.jump_via', { name: hosts.find((x) => x.id === h.jump)?.name ?? '?' })}
          </span>
        )}
      </div>
      <div className="ssh-ai-quick">
        <Bot size={16} aria-hidden="true" />
        <span className="grow">
          {t('ssh.allow_ai')}
          {h.allow_ai && ai && (!ai.mcp || !ai.ssh) && (
            <Link to="/ai" className="small warn-text ssh-ai-off">
              {t(!ai.mcp ? 'ssh.ai_mcp_off' : 'ssh.ai_ssh_off')}
            </Link>
          )}
        </span>
        <Switch checked={h.allow_ai} disabled={aiBusy} onChange={toggleAi} label={`${t('ssh.allow_ai')} · ${h.name}`} />
      </div>
      {test && <TestResult result={test.result} error={test.error} />}
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
  const { status } = useStatus();
  const [data, error, reload] = useLoad(() => api.get('/ssh/hosts'));
  const [dialog, setDialog] = useState(null);
  const [importing, setImporting] = useState(false);
  // 刚添加、保存前没测通的主机：卡片出来后自动测试
  const [autoTest, setAutoTest] = useState(null);
  const items = data?.items ?? [];
  return (
    <>
      <div className="page-head">
        <h1 className="inline">
          {t('ssh.title')} <HelpLink page="ssh" />
        </h1>
        <span className="row">
          <Button variant="ghost" icon={ArrowDownToLine} onClick={() => setImporting(true)}>
            {t('sshx.import')}
          </Button>
          {items.length > 0 && (
            <Button icon={Plus} onClick={() => setDialog({})}>
              {t('ssh.add')}
            </Button>
          )}
        </span>
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
            <HostCard key={h.id} h={h} hosts={items} ai={data.ai} autoTest={autoTest === h.id} onEdit={setDialog} onChanged={reload} />
          ))}
        </div>
      )}
      <FoldNote icon={ShieldCheck} summary={t('ssh.safety_title')}>
        <ul className="ssh-safety">
          <li>{t('ssh.safety_1')}</li>
          <li>{t('ssh.safety_2')}</li>
          <li>
            {t('ssh.safety_3')} <Link to="/ai">{t('nav.ai')}</Link>
          </li>
          <li>{t('ssh.safety_4')}</li>
        </ul>
      </FoldNote>
      <TransferPanel local={status.access === 'local'} />
      {importing && <ImportDialog local={items} onClose={() => setImporting(false)} onDone={reload} />}
      {dialog && (
        <HostDialog
          initial={dialog.id ? dialog : null}
          hosts={items}
          onClose={() => setDialog(null)}
          onSaved={(r, { tested }) => {
            if (!dialog.id && !tested) setAutoTest(r.id);
            setDialog(null);
            reload();
          }}
        />
      )}
    </>
  );
}
