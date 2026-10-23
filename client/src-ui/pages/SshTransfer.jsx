// SSH 主机从另一台电脑导入（docs/SSH_TRANSFER.md）。
// 新电脑：“从其他电脑导入”弹窗——选电脑 → 勾选主机、处理重名 → 显示 6 位确认码，等用户到旧电脑上输入 → 导入并逐台测试连接。
// 旧电脑：顶部横幅提示有待确认的导出，确认框里列出来自哪台电脑、哪些主机、会带上什么，输入新电脑上显示的确认码才导出；
// 打开“允许远程确认”后远程会话也能确认，另要邮箱验证码。秘密只在两台电脑的看看之间传，页面只看到进度和结果。
import { ArrowDownToLine, Laptop, ShieldAlert, ShieldCheck } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api, localApi } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Button, Empty, ErrorNote, Field, FoldNote, Modal, Note, Skeleton, Switch, Time, useLoad, useToast } from '../shared/ui.jsx';
import { Seg } from './Settings.jsx';
import { TestResult } from './SshHosts.jsx';

/** 一台主机会带上什么（`carries` 见 ssh.rs 的 Brief）。agentKey：“系统密钥”主机选了带上旧电脑的默认私钥。 */
function carriesText(t, h, agentKey) {
  if (h.auth === 'agent') return agentKey ? t('sshx.carry_agent_key', { path: agentKey }) : t('sshx.carry_agent');
  switch (h.carries) {
    case 'password':
      return t('sshx.carry_password');
    case 'key':
      return t(h.passphrase ? 'sshx.carry_key_pass' : 'sshx.carry_key');
    case 'key_file':
      return t(h.passphrase ? 'sshx.carry_key_file_pass' : 'sshx.carry_key_file', { path: h.key_path });
    case 'key_missing':
      return t('sshx.carry_key_missing', { path: h.key_path });
    default:
      return t('sshx.carry_none');
  }
}

/** 选中的主机连同它们用到的跳板机（顺着往上找）。返回 [全部 id, 因为是跳板机才带上的 id]。 */
function withJumps(items, picked) {
  const all = new Set(picked);
  const extra = new Set();
  for (const id of picked) {
    let cur = items.find((h) => h.id === id)?.jump;
    for (let n = 0; cur && n < 8; n += 1) {
      if (!all.has(cur)) extra.add(cur);
      all.add(cur);
      cur = items.find((h) => h.id === cur)?.jump;
    }
  }
  return [items.filter((h) => all.has(h.id)).map((h) => h.id), extra];
}

/** 新电脑这边：从其他电脑导入。local：这台电脑已有的主机（判断重名）。 */
export function ImportDialog({ local, onClose, onDone }) {
  const { t } = useTranslation();
  const [from, setFrom] = useState(null);
  const [job, setJob] = useState(null);
  return (
    <Modal open wide title={t('sshx.import_title')} onClose={onClose}>
      {job ? (
        <ImportProgress id={job} onClose={onClose} onDone={onDone} />
      ) : from ? (
        <PickHosts from={from} local={local} onBack={() => setFrom(null)} onStarted={setJob} />
      ) : (
        <PickSource onPick={setFrom} />
      )}
    </Modal>
  );
}

function PickSource({ onPick }) {
  const { t } = useTranslation();
  const [data, error] = useLoad(() => api.get('/ssh/import/sources'));
  const items = data?.items ?? [];
  return (
    <div className="sshx">
      <p className="muted">{t('sshx.source_intro')}</p>
      <ErrorNote error={error} />
      {!data && !error ? (
        <Skeleton height={80} />
      ) : (
        data &&
        (items.length === 0 ? (
          <Empty icon={Laptop} title={t('sshx.no_sources')}>
            <p className="muted small">{t('sshx.no_sources_hint')}</p>
          </Empty>
        ) : (
          <div className="sshx-list" role="list">
            {items.map((d) => (
              <button key={d.id} type="button" role="listitem" className="sshx-source" disabled={!d.online} onClick={() => onPick(d)}>
                <Laptop size={18} aria-hidden="true" />
                <span className="grow ellipsis">{d.name || d.id}</span>
                <span className={`small ${d.online ? 'ok-text' : 'muted'}`}>{t(d.online ? 'sshx.online' : 'sshx.offline')}</span>
              </button>
            ))}
          </div>
        ))
      )}
      <Note icon={ShieldCheck}>{t('sshx.source_safety')}</Note>
    </div>
  );
}

