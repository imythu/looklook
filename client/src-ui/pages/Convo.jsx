// 对话迁移：把一段对话搬到另一个 AI 工具里接着聊（客户端 src/convo）。
// 来源 / 目标是这台电脑上的“对话来源”：每种工具自动找到的默认目录，加上用户登记的目录（同一种工具可以登记好几个，
// 例如个人和工作两个 Codex）。目前 Codex、DSH 能读能写；Claude Code、OpenCode 能登记，显示为“即将支持”。
// 选来源和对话 → 预览能带过去多少、哪些不会带过去 → 选目标 → 迁移。到默认目录的 Codex 时直接打开一个接着聊的终端；
// DSH 是网页界面，新会话出现在它的会话列表里。原对话从不修改。
import { ArrowRight, Bot, Check, Fish, Monitor, Plus, Sparkles, SquareTerminal, Trash2 } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi, rememberTerminals } from '../shared/api.js';
import { deviceName, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { relativeTime } from '../shared/time.js';
import { Button, CopyButton, ErrorNote, Field, Modal, Note, Skeleton, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { FolderField } from './Terminals.jsx';

const KIND_ICON = { codex: Bot, dsh: Fish, claude: Sparkles, opencode: SquareTerminal };
const KINDS = ['codex', 'dsh', 'claude', 'opencode'];

function shortPath(p) {
  if (!p) return '';
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join('/')}` : p;
}

/** 来源的名字：登记时起的名字，否则是工具名。 */
export function sourceName(t, s) {
  return s?.name || t(`launch.${s?.kind}.title`, { defaultValue: s?.kind ?? '' });
}

/** 不会带过去的东西：按类别列出数量。 */
function Dropped({ manifest }) {
  const { t } = useTranslation();
  const dropped = Object.entries(manifest?.dropped ?? {});
  const synth = manifest?.synthesized?.missing_tool_result ?? 0;
  if (dropped.length === 0 && !synth) return null;
  return (
    <p className="small muted" style={{ margin: '6px 0 0' }}>
      {dropped.length > 0 && (
        <>
          {t('convo.dropped')}
          {dropped.map(([k, n], i) => (
            <span key={k}>
              {i > 0 ? t('convo.sep') : ' '}
              {t(`convo.kind.${k}`, { defaultValue: t('convo.kind.other', { kind: k }) })} ×{n}
            </span>
          ))}
        </>
      )}
      {synth > 0 && <span style={{ display: 'block' }}>{t('convo.synthesized', { n: synth })}</span>}
    </p>
  );
}

/** 目标卡片上的状态：能迁移 / 就是来源 / 目录不在 / 即将支持。 */
function targetState(s, fromId) {
  if (s.id === fromId) return 'source';
  if (!s.can_import) return 'soon';
  if (!s.exists) return 'missing';
  return 'ok';
}

const STATE_BADGE = { ok: 'badge-ok', source: '', missing: 'badge-warn', soon: 'badge-warn' };

function TargetCard({ s, state, picked, onPick, onRemove }) {
  const { t } = useTranslation();
  const Icon = KIND_ICON[s.kind] ?? Bot;
  const usable = state === 'ok';
  return (
    <div className={`convo-target ${picked ? 'on' : ''} ${usable ? '' : 'off'}`}>
      <label className="convo-target-main">
        <input type="radio" name="convo-target" checked={picked} disabled={!usable} onChange={onPick} />
        <Icon size={18} aria-hidden="true" />
        <span className="grow" style={{ minWidth: 0 }}>
          <span className="convo-target-name">
            <b className="ellipsis">{sourceName(t, s)}</b>
            <span className={`badge ${STATE_BADGE[state]}`}>{t(`convo.target.${state}`)}</span>
          </span>
          <span className="small muted mono ellipsis" style={{ display: 'block' }} title={s.home}>
            {shortPath(s.home)}
            {s.registered ? ` · ${t('convo.registered')}` : ''}
          </span>
        </span>
      </label>
      {s.registered && (
        <button type="button" className="btn btn-plain btn-icon" onClick={onRemove} aria-label={t('convo.unregister', { name: sourceName(t, s) })} title={t('convo.unregister', { name: sourceName(t, s) })}>
          <Trash2 size={16} />
        </button>
      )}
    </div>
  );
}

/** 登记一个 AI 工具的数据目录：先检查（找不找得到程序、里面有几段对话、支持到什么程度），再保存。 */
function RegisterDialog({ open, onClose, onSaved, client }) {
  const { t } = useTranslation();
  const [kind, setKind] = useState('claude');
  const [name, setName] = useState('');
  const [home, setHome] = useState('');
  const [bin, setBin] = useState('');
  const [check, setCheck] = useState(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  useEffect(() => {
    if (!open) return;
    setKind('claude');
    setName('');
    setHome('');
    setBin('');
    setCheck(null);
    setError(null);
  }, [open]);
  // 内容改了就重新检查（停顿半秒后）
  useEffect(() => {
    if (!open || !home.trim()) {
      setCheck(null);
      return undefined;
    }
    let alive = true;
    const timer = setTimeout(async () => {
      setChecking(true);
      try {
        const r = await client.post('/convo/harnesses/check', { kind, name, home: home.trim(), bin: bin.trim() || undefined });
        if (alive) setCheck(r);
      } catch (e) {
        if (alive) setCheck({ failed: e });
      } finally {
        if (alive) setChecking(false);
      }
    }, 500);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [open, kind, home, bin]);
  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await client.post('/convo/harnesses', { kind, name: name.trim(), home: home.trim(), bin: bin.trim() || undefined });
      onSaved(r.item);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const support = (yes) => (yes ? t('convo.reg.yes') : t('convo.reg.soon'));
  return (
    <Modal open={open} onClose={onClose} title={t('convo.reg.title')}>
      <p className="small muted" style={{ marginTop: 0 }}>
        {t('convo.reg.intro')}
      </p>
      <div className="field">
        <span className="field-label">{t('convo.reg.kind')}</span>
        <div className="seg seg-sm" role="group" aria-label={t('convo.reg.kind')}>
          {KINDS.map((k) => (
            <button key={k} type="button" aria-pressed={kind === k} onClick={() => setKind(k)}>
              {t(`launch.${k}.title`)}
            </button>
          ))}
        </div>
      </div>
      <Field label={t('convo.reg.name')} hint={t('convo.reg.name_hint')}>
        <input value={name} maxLength={40} onChange={(e) => setName(e.target.value)} placeholder={t('convo.reg.name_ph', { tool: t(`launch.${kind}.title`) })} />
      </Field>
      <FolderField value={home} onChange={setHome} label={t('convo.reg.home')} hint={t('convo.reg.home_hint')} client={client} />
      <Field label={t('convo.reg.bin')} hint={t('convo.reg.bin_hint')}>
        <input className="mono" value={bin} onChange={(e) => setBin(e.target.value)} spellCheck={false} autoComplete="off" />
      </Field>
      {home.trim() && (
        <div className="convo-check" aria-live="polite">
          <b className="small">{t('convo.reg.check')}</b>
          {checking && !check && <Skeleton height={48} />}
          {check?.failed && <ErrorNote error={check.failed} />}
          {check && !check.failed && (
            <>
              {check.error ? (
                <p className="small convo-check-bad">{t(`convo.reg.err.${check.error.field}_${check.error.rule}`, { defaultValue: t('convo.reg.err.other') })}</p>
              ) : (
                <p className="small convo-check-ok">
                  <Check size={14} aria-hidden="true" /> {check.sessions != null ? t('convo.reg.sessions', { n: check.sessions }) : t('convo.reg.dir_ok')}
                </p>
              )}
              <p className="small muted" style={{ margin: 0 }}>
                {check.program ? t('convo.reg.program', { path: check.program }) : t('convo.reg.no_program')}
              </p>
              <p className="small" style={{ margin: 0 }}>
                {t('convo.reg.export')}：{support(check.can_export)} · {t('convo.reg.import')}：{support(check.can_import)}
              </p>
            </>
          )}
        </div>
      )}
      <ErrorNote error={error} />
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          {t('action.cancel')}
        </Button>
        <Button busy={busy} disabled={!home.trim() || Boolean(check?.error) || checking} onClick={save}>
          {t('convo.reg.submit')}
        </Button>
      </div>
    </Modal>
  );
}

export default function ConvoDialog({ open, onClose, onOpened, preset }) {
  const { t } = useTranslation();
  const toast = useToast();
  const confirm = useConfirm();
  const devices = useDevices();
  const dev = devices.multi ? devices.selected : null;
  const client = deviceApi(dev);
  const [sources, sourcesError, reloadSources] = useLoad(() => (open ? client.get('/convo/harnesses') : Promise.resolve(null)), [open, dev]);
  const all = sources?.items ?? [];
  const readable = all.filter((s) => s.can_export && s.exists);
  const [from, setFrom] = useState('codex');
  const [to, setTo] = useState(null);
  const [query, setQuery] = useState('');
  const [picked, setPicked] = useState(null);
  const [cwd, setCwd] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const [done, setDone] = useState(null);
  const [register, setRegister] = useState(false);
  const [list, listError] = useLoad(() => (open && from ? client.get(`/convo/sessions?from=${encodeURIComponent(from)}`) : Promise.resolve(null)), [open, from, dev]);
  const [preview, previewError] = useLoad(
    () => (open && picked ? client.post('/convo/preview', { from, id: picked.id }) : Promise.resolve(null)),
    [open, from, picked?.id, dev],
  );
  // 从对话记录页打开时带着来源和那段对话
  const presetRef = useRef(null);
  useEffect(() => {
    if (!open) return;
    setPicked(null);
    setQuery('');
    setError(null);
    setDone(null);
    setTo(null);
    presetRef.current = preset ?? null;
    if (preset?.from === from && preset.session) {
      // 来源没变，下面按来源清空选择的那段不会再跑：直接选上
      setPicked(preset.session);
      presetRef.current = null;
    } else if (preset?.from) setFrom(preset.from);
  }, [open]);
  // 默认来源：读得到的第一个
  useEffect(() => {
    if (readable.length && !readable.some((s) => s.id === from) && !presetRef.current) setFrom(readable[0].id);
  }, [sources]);
  useEffect(() => {
    const p = presetRef.current;
    if (p && p.from === from && p.session) {
      setPicked(p.session);
      presetRef.current = null;
    } else setPicked(null);
    setError(null);
  }, [from]);
  useEffect(() => setCwd(picked?.cwd ?? ''), [picked?.id]);
  // 默认目标：第一个能迁移进去、又不是来源的
  useEffect(() => {
    const cur = all.find((s) => s.id === to);
    if (cur && targetState(cur, from) === 'ok') return;
    setTo(all.find((s) => targetState(s, from) === 'ok')?.id ?? null);
  }, [sources, from]);
  const target = all.find((s) => s.id === to);
  const fromSrc = all.find((s) => s.id === from);
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const items = (list?.items ?? []).filter((s) => words.every((w) => `${s.title ?? ''} ${s.cwd ?? ''} ${s.id}`.toLowerCase().includes(w)));
  const migrate = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await client.post('/convo/migrate', { from, id: picked.id, to, cwd: cwd.trim() || undefined });
      if (r.instance) {
        if (dev) rememberTerminals(dev, [r.instance]);
        toast(t('convo.opened', { tool: sourceName(t, target) }));
        onClose();
        onOpened?.(r.instance);
      } else {
        setDone({ ...r.migrated, target });
      }
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const unregister = async (s) => {
    if (!(await confirm({ title: t('convo.unregister', { name: sourceName(t, s) }), body: t('convo.unregister_body'), confirm: t('convo.unregister_confirm'), danger: true }))) return;
    try {
      await client.del(`/convo/harnesses/${encodeURIComponent(s.id)}`);
      reloadSources();
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  return (
    <Modal open={open} onClose={onClose} title={t('convo.title')} wide>
      {devices.multi && (
        <p className="muted small" style={{ margin: '0 0 8px' }}>
          <Monitor size={13} aria-hidden="true" /> {deviceName(t, devices.byId(dev))}
        </p>
      )}
      {done ? (
        <>
          <Note kind="ok">
            <b>{t('convo.done_title', { tool: sourceName(t, done.target) })}</b>
            <span style={{ display: 'block', marginTop: 4 }}>{t(done.target?.kind === 'dsh' ? 'convo.done_dsh_hint' : 'convo.done_hint', { n: done.items })}</span>
          </Note>
          <Field label={t('convo.resume_cmd')}>
            <span className="inline" style={{ flexWrap: 'nowrap' }}>
              <code className="mono small ellipsis grow">{done.resume}</code>
              <CopyButton text={done.resume} />
            </span>
          </Field>
          <Dropped manifest={done.manifest} />
          <div className="modal-actions">
            <Button variant="secondary" onClick={() => setDone(null)}>
              {t('convo.again')}
            </Button>
            <Button onClick={onClose}>{t('action.close')}</Button>
          </div>
        </>
      ) : (
        <>
          <p className="small muted" style={{ margin: '0 0 8px' }}>
            {t('convo.intro')}
          </p>
          <ErrorNote error={sourcesError} />
          <span className="field-label">{t('convo.from')}</span>
          <div className="seg seg-sm convo-from" role="group" aria-label={t('convo.from')}>
            {readable.map((s) => {
              const Icon = KIND_ICON[s.kind] ?? Bot;
              return (
                <button key={s.id} type="button" aria-pressed={from === s.id} onClick={() => setFrom(s.id)} title={s.home}>
                  <Icon size={15} aria-hidden="true" /> {sourceName(t, s)}
                </button>
              );
            })}
          </div>
          <ErrorNote error={listError} />
          <input
            type="search"
            className="convo-search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('convo.search')}
            aria-label={t('convo.search')}
            spellCheck={false}
          />
          <div className="convo-list" role="listbox" aria-label={t('convo.sessions')}>
            {!list && !listError ? (
              <Skeleton height={120} />
            ) : items.length === 0 ? (
              <p className="muted small" style={{ padding: 8 }}>
                {t(query ? 'convo.no_match' : 'convo.empty', { tool: sourceName(t, fromSrc) })}
              </p>
            ) : (
              items.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  role="option"
                  aria-selected={picked?.id === s.id}
                  className={`convo-item ${picked?.id === s.id ? 'on' : ''}`}
                  onClick={() => setPicked(s)}
                  title={`${s.title ?? s.id}\n${s.cwd ?? ''}`}
                >
                  <span className="ellipsis">{s.title ?? s.id}</span>
                  <span className="small muted ellipsis">
                    {s.cwd ? <span className="mono">{shortPath(s.cwd)}</span> : null}
                    {s.cwd ? ' · ' : ''}
                    {relativeTime(t, new Date(s.updated_ms).toISOString())}
                  </span>
                </button>
              ))
            )}
          </div>
          {picked && (
            <div style={{ marginTop: 12 }}>
              <ErrorNote error={previewError} />
              {preview && (
                <Note kind="info">
                  {t('convo.preview_n', { n: preview.items })}
                  <Dropped manifest={preview.manifest} />
                </Note>
              )}
              <span className="field-label">{t('convo.to')}</span>
              <div className="convo-targets" role="radiogroup" aria-label={t('convo.to')}>
                {all.map((s) => (
                  <TargetCard key={s.id} s={s} state={targetState(s, from)} picked={to === s.id} onPick={() => setTo(s.id)} onRemove={() => unregister(s)} />
                ))}
                <button type="button" className="convo-target-add" onClick={() => setRegister(true)}>
                  <Plus size={16} aria-hidden="true" /> {t('convo.register')}
                </button>
              </div>
              <Field label={t('convo.cwd')} hint={t('convo.cwd_hint')}>
                <input className="mono" value={cwd} onChange={(e) => setCwd(e.target.value)} spellCheck={false} autoComplete="off" />
              </Field>
            </div>
          )}
          <ErrorNote error={error} />
          <div className="modal-actions">
            <Button variant="ghost" onClick={onClose}>
              {t('action.cancel')}
            </Button>
            <Button busy={busy} disabled={!picked || !preview || !target} onClick={migrate} icon={ArrowRight}>
              {target ? t('convo.submit', { tool: sourceName(t, target) }) : t('convo.pick_target')}
            </Button>
          </div>
          <RegisterDialog
            open={register}
            onClose={() => setRegister(false)}
            client={client}
            onSaved={(s) => {
              reloadSources();
              if (s.can_import && s.id !== from) setTo(s.id);
              toast(t('convo.reg.saved', { name: sourceName(t, s) }));
            }}
          />
        </>
      )}
    </Modal>
  );
}
