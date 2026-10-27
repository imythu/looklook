// 设置页“通知”分区（docs/TASK_BOARD.md §6.1、§6.5）：
// - FeishuCard：没配时是一步步的向导（建应用 → 权限 → 填 App ID / Secret → 长连接和回调 → 发布 → 绑定），配好后显示连接状态、绑定、测试；
// - RulesCard：发哪些通知、能不能在飞书里处理、卡片里显示不显示内容、静音的项目。
// 配置经“设置同步”在用户自己的电脑之间传；连接状态是当前这台电脑的。
import { CheckCircle2, ExternalLink } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { useSessions } from '../shared/sessions.jsx';
import { Button, Card, CopyButton, ErrorNote, Field, Note, PasswordInput, Switch, useAction, useLoad } from '../shared/ui.jsx';

const CONSOLE = { feishu: 'https://open.feishu.cn/app', lark: 'https://open.larksuite.com/app' };
const SCOPES = ['im:message:send_as_bot', 'im:message.p2p_msg:readonly'];
const EVENT = 'im.message.receive_v1';
const CALLBACK = 'card.action.trigger';
// 常见错误码 → 怎么办（notify.feishu_hint_*）
const HINTS = [10003, 10014, 230013, 99991672, 1000040346, 1000040350];

function Code({ text }) {
  return (
    <span className="nt-code">
      <code>{text}</code>
      <CopyButton text={text} label={false} />
    </span>
  );
}

function FeishuError({ error }) {
  const { t } = useTranslation();
  if (!error) return null;
  const code = error.params?.code ?? error.code;
  if (error.code === 'NOTIFY_FEISHU' || typeof code === 'number') {
    const hint = HINTS.includes(code) ? t(`notify.feishu_hint_${code}`) : '';
    return (
      <Note kind="error">
        {t('notify.feishu_error', { code, msg: error.params?.msg ?? error.msg ?? '' })}
        {hint && <span style={{ display: 'block' }}>{hint}</span>}
      </Note>
    );
  }
  return <ErrorNote error={error} />;
}

/** 填 App ID / Secret（向导第 3 步，或者“修改”） */
function CredsForm({ data, onSaved, onCancel }) {
  const { t } = useTranslation();
  const f = data?.feishu;
  const [appId, setAppId] = useState(f?.app_id ?? '');
  const [secret, setSecret] = useState('');
  const [domain, setDomain] = useState(f?.domain ?? 'feishu');
  const { busy, error, run } = useAction();
  const save = async (e) => {
    e.preventDefault();
    const r = await run(() => api.put('/notify/feishu', { app_id: appId.trim(), app_secret: secret.trim() || undefined, domain }));
    if (r) onSaved(r);
  };
  return (
    <form className="stack" style={{ gap: 12 }} onSubmit={save}>
      <Field label={t('notify.domain')}>
        <select value={domain} onChange={(e) => setDomain(e.target.value)}>
          <option value="feishu">{t('notify.domain_feishu')}</option>
          <option value="lark">{t('notify.domain_lark')}</option>
        </select>
      </Field>
      <Field label="App ID">
        <input value={appId} onChange={(e) => setAppId(e.target.value)} placeholder="cli_…" autoComplete="off" spellCheck={false} />
      </Field>
      <Field label="App Secret" hint={f ? t('notify.secret_keep') : t('notify.secret_hint')}>
        <PasswordInput value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="off" spellCheck={false} />
      </Field>
      <FeishuError error={error} />
      <div className="nt-actions">
        <Button type="submit" busy={busy} disabled={!appId.trim() || (!f && !secret.trim())}>
          {t('notify.save_creds')}
        </Button>
        {onCancel && (
          <Button variant="ghost" type="button" onClick={onCancel}>
            {t('action.cancel')}
          </Button>
        )}
      </div>
    </form>
  );
}