function PickHosts({ from, local, onBack, onStarted }) {
  const { t } = useTranslation();
  const [data, error] = useLoad(() => api.post('/ssh/import/hosts', { from: from.id }), [from.id]);
  const items = useMemo(() => data?.items ?? [], [data]);
  const [picked, setPicked] = useState(() => new Set());
  const [agentKeys, setAgentKeys] = useState(() => new Set());
  const [resolve, setResolve] = useState({});
  const [busy, setBusy] = useState(false);
  const [startError, setStartError] = useState(null);
  const [ids, viaJump] = useMemo(() => withJumps(items, [...picked]), [items, picked]);
  const clash = (h) => local.some((x) => x.name.toLowerCase() === h.name.toLowerCase());
  const toggle = (set, id) => set((s) => {
    const n = new Set(s);
    if (n.has(id)) n.delete(id);
    else n.add(id);
    return n;
  });
  const action = (h) => resolve[h.id] ?? { action: 'skip' };
  const renameBad = (h) => {
    const a = action(h);
    if (a.action !== 'rename') return false;
    const n = (a.name ?? '').trim().toLowerCase();
    return !n || local.some((x) => x.name.toLowerCase() === n) || items.some((x) => x.id !== h.id && ids.includes(x.id) && x.name.toLowerCase() === n);
  };
  const chosen = items.filter((h) => ids.includes(h.id));
  const invalid = chosen.some(renameBad);
  const start = async () => {
    setBusy(true);
    setStartError(null);
    try {
      const res = Object.fromEntries(chosen.filter(clash).map((h) => [h.id, action(h)]));
      const r = await api.post('/ssh/import/start', {
        from: from.id,
        hosts: ids,
        agent_keys: [...agentKeys].filter((id) => ids.includes(id) && data?.agent_key),
        resolve: res,
        names: chosen.map((h) => h.name),
      });
      onStarted(r.id);
    } catch (e) {
      setStartError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="sshx">
      <p className="muted">{t('sshx.hosts_intro', { name: from.name || from.id })}</p>
      <ErrorNote error={error} />
      {!data && !error && <Skeleton height={120} />}
      {data && items.length === 0 && <p className="muted">{t('sshx.no_hosts')}</p>}
      {items.length > 0 && (
        <div className="sshx-list" role="list">
          {items.map((h) => {
            const on = ids.includes(h.id);
            const jumpOnly = viaJump.has(h.id) && !picked.has(h.id);
            const ak = h.auth === 'agent' && data.agent_key && agentKeys.has(h.id) ? data.agent_key : null;
            return (
              <div key={h.id} role="listitem" className={`sshx-host ${on ? 'on' : ''}`}>
                <label className="row sshx-host-head">
                  <input type="checkbox" checked={on} disabled={jumpOnly} onChange={() => toggle(setPicked, h.id)} />
                  <span className="grow" style={{ minWidth: 0 }}>
                    <b className="ellipsis">{h.name}</b>
                    <span className="mono small muted ellipsis">
                      {' '}
                      {h.user}@{h.host}
                      {h.port !== 22 && `:${h.port}`}
                    </span>
                  </span>
                  {jumpOnly && <span className="badge">{t('sshx.as_jump')}</span>}
                </label>
                {on && (
                  <div className="sshx-host-body small">
                    <span className={h.carries === 'none' && h.auth !== 'agent' ? 'muted' : ''}>{carriesText(t, h, ak)}</span>
                    {h.auth === 'agent' && data.agent_key && (
                      <div className="row sshx-agent">
                        <Switch checked={agentKeys.has(h.id)} onChange={() => toggle(setAgentKeys, h.id)} label={t('sshx.agent_key_toggle', { path: data.agent_key })} />
                        <span>{t('sshx.agent_key_toggle', { path: data.agent_key })}</span>
                      </div>
                    )}
                    {clash(h) && (
                      <div className="sshx-clash">
                        <span className="warn-text">{t('sshx.clash', { name: h.name })}</span>
                        <Seg
                          value={action(h).action}
                          label={t('sshx.clash', { name: h.name })}
                          onChange={(v) => setResolve((r) => ({ ...r, [h.id]: v === 'rename' ? { action: 'rename', name: `${h.name}-2` } : { action: v } }))}
                          options={[
                            ['skip', t('sshx.clash_skip')],
                            ['overwrite', t('sshx.clash_overwrite')],
                            ['rename', t('sshx.clash_rename')],
                          ]}
                        />
                        {action(h).action === 'rename' && (
                          <Field label={t('sshx.new_name')} error={renameBad(h) ? t('sshx.new_name_bad') : ''}>
                            <input value={action(h).name ?? ''} maxLength={40} onChange={(e) => setResolve((r) => ({ ...r, [h.id]: { action: 'rename', name: e.target.value } }))} />
                          </Field>
                        )}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
      {chosen.length > 0 && <Note icon={ShieldCheck}>{t('sshx.after_import')}</Note>}
      <ErrorNote error={startError} />
      <div className="modal-actions">
        <Button variant="ghost" onClick={onBack}>
          {t('sshx.back')}
        </Button>
        <Button icon={ArrowDownToLine} busy={busy} disabled={chosen.length === 0 || invalid} onClick={start}>
          {t('sshx.import_n', { count: chosen.length })}
        </Button>
      </div>
    </div>
  );
}

const STATES = ['connecting', 'waiting', 'importing', 'testing', 'done'];

function ImportProgress({ id, onClose, onDone }) {
  const { t } = useTranslation();
  const [v, setV] = useState(null);
  const [error, setError] = useState(null);
  const finished = v && ['done', 'failed', 'canceled'].includes(v.state);
  useEffect(() => {
    if (finished) return undefined;
    let alive = true;
    const tick = () =>
      api
        .get(`/ssh/import/${id}`)
        .then((r) => alive && setV(r))
        .catch((e) => alive && setError(e));
    tick();
    const timer = setInterval(tick, 1000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [id, finished]);
  const imported = v?.state === 'testing' || v?.state === 'done';
  useEffect(() => {
    if (imported) onDone?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imported]);
  const cancel = () => api.post(`/ssh/import/${id}/cancel`).catch(() => {});
  if (error) return <ErrorNote error={error} />;
  if (!v) return <Skeleton height={120} />;
  const step = STATES.indexOf(v.state);
  return (
    <div className="sshx">
      <ol className="sshx-steps">
        {STATES.slice(0, 4).map((s, i) => (
          <li key={s} className={i < step || v.state === 'done' ? 'done' : i === step ? 'now' : ''}>
            {t(`sshx.state_${s}`)}
          </li>
        ))}
      </ol>
      {v.state === 'waiting' && v.code && (
        <div className="sshx-code-box" role="status">
          <p>{t('sshx.code_intro', { name: v.from_name || v.from })}</p>
          <div className="sshx-code mono" aria-label={t('sshx.code_label')}>
            {v.code.slice(0, 3)} {v.code.slice(3)}
          </div>
          <p className="small muted">{t('sshx.code_hint', { minutes: Math.max(1, Math.ceil(v.expires_in / 60)) })}</p>
        </div>
      )}
      {v.state === 'failed' && <ErrorNote error={v.error} />}
      {v.state === 'canceled' && <Note>{t('sshx.canceled')}</Note>}
      {imported && (
        <div className="sshx-results">
          {v.imported.map((h) => (
            <div key={h.from} className="sshx-result">
              <div className="row">
                <b className="grow ellipsis">{h.name}</b>
                <span className="badge">{t(`sshx.action_${h.action}`)}</span>
              </div>
              {h.action !== 'skipped' && (v.tests[h.id] ? <TestResult result={v.tests[h.id].error ? undefined : v.tests[h.id]} error={v.tests[h.id].error} /> : <p className="small muted">{t('sshx.testing')}</p>)}
            </div>
          ))}
          <p className="small muted">{t('sshx.ai_off')}</p>
        </div>
      )}
      <div className="modal-actions">
        {finished ? (
          <Button onClick={onClose}>{t('action.close')}</Button>
        ) : (
          v.state !== 'testing' && (
            <Button variant="ghost" onClick={cancel} disabled={v.state === 'importing'}>
              {t('action.cancel')}
            </Button>
          )
        )}
      </div>
    </div>
  );
}

// ---------------- 旧电脑这边：确认导出 ----------------

const DISMISS_KEY = 'looklook.sshx-seen';

/** 顶部横幅：有等这台电脑确认的导出时提示，点开确认框；刚导出成功时再提醒一次（不是本人操作的话马上去改密码）。
 *  问的是这台电脑自己（localApi），多台电脑时也不随顶栏选中的电脑变。 */
export function TransferBanner() {
  const { t } = useTranslation();
  const [data, setData] = useState(null);
  const [open, setOpen] = useState(null);
  const [seen, setSeen] = useState(() => {
    try {
      return localStorage.getItem(DISMISS_KEY) ?? '';
    } catch {
      return '';
    }
  });
  const load = useCallback(() => localApi.get('/ssh/transfer/pending').then(setData, () => setData(null)), []);
  useEffect(() => {
    load();
    // 打不开（远程会话、没开远程确认）也隔一会儿再看看，设置可能刚改
    const timer = setInterval(load, data === null ? 30000 : 4000);
    return () => clearInterval(timer);
  }, [load, data === null]); // eslint-disable-line react-hooks/exhaustive-deps
  const items = data?.items ?? [];
  const last = data?.last_export;
  const recent = last && last.at !== seen && Date.now() - Date.parse(last.at) < 3600_000;
  const dismiss = () => {
    setSeen(last.at);
    try {
      localStorage.setItem(DISMISS_KEY, last.at);
    } catch {
      /* 存不了就只在这次打开里不再提示 */
    }
  };
  const current = items.find((x) => x.id === open);
  return (
    <>
      {items.length > 0 && (
        <div className="banner banner-warn" role="alert">
          <ShieldAlert size={18} aria-hidden="true" />
          <span className="grow">{t('sshx.banner', { name: items[0].from_name || items[0].from, count: items[0].hosts.length })}</span>
          <Button onClick={() => setOpen(items[0].id)}>{t('sshx.review')}</Button>
        </div>
      )}
      {items.length === 0 && recent && (
        <div className="banner banner-info" role="status">
          <ShieldCheck size={18} aria-hidden="true" />
          <span className="grow">{t('sshx.exported', { name: last.peer_name, count: last.hosts.length, hosts: last.hosts.join('、') })}</span>
          <Button variant="ghost" onClick={dismiss}>
            {t('sshx.got_it')}
          </Button>
        </div>
      )}
      {current && (
        <ApproveDialog
          x={current}
          remote={data.remote}
          onClose={() => setOpen(null)}
          onDone={() => {
            setOpen(null);
            load();
          }}
        />
      )}
    </>
  );
}

function ApproveDialog({ x, remote, onClose, onDone }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [code, setCode] = useState('');
  const [email, setEmail] = useState('');
  const [sent, setSent] = useState(false);
  const [busy, setBusy] = useState(null);
  const [error, setError] = useState(null);
  const input = useRef(null);
  useEffect(() => input.current?.focus(), []);
  const digits = code.replace(/\D/g, '');
  const approve = async (e) => {
    e.preventDefault();
    setBusy('approve');
    setError(null);
    try {
      await localApi.post(`/ssh/transfer/${x.id}/approve`, { code: digits, email_code: remote ? email : undefined });
      toast(t('sshx.approved', { name: x.from_name || x.from }));
      onDone();
    } catch (err) {
      setError(err);
      if (['SSH_TRANSFER_CODE_LOCKED', 'SSH_TRANSFER_EXPIRED'].includes(err.code)) setTimeout(onDone, 2500);
    } finally {
      setBusy(null);
    }
  };
  const reject = async () => {
    setBusy('reject');
    try {
      await localApi.post(`/ssh/transfer/${x.id}/reject`);
      onDone();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(null);
    }
  };
  const sendEmail = async () => {
    setBusy('email');
    try {
      await localApi.post(`/ssh/transfer/${x.id}/email-code`);
      setSent(true);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(null);
    }
  };
  return (
    <Modal open title={t('sshx.approve_title')} onClose={onClose}>
      <form className="sshx" onSubmit={approve}>
        <p>{t('sshx.approve_intro', { name: x.from_name || x.from, count: x.hosts.length })}</p>
        <ul className="sshx-approve-list">
          {x.hosts.map((h) => (
            <li key={h.id}>
              <b>{h.name}</b> <span className="mono small muted">{h.target}</span>
              <div className="small">{carriesText(t, h, h.agent_key ? t('sshx.default_key') : null)}</div>
            </li>
          ))}
        </ul>
        <p className="small muted">
          {t('sshx.started')} <Time value={x.started_at} /> · {t('sshx.expires_in', { minutes: Math.max(1, Math.ceil(x.expires_in / 60)) })}
        </p>
        <Note kind="warn" icon={ShieldAlert}>
          {t('sshx.approve_warn')}
        </Note>
        {remote && <Note kind="warn">{t('sshx.remote_warn')}</Note>}
        <Field label={t('sshx.code_input')} hint={t('sshx.code_input_hint', { n: x.tries_left })}>
          <input ref={input} className="mono sshx-code-input" inputMode="numeric" autoComplete="one-time-code" maxLength={7} value={code} onChange={(e) => setCode(e.target.value)} placeholder="000 000" />
        </Field>
        {remote && (
          <Field label={t('sshx.email_code')} hint={t(sent ? 'sshx.email_sent' : 'sshx.email_hint')}>
            <div className="row">
              <input className="mono grow" inputMode="numeric" maxLength={6} value={email} onChange={(e) => setEmail(e.target.value)} />
              <Button variant="secondary" busy={busy === 'email'} disabled={Boolean(busy)} onClick={sendEmail}>
                {t(sent ? 'sshx.email_resend' : 'sshx.email_send')}
              </Button>
            </div>
          </Field>
        )}
        {error && <p className="small ssh-ki-error" role="alert">{errorText(t, error)}</p>}
        <div className="modal-actions">
          <Button variant="ghost" busy={busy === 'reject'} disabled={Boolean(busy)} onClick={reject}>
            {t('sshx.reject')}
          </Button>
          <Button type="submit" variant="danger" busy={busy === 'approve'} disabled={Boolean(busy) || digits.length !== 6 || (remote && email.trim().length < 6)}>
            {t('sshx.approve')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/** SSH 主机页底部：导入 / 导出记录，以及“允许远程确认 SSH 导出”（只能在这台电脑的本机界面打开）。 */
export function TransferPanel({ local }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [log, logError, reloadLog] = useLoad(() => api.get('/ssh/transfer/log'));
  const [cfg, , reloadCfg] = useLoad(() => api.get('/ssh/transfer/settings'));
  const [busy, setBusy] = useState(false);
  const items = log?.items ?? [];
  const setRemote = async (on) => {
    setBusy(true);
    try {
      await api.put('/ssh/transfer/settings', { allow_remote_approve: on });
      reloadCfg();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <FoldNote icon={ArrowDownToLine} summary={t('sshx.panel_title')}>
      <div className="sshx-panel">
        <div className="row sshx-remote">
          <span className="grow">
            {t('sshx.allow_remote')}
            <span className="small muted block">{t(local ? 'sshx.allow_remote_hint' : 'sshx.allow_remote_local_only')}</span>
          </span>
          <Switch checked={Boolean(cfg?.allow_remote_approve)} disabled={!local || busy || !cfg} onChange={setRemote} label={t('sshx.allow_remote')} />
        </div>
        <b className="small">{t('sshx.log_title')}</b>
        <ErrorNote error={logError} />
        {items.length === 0 ? (
          <p className="small muted">{t('sshx.log_empty')}</p>
        ) : (
          <ul className="sshx-log">
            {items.map((r, i) => (
              <li key={`${r.at}-${i}`} className="small">
                <Time value={r.at} /> · {t(`sshx.log_${r.dir}`, { name: r.peer_name || r.peer })} · <span className={r.result === 'ok' ? 'ok-text' : 'warn-text'}>{t(`sshx.result_${r.result}`)}</span>
                {r.hosts.length > 0 && <span className="muted"> · {r.hosts.join('、')}</span>}
              </li>
            ))}
          </ul>
        )}
        <button type="button" className="link-btn small" onClick={reloadLog}>
          {t('sshx.log_refresh')}
        </button>
      </div>
    </FoldNote>
  );
}
