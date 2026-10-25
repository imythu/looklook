// 设置 → Claude Code 账号：本机默认账号、另外登录的官方账号、第三方接口（Anthropic 官方格式），选一个当默认。
// 新建的会话用默认的（开始页可以另选），之后切换默认不影响已经建好的会话。
// 官方账号的登录由 Claude Code 自己完成（后台运行 `claude auth login`，界面显示授权页地址、收授权码），看看不经手令牌。
import { Check, ExternalLink, Gauge, KeyRound, LogIn, Plus, Server, UserRound } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import PopMenu from '../shared/PopMenu.jsx';
import { useRouter } from '../shared/router.jsx';
import { formatTime } from '../shared/time.js';
import { Button, Card, CopyButton, ErrorNote, Field, FoldNote, Modal, Note, PasswordInput, Skeleton, Spinner, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { Disclaimer, ProviderLogo } from './ModelProviders.jsx';

const CUSTOM = 'custom';
const MODEL_KEYS = ['main', 'opus', 'sonnet', 'haiku'];

/** 配置显示的名字：本机默认账号没有名字 */
export function profileName(t, p) {
  return p.id === 'official' ? t('cprof.builtin') : p.name;
}

function KindIcon({ p, size = 15 }) {
  const Icon = p.kind === 'api' ? Server : UserRound;
  return <Icon size={size} aria-hidden="true" />;
}

/** 官方账号的登录状态：每个单独问（`claude auth status` 要一两秒）。`expired`：查用量时发现登录失效了 */
function LoginStatus({ s, err, expired }) {
  const { t } = useTranslation();
  if (err || s?.error) return <span className="muted">{t(s?.error === 'not_installed' ? 'cprof.not_installed' : 'cprof.status_failed')}</span>;
  if (!s) return <span className="muted">{t('cprof.checking')}</span>;
  if (!s.logged_in) return <span className="cprof-warn">{t(expired ? 'cprof.expired' : 'cprof.logged_out')}</span>;
  // 本机默认账号也可能是环境变量里的 Key（authMethod 不是 claude.ai）
  const who = s.email || (s.auth_method && s.auth_method !== 'claude.ai' ? t(`cprof.auth.${s.auth_method}`, { defaultValue: s.auth_method }) : '');
  return (
    <span>
      <Check size={14} style={{ verticalAlign: '-2px', color: 'var(--ok)' }} /> {who || t('cprof.logged_in')}
      {s.plan && <span className="muted"> · {String(s.plan).replace(/^\w/, (c) => c.toUpperCase())}</span>}
    </span>
  );
}

/** 查到的订阅用量：“5 小时 5% · 7 天 91%”，用得多的标黄，重置时间放在提示里 */
function UsageLine({ limits }) {
  const { t } = useTranslation();
  const wins = [
    ['usage_5h', limits?.five_hour],
    ['usage_7d', limits?.seven_day],
  ].filter(([, w]) => w);
  if (!wins.length) return <span className="muted">{t('cprof.usage_empty')}</span>;
  return wins.map(([key, w], i) => (
    <span key={key} className={w.used > 85 ? 'cprof-warn' : 'muted'} title={w.resets_at ? t('cprof.usage_reset', { at: formatTime(w.resets_at) }) : undefined}>
      {i > 0 && ' · '}
      {t(`cprof.${key}`, { used: Math.round(w.used) })}
    </span>
  ));
}

/**
 * 登录官方账号：电脑上在后台运行 Claude Code 自己的 `claude auth login`，这里只显示它给的授权页地址（不做成链接，
 * 旁边的按钮打开、复制），用户在 claude.com 登录后把授权码贴回来。
 */
function LoginModal({ profile, onClose }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [st, setSt] = useState(null);
  const [code, setCode] = useState('');
  const { busy, error, setError, run } = useAction();
  const start = () => {
    setSt(null);
    setCode('');
    run(async () => setSt(await api.post(`/claude-profiles/${profile.id}/login`, {})));
  };
  useEffect(start, [profile.id]);
  // 等授权页地址、等换令牌的结果（Claude Code 自己打开了本机浏览器时，不用贴授权码也会完成）
  useEffect(() => {
    if (st?.state !== 'running') return undefined;
    const timer = setTimeout(async () => {
      const r = await api.get(`/claude-profiles/${profile.id}/login`).catch(() => null);
      if (r) setSt(r);
    }, 1500);
    return () => clearTimeout(timer);
  }, [st]);
  useEffect(() => {
    if (st?.state !== 'ok') return;
    toast(t('cprof.login_done', { name: profileName(t, profile) }));
    onClose();
  }, [st?.state]);
  const close = () => {
    if (st?.state === 'running') api.del(`/claude-profiles/${profile.id}/login`).catch(() => {});
    onClose();
  };
  const submit = (e) => {
    e.preventDefault();
    setError(null);
    run(async () => setSt(await api.post(`/claude-profiles/${profile.id}/login/code`, { code: code.trim() })));
  };
  const waiting = st?.state === 'running' && st.code_sent;
  return (
    <Modal open onClose={close} title={t('cprof.login_title', { name: profileName(t, profile) })}>
      {!st && !error && <Skeleton height={80} />}
      {st?.state === 'running' && !st.url && (
        <p className="muted small">
          <Spinner size={14} /> {t('cprof.login_starting')}
        </p>
      )}
      {st?.url && st.state !== 'ok' && (
        <form onSubmit={submit}>
          <Field label={t('cprof.login_url')} hint={t('cprof.login_url_hint')}>
            <div className="cprof-url">
              <code className="mono">{st.url}</code>
              <span className="inline">
                <Button variant="secondary" icon={ExternalLink} onClick={() => window.open(st.url, '_blank', 'noopener')}>
                  {t('action.open')}
                </Button>
                <CopyButton text={st.url} variant="secondary" />
              </span>
            </div>
          </Field>
          <Field label={t('cprof.login_code')}>
            <input value={code} onChange={(e) => setCode(e.target.value)} autoComplete="off" spellCheck={false} autoCapitalize="none" disabled={st.state !== 'running' || waiting} autoFocus />
          </Field>
          {st.state === 'failed' && <Note kind="warn">{st.error ? t('cprof.login_failed_detail', { detail: st.error }) : t('cprof.login_failed')}</Note>}
          <ErrorNote error={error} />
          <div className="modal-actions">
            <Button variant="ghost" onClick={close}>
              {t('action.cancel')}
            </Button>
            {st.state === 'failed' ? (
              <Button onClick={start} busy={busy}>
                {t('cprof.login_retry')}
              </Button>
            ) : (
              <Button type="submit" busy={busy || waiting} disabled={!code.trim()}>
                {t('cprof.login_submit')}
              </Button>
            )}
          </div>
        </form>
      )}
      {(!st?.url || st.state === 'ok') && (
        <>
          {st?.state === 'failed' && <Note kind="warn">{st.error ? t('cprof.login_failed_detail', { detail: st.error }) : t('cprof.login_failed')}</Note>}
          <ErrorNote error={error} />
          <div className="modal-actions">
            <Button variant="ghost" onClick={close}>
              {t('action.cancel')}
            </Button>
            {(st?.state === 'failed' || error) && (
              <Button onClick={start} busy={busy}>
                {t('cprof.login_retry')}
              </Button>
            )}
          </div>
        </>
      )}
    </Modal>
  );
}

function OfficialForm({ initial, onClose, onSaved }) {
  const { t } = useTranslation();
  const [name, setName] = useState(initial?.name ?? '');
  const { busy, error, run } = useAction();
  const save = (e) => {
    e.preventDefault();
    run(async () => {
      const r = initial ? await api.put(`/claude-profiles/${initial.id}`, { name }) : await api.post('/claude-profiles', { kind: 'official', name });
      onSaved(r, !initial);
    });
  };
  return (
    <Modal open onClose={onClose} title={t(initial ? 'cprof.rename' : 'cprof.add_official')}>
      <form onSubmit={save}>
        {!initial && <p className="small muted">{t('cprof.official_intro')}</p>}
        <Field label={t('cprof.name')}>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={t('cprof.name_ph_official')} required autoFocus />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!name.trim()}>
            {t(initial ? 'action.save' : 'cprof.add_and_login')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/** 第三方接口：平台推荐的服务商（含推广）或自己填地址；Key 只存在这台电脑上 */
function ApiForm({ initial, accepted, onClose, onSaved }) {
  const { t, i18n } = useTranslation();
  const [data] = useLoad(() => api.get(`/model-providers?locale=${i18n.language}`).catch(() => ({ providers: [], error: 'x' })), [i18n.language]);
  const providers = (data?.providers ?? []).filter((p) => p.clients.some((c) => c.client === 'claude_code'));
  const [pick, setPick] = useState(initial ? (initial.provider_id ?? CUSTOM) : null);
  const [name, setName] = useState(initial?.name ?? '');
  const [baseUrl, setBaseUrl] = useState(initial && !initial.provider_id ? initial.base_url : '');
  const [models, setModels] = useState(() => Object.fromEntries(MODEL_KEYS.map((k) => [k, initial?.models?.[k] ?? ''])));
  const [key, setKey] = useState('');
  const [agree, setAgree] = useState(false);
  const { busy, error, run } = useAction();
  // 列表到了还没选：有推荐的选第一个，没有就自定义
  useEffect(() => {
    if (pick === null && data) setPick(providers[0]?.id ?? CUSTOM);
  }, [data]);
  const chosen = providers.find((p) => p.id === pick);
  const endpoint = chosen?.clients.find((c) => c.client === 'claude_code')?.base_url;
  const keepKey = Boolean(initial?.key_set);
  const urlChanged = !initial || pick !== (initial.provider_id ?? CUSTOM) || (pick === CUSTOM && baseUrl.trim() !== initial.base_url);
  const ready = pick !== null && (accepted || agree) && (pick !== CUSTOM || (name.trim() && baseUrl.trim())) && (keepKey || key.trim());
  const save = (e) => {
    e.preventDefault();
    run(async () => {
      const body = {
        kind: 'api',
        name: pick === CUSTOM || initial || name.trim() ? name : undefined,
        models,
        api_key: key.trim() || undefined,
        accept: !accepted && agree,
      };
      if (urlChanged) Object.assign(body, pick === CUSTOM ? { base_url: baseUrl } : { provider_id: pick });
      const r = initial ? await api.put(`/claude-profiles/${initial.id}`, body) : await api.post('/claude-profiles', body);
      onSaved(r, !initial);
    });
  };
  return (
    <Modal open onClose={onClose} title={t(initial ? 'cprof.edit_api' : 'cprof.add_api')} wide>
      <form onSubmit={save}>
        {accepted ? (
          <FoldNote kind="warn" summary={t('model.disclaimer.title')}>
            <Disclaimer />
          </FoldNote>
        ) : (
          <Note kind="warn">
            <b>{t('model.disclaimer.title')}</b>
            <Disclaimer />
            <label className="check-row" style={{ margin: 0 }}>
              <input type="checkbox" checked={agree} onChange={(e) => setAgree(e.target.checked)} />
              <span>{t('model.disclaimer.agree')}</span>
            </label>
          </Note>
        )}
        <p className="small muted">{t('cprof.api_intro')}</p>
        {!data && <Skeleton height={64} />}
        {data?.error && <Note kind="warn">{t('model.list_failed')}</Note>}
        {data && (
          <div className="model-grid" role="radiogroup" aria-label={t('model.provider')}>
            {providers.map((p) => (
              <button key={p.id} type="button" role="radio" aria-checked={pick === p.id} className="launch model-option" onClick={() => setPick(p.id)}>
                <span className="model-option-head">
                  <ProviderLogo p={p} />
                  <b className="grow ellipsis">{p.name}</b>
                  <span className="badge">{t('promotion.label')}</span>
                </span>
                {p.tagline && <span className="small muted">{p.tagline}</span>}
              </button>
            ))}
            <button type="button" role="radio" aria-checked={pick === CUSTOM} className="launch model-option" onClick={() => setPick(CUSTOM)}>
              <span className="model-option-head">
                <span className="model-logo">
                  <Server size={16} />
                </span>
                <b className="grow">{t('model.custom')}</b>
              </span>
              <span className="small muted">{t('model.custom_desc')}</span>
            </button>
          </div>
        )}
        {chosen && (
          <div className="model-detail">
            {chosen.description && <p className="small" style={{ whiteSpace: 'pre-wrap' }}>{chosen.description}</p>}
            <p className="small muted" style={{ margin: 0 }}>
              {t('model.endpoint')} <span className="mono">{endpoint}</span> ·{' '}
              <a href={chosen.homepage_url} target="_blank" rel="noopener noreferrer sponsored">
                {t('model.homepage')}
              </a>
            </p>
          </div>
        )}
        <Field label={t('cprof.name')} hint={pick !== CUSTOM && !initial ? t('cprof.name_hint_provider') : undefined}>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={chosen?.name ?? ''} required={pick === CUSTOM} />
        </Field>
        {pick === CUSTOM && (
          <Field label={t('model.custom_url')} hint={t('model.custom_url_hint_claude')}>
            <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://api.example.com" inputMode="url" spellCheck={false} autoCapitalize="none" required />
          </Field>
        )}
        <Field label="API Key" hint={keepKey ? t('model.key_keep') : t('model.key_hint')}>
          <PasswordInput value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" spellCheck={false} placeholder={keepKey ? '••••••••' : 'sk-…'} />
        </Field>
        <FoldNote summary={t('cprof.models')}>
          <p className="small muted" style={{ marginTop: 0 }}>
            {t('cprof.models_hint')}
          </p>
          <div className="cprof-models">
            {MODEL_KEYS.map((k) => (
              <Field key={k} label={t(`cprof.model.${k}`)}>
                <input className="mono" value={models[k]} onChange={(e) => setModels({ ...models, [k]: e.target.value })} maxLength={100} spellCheck={false} autoCapitalize="none" />
              </Field>
            ))}
          </div>
        </FoldNote>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!ready}>
            {t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/**
 * 一个账号。官方账号：已登录时按钮是“查用量”（请 Claude Code 去取，顺便确认登录状态；登录失效就提示重新登录），
 * 没登录时是“登录”，“重新登录”在更多菜单里。
 */
function ProfileRow({ p, isDefault, busy, nonce, onDefault, onLogin, onEdit, onRemove }) {
  const { t } = useTranslation();
  const toast = useToast();
  const official = p.kind === 'official';
  const [loaded, err] = useLoad(() => (official ? api.get(`/claude-profiles/${p.id}/status`) : Promise.resolve(null)), [p.id, nonce]);
  // 查用量时顺便拿到的登录状态比上面问到的新；重新问过之后以问到的为准
  const [fresh, setFresh] = useState(null);
  useEffect(() => setFresh(null), [loaded]);
  const s = fresh ?? loaded;
  const [usage, setUsage] = useState(null);
  const q = useAction();
  const queryUsage = async () => {
    const r = await q.run(() => api.post(`/claude-profiles/${p.id}/usage`, {}));
    if (!r) return;
    if (r.status && !r.status.error) setFresh(r.status);
    setUsage(r.state === 'ok' ? r : r.state === 'logged_out' ? { state: 'logged_out' } : null);
    if (r.state === 'logged_out') toast(t('cprof.expired_toast', { name: profileName(t, p) }), 'error');
    else if (r.state === 'no_limits') toast(t('cprof.usage_none'), 'error');
    else if (r.state === 'failed') toast(t('cprof.usage_failed'), 'error');
  };
  useEffect(() => {
    if (q.error) toast(errorText(t, q.error), 'error');
  }, [q.error]);
  const loggedIn = Boolean(s?.logged_in);
  return (
    <li className={isDefault ? 'cprof-default' : ''}>
      <button
        type="button"
        className="cprof-radio"
        role="radio"
        aria-checked={isDefault}
        disabled={busy}
        onClick={() => !isDefault && onDefault(p)}
        title={isDefault ? t('cprof.is_default') : t('cprof.make_default')}
        aria-label={`${profileName(t, p)} · ${isDefault ? t('cprof.is_default') : t('cprof.make_default')}`}
      />
      <span className="grow" style={{ minWidth: 0 }}>
        <b className="cprof-name">
          <KindIcon p={p} />
          <span className="ellipsis">{profileName(t, p)}</span>
          {isDefault && <span className="badge badge-ok">{t('cprof.default')}</span>}
        </b>
        <span className="small" style={{ display: 'block' }}>
          {official ? (
            <LoginStatus s={s} err={err} expired={usage?.state === 'logged_out'} />
          ) : (
            <>
              <span className="mono muted ellipsis">{p.base_url}</span>
              {p.models?.main && <span className="muted"> · {p.models.main}</span>}
              {!p.key_set && (
                <span className="cprof-warn">
                  {' '}
                  · <KeyRound size={13} style={{ verticalAlign: '-2px' }} /> {t('cprof.no_key')}
                </span>
              )}
            </>
          )}
        </span>
        {official && loggedIn && usage?.state === 'ok' && (
          <span className="small" style={{ display: 'block' }}>
            <UsageLine limits={usage.limits} />
          </span>
        )}
      </span>
      <span className="inline">
        {official &&
          (loggedIn || !s ? (
            <Button variant="ghost" icon={Gauge} onClick={queryUsage} busy={q.busy} disabled={busy || !s} title={t('cprof.usage_hint')}>
              {t('cprof.usage')}
            </Button>
          ) : (
            <Button variant="ghost" icon={LogIn} onClick={() => onLogin(p)} disabled={busy}>
              {t('cprof.login')}
            </Button>
          ))}
        {(p.id !== 'official' || loggedIn) && (
          <PopMenu
            label={t('action.more')}
            items={[
              official && loggedIn && { icon: LogIn, label: t('cprof.relogin'), onClick: () => onLogin(p) },
              p.id !== 'official' && !isDefault && { icon: Check, label: t('cprof.make_default'), onClick: () => onDefault(p) },
              p.id !== 'official' && { label: t(p.kind === 'api' ? 'cprof.edit' : 'cprof.rename'), onClick: () => onEdit(p) },
              p.id !== 'official' && { label: t('action.delete'), danger: true, onClick: () => onRemove(p) },
            ]}
          />
        )}
      </span>
    </li>
  );
}

export default function ClaudeProfilesCard() {
  const { t } = useTranslation();
  const confirm = useConfirm();
  const toast = useToast();
  const [data, error, reload] = useLoad(() => api.get('/claude-profiles'), []);
  const [form, setForm] = useState(null);
  // 登录完回来时重新问一遍登录状态
  const [nonce, setNonce] = useState(0);
  const { busy, error: actErr, run } = useAction();
  // 从开始页的“管理账号”过来时滚到这里
  useEffect(() => {
    if (data && window.location.hash === '#claude-accounts') document.getElementById('claude-accounts')?.scrollIntoView({ block: 'start' });
  }, [Boolean(data)]);
  useEffect(() => {
    const again = () => document.visibilityState === 'visible' && setNonce((n) => n + 1);
    document.addEventListener('visibilitychange', again);
    return () => document.removeEventListener('visibilitychange', again);
  }, []);
  const apply = () => reload();
  // 正在登录的账号（弹出登录框）
  const [loggingIn, setLoggingIn] = useState(null);
  const login = (p) => setLoggingIn(p);
  const setDefault = async (p) => {
    const r = await run(() => api.put('/claude-profiles/default', { id: p.id }));
    if (r) {
      apply(r);
      toast(t('cprof.default_set', { name: profileName(t, p) }));
    }
  };
  const remove = async (p) => {
    const ok = await confirm({ title: t('cprof.delete_title', { name: profileName(t, p) }), body: t(p.kind === 'api' ? 'cprof.delete_body_api' : 'cprof.delete_body_official'), confirm: t('action.delete'), danger: true });
    if (!ok) return;
    const r = await run(() => api.del(`/claude-profiles/${p.id}`));
    if (r) apply(r);
  };
  const saved = (r, added) => {
    setForm(null);
    apply(r);
    const p = r.profiles?.find((x) => x.id === r.added);
    // 新加的官方账号：接着去登录
    if (added && p?.kind === 'official') login(p);
    else toast(t('cprof.saved'));
  };
  return (
    <div id="claude-accounts" className="anchor">
      <Card
        title={t('cprof.title')}
        actions={
          data && (
            <PopMenu
              className="btn btn-secondary"
              icon={Plus}
              text={t('cprof.add')}
              label={t('cprof.add')}
              items={[
                { icon: UserRound, label: t('cprof.add_official'), hint: t('cprof.add_official_hint'), onClick: () => setForm({ kind: 'official' }) },
                { icon: Server, label: t('cprof.add_api'), hint: t('cprof.add_api_hint'), onClick: () => setForm({ kind: 'api' }) },
              ]}
            />
          )
        }
      >
        <p className="muted small">{t('cprof.intro')}</p>
        <ErrorNote error={error ?? actErr} />
        {!data && !error && <Skeleton height={96} />}
        {data && (
          <ul className="list cprof-list">
            {data.profiles.map((p) => (
              <ProfileRow
                key={p.id}
                p={p}
                isDefault={p.id === data.default}
                busy={busy}
                nonce={nonce}
                onDefault={setDefault}
                onLogin={login}
                onEdit={(x) => setForm({ kind: x.kind, initial: x })}
                onRemove={remove}
              />
            ))}
          </ul>
        )}
        <p className="muted small" style={{ margin: '12px 0 0' }}>
          {t('cprof.note')}
        </p>
      </Card>
      {loggingIn && (
        <LoginModal
          profile={loggingIn}
          onClose={() => {
            setLoggingIn(null);
            setNonce((n) => n + 1);
          }}
        />
      )}
      {form?.kind === 'official' && <OfficialForm initial={form.initial} onClose={() => setForm(null)} onSaved={saved} />}
      {form?.kind === 'api' && data && <ApiForm initial={form.initial} accepted={Boolean(data.accepted_at)} onClose={() => setForm(null)} onSaved={saved} />}
    </div>
  );
}

/**
 * 新建会话时选 Claude Code 用哪个账号 / 接口（开始页、新建终端表单）。`value` 为 null 表示用默认。
 * 那台电脑的客户端太旧（没有这个接口）、或者只有本机默认账号时什么都不显示。
 */
export function ClaudeProfilePick({ client, device, value, onChange, className = 'btn btn-plain start-adv' }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const [data] = useLoad(() => client.get('/claude-profiles').catch(() => null), [device]);
  // 换了电脑：另选的那个在新电脑上没有就回到默认
  useEffect(() => {
    if (data && value && !data.profiles.some((p) => p.id === value)) onChange(null);
  }, [data]);
  if (!data || data.profiles.length < 2) return null;
  const def = data.profiles.find((p) => p.id === data.default) ?? data.profiles[0];
  const cur = data.profiles.find((p) => p.id === value) ?? def;
  return (
    <PopMenu
      className={className}
      icon={(p) => <KindIcon p={cur} {...p} />}
      text={profileName(t, cur)}
      label={t('cprof.pick_label')}
      items={[
        ...data.profiles.map((p) => ({
          icon: (x) => <KindIcon p={p} {...x} />,
          label: profileName(t, p),
          hint: p.id === def.id ? t('cprof.default') : p.kind === 'api' ? p.base_url : undefined,
          checked: p.id === cur.id,
          onClick: () => onChange(p.id === def.id ? null : p.id),
        })),
        'sep',
        { label: t('cprof.manage'), onClick: () => navigate('/settings#claude-accounts') },
      ]}
    />
  );
}