function Step({ n, done, title, children }) {
  return (
    <li className={`nt-step${done ? ' done' : ''}`}>
      <span className="nt-step-n" aria-hidden="true">
        {done ? <CheckCircle2 size={18} /> : n}
      </span>
      <div className="nt-step-body">
        <b>{title}</b>
        {/* 做完的步骤只留标题 */}
        {!done && children}
      </div>
    </li>
  );
}

function PairCode({ pair }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const iv = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(iv);
  }, []);
  if (!pair) return null;
  const left = Math.max(0, Math.round((pair.expires_at - now) / 60000));
  return (
    <div className="nt-pair">
      <span className="nt-pair-code mono">{pair.code}</span>
      <CopyButton text={pair.code} />
      <span className="small muted">{t('notify.pair_left', { n: left })}</span>
    </div>
  );
}

export function FeishuCard({ data, reload }) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const { busy, error, run } = useAction();
  const [tested, setTested] = useState(false);
  const f = data?.feishu;
  const st = data?.status ?? {};
  const consoleUrl = CONSOLE[f?.domain ?? 'feishu'];

  // 等绑定、等回调验证时勾得快一点
  useEffect(() => {
    if (!f || (f.bound && f.callback_ok)) return undefined;
    const iv = setInterval(reload, 3000);
    return () => clearInterval(iv);
  }, [f?.bound, f?.callback_ok, Boolean(f)]);

  const saved = () => {
    setEditing(false);
    reload();
  };
  const test = async () => {
    if ((await run(() => api.post('/notify/test'))) !== undefined) setTested(true);
  };
  const unbind = async () => {
    if (await run(() => api.post('/notify/unbind'))) reload();
  };
  const disconnect = async () => {
    if (await run(() => api.delete('/notify/feishu'))) reload();
  };

  const steps = (
    <ol className="nt-steps">
      <Step n={1} done={Boolean(f)} title={t('notify.step_app')}>
        <p className="small muted">{t('notify.step_app_body')}</p>
        <a className="small" href={consoleUrl} target="_blank" rel="noopener noreferrer">
          {t('notify.open_console')} <ExternalLink size={12} aria-hidden="true" />
        </a>
        <p className="small muted">{t('notify.step_app_team')}</p>
      </Step>
      <Step n={2} done={Boolean(f)} title={t('notify.step_scopes')}>
        <p className="small muted">{t('notify.step_scopes_body')}</p>
        {SCOPES.map((s) => (
          <Code key={s} text={s} />
        ))}
      </Step>
      <Step n={3} done={Boolean(f)} title={t('notify.step_creds')}>
        <p className="small muted">{t('notify.step_creds_body')}</p>
        {!f && <CredsForm data={data} onSaved={saved} />}
      </Step>
      <Step n={4} done={Boolean(f?.callback_ok)} title={t('notify.step_events')}>
        <p className="small muted">{t('notify.step_events_body')}</p>
        <span className="small">{t('notify.step_events_event')}</span>
        <Code text={EVENT} />
        <span className="small">{t('notify.step_events_callback')}</span>
        <Code text={CALLBACK} />
        <Note kind="warn">{t('notify.step_events_warn')}</Note>
      </Step>
      <Step n={5} done={Boolean(f?.bound)} title={t('notify.step_publish')}>
        <p className="small muted">{t('notify.step_publish_body')}</p>
      </Step>
      <Step n={6} done={Boolean(f?.bound)} title={t('notify.step_bind')}>
        {f && !f.bound ? (
          <>
            <p className="small muted">{t('notify.step_bind_body')}</p>
            <PairCode pair={data.pair} />
          </>
        ) : (
          <p className="small muted">{t(f?.bound ? 'notify.bound' : 'notify.step_bind_later')}</p>
        )}
      </Step>
    </ol>
  );

  const connection = f && (
    <div className="stack" style={{ gap: 8 }}>
      <div className="between" style={{ gap: 12 }}>
        <span>
          <span className={`nt-dot ${st.connected ? 'ok' : 'bad'}`} aria-hidden="true" />
          {st.connected ? t('notify.connected') : t('notify.disconnected')}
          <span className="small muted"> · {f.app_id}</span>
        </span>
        <span className="nt-actions">
          <Button variant="ghost" className="btn-sm" onClick={() => setEditing(true)}>
            {t('notify.edit')}
          </Button>
          <Button variant="ghost" className="btn-sm danger-text" busy={busy} onClick={disconnect}>
            {t('notify.disconnect')}
          </Button>
        </span>
      </div>
      {st.error && <FeishuError error={{ code: 'NOTIFY_FEISHU', params: st.error }} />}
      {!data.sync_enabled && <Note kind="warn">{t('notify.sync_off')}</Note>}
      {editing && <CredsForm data={data} onSaved={saved} onCancel={() => setEditing(false)} />}
      {f.bound && (
        <div className="between" style={{ gap: 12 }}>
          <span className="small">
            {t('notify.bound')}
            {f.callback_ok ? <span className="muted"> · {t('notify.callback_ok')}</span> : <span className="muted"> · {t('notify.callback_unknown')}</span>}
          </span>
          <span className="nt-actions">
            <Button className="btn-sm" busy={busy} onClick={test}>
              {t('notify.test')}
            </Button>
            <Button variant="ghost" className="btn-sm" disabled={busy} onClick={unbind}>
              {t('notify.unbind')}
            </Button>
          </span>
        </div>
      )}
      {tested && !f.callback_ok && <Note>{t('notify.test_sent')}</Note>}
      <ErrorNote error={error && error.code !== 'NOTIFY_FEISHU' ? error : null} />
      {error?.code === 'NOTIFY_FEISHU' && <FeishuError error={error} />}
    </div>
  );

  return (
    <Card title={t('notify.feishu_title')}>
      <p className="small muted" style={{ marginTop: 0 }}>
        {t('notify.feishu_lead')}
      </p>
      {connection}
      {(!f || !f.bound || !f.callback_ok) && steps}
    </Card>
  );
}

export function RulesCard({ data, reload }) {
  const { t, i18n } = useTranslation();
  const sessions = useSessions();
  const { busy, error, run } = useAction();
  const r = data?.rules;
  const [adding, setAdding] = useState('');
  const lang = i18n.language?.startsWith('en') ? 'en-US' : 'zh-CN';
  const save = async (patch) => {
    if (await run(() => api.put('/notify/rules', { ...r, ...patch, lang }))) reload();
  };
  // 卡片跟着界面语言：换了语言后下次打开这里时顺手更新
  useEffect(() => {
    if (r && r.lang !== lang && data?.feishu) save({});
  }, [lang, Boolean(r)]);
  const dirs = useMemo(() => [...new Set((sessions.items ?? []).map((i) => i.workdir).filter(Boolean))].sort(), [sessions.items]);
  if (!r) return null;
  const toggle = (k, title, hint) => (
    <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
      <span>
        <b>{title}</b>
        {hint && (
          <span className="small muted" style={{ display: 'block' }}>
            {hint}
          </span>
        )}
      </span>
      <Switch checked={Boolean(r[k])} disabled={busy} onChange={(v) => save({ [k]: v })} label={title} />
    </div>
  );
  const name = (d) => d.split(/[\\/]/).filter(Boolean).pop() || d;
  const pick = (k, title, hint, options) => (
    <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
      <span>
        <b>{title}</b>
        <span className="small muted" style={{ display: 'block' }}>
          {hint}
        </span>
      </span>
      <select value={r[k] ?? 0} disabled={busy} onChange={(e) => save({ [k]: Number(e.target.value) })} aria-label={title} style={{ width: 'auto', minWidth: 120, flexShrink: 0 }}>
        {options.map(([v, label]) => (
          <option key={v} value={v}>
            {label}
          </option>
        ))}
      </select>
    </div>
  );
  const quiet = r.quiet;
  const setQuiet = (patch) => {
    const q = { start: '23:00', end: '08:00', ...quiet, ...patch };
    if (/^\d{2}:\d{2}$/.test(q.start) && /^\d{2}:\d{2}$/.test(q.end) && q.start !== q.end) save({ quiet: q });
  };
  return (
    <Card title={t('notify.rules_title')}>
      <div className="stack" style={{ gap: 14 }}>
        <p className="small muted" style={{ margin: 0 }}>
          {t('notify.rules_lead')}
        </p>
        {toggle('done', t('notify.rule_done'), t('notify.rule_done_hint'))}
        {toggle('failed', t('notify.rule_failed'))}
        {toggle('actions', t('notify.rule_actions'), t('notify.rule_actions_hint'))}
        {toggle('content', t('notify.rule_content'), t('notify.rule_content_hint'))}
        <div className="stack nt-calm" style={{ gap: 14 }}>
          <b className="nt-calm-title">{t('notify.calm_title')}</b>
          <div className="stack" style={{ gap: 8 }}>
            <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
              <span>
                <b>{t('notify.quiet')}</b>
                <span className="small muted" style={{ display: 'block' }}>
                  {t('notify.quiet_hint')}
                </span>
              </span>
              <Switch checked={Boolean(quiet)} disabled={busy} onChange={(v) => (v ? setQuiet({}) : save({ quiet: null }))} label={t('notify.quiet')} />
            </div>
            {quiet && (
              <div className="nt-actions nt-quiet">
                <input type="time" value={quiet.start} disabled={busy} aria-label={t('notify.quiet_start')} onChange={(e) => setQuiet({ start: e.target.value })} />
                <span className="muted">–</span>
                <input type="time" value={quiet.end} disabled={busy} aria-label={t('notify.quiet_end')} onChange={(e) => setQuiet({ end: e.target.value })} />
              </div>
            )}
          </div>
          {pick('done_batch', t('notify.batch'), t('notify.batch_hint'), [
            [0, t('notify.batch_off')],
            [10, t('notify.every_min', { n: 10 })],
            [30, t('notify.every_min', { n: 30 })],
            [60, t('notify.every_hour')],
          ])}
          {pick('stall_min', t('notify.stall'), t('notify.stall_hint'), [
            [0, t('notify.stall_off')],
            [10, t('notify.after_min', { n: 10 })],
            [20, t('notify.after_min', { n: 20 })],
            [30, t('notify.after_min', { n: 30 })],
            [60, t('notify.after_min', { n: 60 })],
          ])}
        </div>
        <div className="stack" style={{ gap: 8 }}>
          <b>{t('notify.muted')}</b>
          <span className="small muted">{t('notify.muted_hint')}</span>
          {r.muted.length > 0 && (
            <div className="nt-chips">
              {r.muted.map((m) => (
                <span key={m} className="nt-chip" title={m}>
                  {name(m)}
                  <button type="button" aria-label={t('notify.unmute', { name: name(m) })} disabled={busy} onClick={() => save({ muted: r.muted.filter((x) => x !== m) })}>
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
          <div className="nt-actions nt-mute-add">
            <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label={t('notify.muted')} style={{ minWidth: 0, flex: 1 }}>
              <option value="">{t('notify.mute_pick')}</option>
              {dirs
                .filter((d) => !r.muted.includes(d))
                .map((d) => (
                  <option key={d} value={d}>
                    {name(d)} · {d}
                  </option>
                ))}
            </select>
            <Button
              variant="ghost"
              disabled={!adding || busy}
              onClick={async () => {
                await save({ muted: [...r.muted, adding] });
                setAdding('');
              }}
            >
              {t('notify.mute')}
            </Button>
          </div>
        </div>
        <ErrorNote error={error} />
      </div>
    </Card>
  );
}

/** 设置页“通知”分区 */
export default function NotifySection() {
  const [data, error, reload] = useLoad(() => api.get('/notify'));
  return (
    <>
      <ErrorNote error={error} />
      <FeishuCard data={data} reload={reload} />
      {data?.feishu && <RulesCard data={data} reload={reload} />}
    </>
  );
}
