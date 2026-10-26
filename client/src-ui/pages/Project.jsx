// 项目页（/project?d=电脑&w=文件夹）：这个文件夹里的全部会话。在运行的在前，其余（停了的终端、以前的对话）按时间分组；
// 以前的对话每次取 50 个（GET /history?workdir=…&before=…），翻到底再取。搜索先在取到的里找，回车在对话全文里搜。
// 停了的 Claude Code / Codex / OpenCode 会话收在最下面：它们的对话已经在上面的列表里了，点一下就接着聊。
// `c=工具:对话号` 时展开那个对话（侧栏里点了可能正开在别处的对话，在这里看提示再选）。
// 多台电脑上的同一个仓库（侧栏里合成一个项目）：上面可以切到别的电脑上的那个文件夹。
import { ChevronDown, Folder, GitBranch, Pin, PinOff, Plus, Search, SquareTerminal, Trash2 } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { deviceApi } from '../shared/api.js';
import { deviceColor, DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import PopMenu from '../shared/PopMenu.jsx';
import { Link, query, useRouter } from '../shared/router.jsx';
import { AGENTS, projectEntries, projectPath, sessionState, sessionTitle, useSessions } from '../shared/sessions.jsx';
import { Spinner, useConfirm, useToast } from '../shared/ui.jsx';
import { ConvRow, timeBucket } from './History.jsx';
import { QUICK_LAUNCHES, shortAgo, useActions, useSessionUi } from './Sessions.jsx';

const PAGE = 50;
const enc = encodeURIComponent;
const BUCKETS = ['today', 'yesterday', 'week', 'month', 'earlier'];
const convKey = (c) => `${c.agent}:${c.id}`;
const ts = (s) => Date.parse(s || '') || 0;

export function ProjectPage() {
  const { t } = useTranslation();
  const { search } = useRouter();
  const s = useSessions();
  const d = query(search, 'd') || null;
  const w = query(search, 'w') ?? '';
  const devices = useDevices();
  const p = s.projects.find((x) => (x.device ?? null) === d && x.workdir === w);
  if (!p) {
    // 记着的项目还没取到时先等（离线的电脑用的是浏览器里记着的，不用等；没有这台电脑就是找不到）
    const known = !devices.multi || devices.byId(d)?.online;
    if (s.loading || (known && !s.storeLoaded(d))) return <div className="proj-loading"><Spinner size={20} /></div>;
    return (
      <div className="proj">
        <p className="muted">{t('proj.not_found')}</p>
        <Link to="/" className="btn btn-ghost">
          {t('proj.back')}
        </Link>
      </div>
    );
  }
  const g = s.projectGroups.find((x) => x.members.includes(p));
  return <ProjectView key={p.key} p={p} group={g?.members.length > 1 ? g : null} focus={query(search, 'c')} />;
}

/** 以前的对话，一页一页取；项目的对话有变化（侧栏的汇总变了）时重取第一页，已经翻出来的更早的留着。 */
function useConvs(p) {
  const [state, setState] = useState({ items: [], marks: {}, done: false, loading: true });
  const busy = useRef(false);
  const fetchPage = (before) => deviceApi(p.device).get(`/history?workdir=${enc(p.workdir)}&limit=${PAGE}${before ? `&before=${before}` : ''}`);
  const more = () => {
    if (busy.current || state.done) return;
    busy.current = true;
    const last = state.items[state.items.length - 1];
    setState((x) => ({ ...x, loading: true }));
    fetchPage(last ? ts(last.updated_at) : 0)
      .then(
        (r) => {
          const got = Array.isArray(r?.items) ? r.items : [];
          setState((x) => {
            const seen = new Set(x.items.map(convKey));
            return { items: [...x.items, ...got.filter((c) => !seen.has(convKey(c)))], marks: r?.marks ?? x.marks, done: got.length < PAGE, loading: false };
          });
        },
        () => setState((x) => ({ ...x, done: true, loading: false })),
      )
      .finally(() => {
        busy.current = false;
      });
  };
  const refresh = () =>
    fetchPage(0).then(
      (r) => {
        const got = Array.isArray(r?.items) ? r.items : [];
        setState((x) => {
          const fresh = new Set(got.map(convKey));
          const oldest = got.length ? ts(got[got.length - 1].updated_at) : Infinity;
          // 第一页之后翻出来的更早的留着（第一页里有的以第一页为准）
          const older = x.items.filter((c) => !fresh.has(convKey(c)) && ts(c.updated_at) < oldest);
          return { items: [...got, ...older], marks: r?.marks ?? {}, done: x.items.length > got.length ? x.done : got.length < PAGE, loading: false };
        });
      },
      // 旧版客户端没有这个接口、电脑离线
      () => setState((x) => ({ ...x, done: true, loading: false })),
    );
  const sig = `${p.total}:${p.convAt}:${p.items.filter((i) => i.running).length}`;
  useEffect(() => {
    if (!p.offline) refresh();
    else setState((x) => ({ ...x, loading: false, done: true }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, p.offline]);
  const setMark = (c, patch) => setState((x) => ({ ...x, marks: { ...x.marks, [convKey(c)]: { ...x.marks[convKey(c)], ...patch } } }));
  return { ...state, more, setMark };
}

/** 在对话全文里搜（回车）：这个项目里命中的对话。 */
function useDeep(p, q) {
  const [res, setRes] = useState(null);
  useEffect(() => setRes(null), [q]);
  const run = () => {
    if (!q.trim()) return;
    setRes({ q, items: null });
    deviceApi(p.device)
      .get(`/history?workdir=${enc(p.workdir)}&q=${enc(q.trim())}&deep=true&limit=200`)
      .then(
        (r) => setRes((x) => x?.q === q && { q, items: Array.isArray(r?.items) ? r.items : [] }),
        () => setRes((x) => x?.q === q && { q, items: [] }),
      );
  };
  return [res, run];
}

function ProjectView({ p, group, focus }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const { status } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const confirm = useConfirm();
  const convs = useConvs(p);
  const [q, setQ] = useState('');
  const [deep, runDeep] = useDeep(p, q);
  const [tool, setTool] = useState('all');
  const [showHidden, setShowHidden] = useState(false);
  const [open, setOpen] = useState(focus);
  const [stoppedOpen, setStoppedOpen] = useState(false);
  const focusRef = useRef(null);
  const sentinel = useRef(null);
  const allowed = status.account.allowed && !p.offline;
  const dev = devices.multi ? devices.byId(p.device) : null;

  const isHidden = (c) => Boolean(convs.marks[convKey(c)]?.hide);
  const source = deep?.items ?? convs.items;
  const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const hit = (text) => words.every((w) => text.toLowerCase().includes(w));
  const convOk = (c) => (tool === 'all' || c.agent === tool) && isHidden(c) === showHidden && (deep?.items || hit([c.title, c.last, c.branch].filter(Boolean).join('\n')));
  const instOk = (i) => (tool === 'all' || (tool === 'term' ? !AGENTS.includes(i.launch) : i.launch === tool)) && !showHidden && hit([sessionTitle(i), i.command, i.workdir].filter(Boolean).join('\n'));
  const e = projectEntries(p, source.filter(convOk));
  const live = e.live.filter((x) => instOk(x.inst));
  const rest = e.rest.filter((x) => x.kind === 'conv' || instOk(x.inst));
  const hiddenCount = convs.items.filter(isHidden).length;
  const now = Date.now();
  const groups = BUCKETS.map((b) => ({ b, items: rest.filter((x) => timeBucket(x.at, now) === b) })).filter((g) => g.items.length);

  // 展开侧栏里点进来的那个对话，取到后滚过去
  useEffect(() => {
    if (focus && focusRef.current) focusRef.current.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [focus, convs.items.length]);
  // 翻到底自动取更早的
  useEffect(() => {
    const el = sentinel.current;
    if (!el || convs.done || deep) return undefined;
    const io = new IntersectionObserver((xs) => xs.some((x) => x.isIntersecting) && convs.more());
    io.observe(el);
    return () => io.disconnect();
  });

  const hide = (c) => {
    const next = !isHidden(c);
    deviceApi(p.device)
      .put('/history/marks', { agent: c.agent, id: c.id, hide: next })
      .then(
        () => {
          convs.setMark(c, { hide: next });
          s.reloadFolders();
          if (next) toast(t('hist.hidden_done'));
        },
        (err) => toast(errorText(t, err), 'error'),
      );
  };
  const removeStopped = async () => {
    const ok = await confirm({ title: t('proj.remove_stopped_title', { count: e.stopped.length }), body: t('proj.remove_stopped_body'), confirm: t('action.delete'), danger: true });
    if (!ok) return;
    const client = deviceApi(p.device);
    await Promise.all(e.stopped.map((i) => client.del(`/instances/${i.id}`).catch(() => {})));
    s.reload();
  };

  const branch = convs.items.find((c) => c.branch)?.branch;
  const running = p.items.filter((i) => i.running).length;
  const termCount = p.items.filter((i) => !AGENTS.includes(i.launch)).length;
  const chips = [
    { id: 'all', label: t('proj.all', { count: projectEntries(p).count }) },
    ...AGENTS.filter((a) => p.counts[a]).map((a) => ({ id: a, label: `${t(`launch.${a}.title`)} ${p.counts[a]}` })),
    termCount > 0 && { id: 'term', label: t('proj.terms', { count: termCount }) },
  ].filter(Boolean);
  const newMenu = QUICK_LAUNCHES.map((l) => ({ icon: (x) => <LaunchIcon launch={l} {...x} />, label: t('sess.new_type', { type: t(`launch.${l}.title`) }), onClick: () => ui.quickCreate(p, l), disabled: !allowed }));
  const menu = [
    { icon: p.pinned ? PinOff : Pin, label: t(p.pinned ? 'sess.unpin' : 'sess.pin'), onClick: () => s.pinProject(p.device, p.workdir, !p.pinned) },
    p.items.length === 0 && {
      icon: Trash2,
      label: t('sess.remove_project'),
      danger: true,
      onClick: () => {
        s.removeProject(p.device, p.workdir);
        navigate('/');
      },
    },
  ];
  const convRow = (c) => (
    <div key={convKey(c)} ref={convKey(c) === focus ? focusRef : undefined}>
      <ConvRow device={p.device} conv={c} open={open === convKey(c)} onToggle={() => setOpen(open === convKey(c) ? null : convKey(c))} onHide={() => hide(c)} hidden={isHidden(c)} />
    </div>
  );
  const row = (x) => (x.kind === 'inst' ? <InstRow key={x.key} inst={x.inst} /> : convRow(x.conv));
  const empty = live.length === 0 && rest.length === 0;

  return (
    <div className="proj">
      <header className="proj-head">
        <span className="proj-icon">
          <Folder size={22} aria-hidden="true" />
        </span>
        <div className="proj-title">
          <h1 className="ellipsis">
            {p.name}
            {p.pinned && <Pin size={14} className="prow-pin" aria-label={t('sess.pinned')} />}
          </h1>
          <span className="proj-meta">
            <span className="mono ellipsis">{p.workdir}</span>
            {dev && !group && <DeviceTag device={dev} />}
            {branch && (
              <span className="proj-branch">
                <GitBranch size={12} aria-hidden="true" />
                {branch}
              </span>
            )}
            {running > 0 && <span>{t('proj.running', { count: running })}</span>}
            {p.offline && <span className="badge">{t('devices.offline')}</span>}
          </span>
        </div>
        <div className="proj-acts">
          <span className="split">
            <button type="button" className="btn btn-primary" onClick={() => ui.draftIn(p)} disabled={!allowed}>
              <Plus size={16} aria-hidden="true" />
              {t('sess.new')}
            </button>
            <PopMenu items={newMenu} className="btn btn-primary split-more" icon={ChevronDown} label={t('proj.new_other')} disabled={!allowed} />
          </span>
          <button type="button" className="btn btn-ghost" onClick={() => ui.quickCreate(p, 'shell')} disabled={!allowed}>
            <SquareTerminal size={16} aria-hidden="true" />
            {t('proj.terminal')}
          </button>
          <PopMenu items={menu} className="btn btn-ghost btn-icon" label={t('sess.project_menu')} />
        </div>
      </header>

      {group && (
        <div className="seg seg-sm proj-devs" role="group" aria-label={t('proj.on_devices')}>
          {group.members.map((m) => (
            <button key={m.key} type="button" aria-pressed={m === p} onClick={() => m !== p && navigate(projectPath(m), { replace: true })} title={m.workdir}>
              <span className={`dev-dot ${m.offline ? 'off' : ''}`} style={{ '--dc': deviceColor(devices, m.device) }} aria-hidden="true" />
              {deviceName(t, devices.byId(m.device))}
              <span className="muted">{m.offline ? t('devices.offline') : projectEntries(m).count}</span>
            </button>
          ))}
        </div>
      )}

      <div className="proj-tools">
        <label className="proj-search">
          <Search size={15} aria-hidden="true" />
          <input
            type="search"
            value={q}
            onChange={(ev) => setQ(ev.target.value)}
            onKeyDown={(ev) => {
              if (ev.key === 'Enter' && !ev.nativeEvent.isComposing) runDeep();
              if (ev.key === 'Escape') setQ('');
            }}
            placeholder={t('proj.search')}
            aria-label={t('proj.search')}
            spellCheck={false}
          />
        </label>
        <span className="seg-chips" role="group" aria-label={t('hist.by_tool')}>
          {chips.map((c) => (
            <button key={c.id} type="button" aria-pressed={tool === c.id} onClick={() => setTool(c.id)}>
              {c.label}
            </button>
          ))}
        </span>
        <span className="grow" />
        {(hiddenCount > 0 || showHidden) && (
          <button type="button" className="btn btn-plain btn-sm" aria-pressed={showHidden} onClick={() => setShowHidden(!showHidden)}>
            {t('proj.hidden', { count: hiddenCount })}
          </button>
        )}
      </div>
      {deep && <p className="small muted">{deep.items ? t('hist.deep_on', { q: deep.q }) : <Spinner size={14} />}</p>}
      {p.persistent === false && <p className="small muted">{t('hist.needs_persistent')}</p>}

      <div className="proj-list">
        {live.length > 0 && (
          <>
            <h3 className="proj-group">{t('proj.group_running', { count: live.length })}</h3>
            {live.map(row)}
          </>
        )}
        {groups.map((g) => (
          <section key={g.b} aria-label={t(`hist.when_${g.b}`)}>
            <h3 className="proj-group">{t(`hist.when_${g.b}`)}</h3>
            {g.items.map(row)}
          </section>
        ))}
        {empty && !convs.loading && <p className="proj-empty">{words.length ? t('hist.no_match') : showHidden ? t('proj.none_hidden') : t('proj.empty')}</p>}
        {!deep && !convs.done && (
          <div ref={sentinel} className="proj-more">
            <button type="button" className="btn btn-ghost btn-sm" onClick={convs.more} disabled={convs.loading}>
              {convs.loading ? <Spinner size={14} /> : t('hist.load_more')}
            </button>
          </div>
        )}
      </div>

      {e.stopped.length > 0 && !showHidden && (
        <section className="proj-stopped" aria-label={t('proj.stopped', { count: e.stopped.length })}>
          <div className="proj-stopped-head">
            <button type="button" className="btn btn-plain btn-sm" onClick={() => setStoppedOpen(!stoppedOpen)} aria-expanded={stoppedOpen}>
              <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
              {t('proj.stopped', { count: e.stopped.length })}
            </button>
            <span className="small muted grow">{t('proj.stopped_hint')}</span>
            <button type="button" className="btn btn-plain btn-sm danger-text" onClick={removeStopped}>
              {t('proj.remove_stopped')}
            </button>
          </div>
          {stoppedOpen && <div className="proj-list">{e.stopped.map((i) => <InstRow key={i.id} inst={i} />)}</div>}
        </section>
      )}
    </div>
  );
}

/** 项目页里的一个会话（终端）：点一下打开（停了的先启动），右边是状态或时间、更多操作。 */
function InstRow({ inst }) {
  const { t } = useTranslation();
  const a = useActions(inst);
  const state = inst.error && !inst.running ? 'error' : sessionState(inst);
  const when = Math.max(ts(inst.last_activity_at), ts(inst.started_at), ts(inst.created_at));
  const badge = { attn: 'badge-warn', working: 'badge-brand', idle: 'badge-ok', error: 'badge-danger' }[state];
  return (
    <div className="conv proj-inst">
      <button type="button" className="conv-row" onClick={a.primary} disabled={a.busy || !a.allowed} title={inst.workdir}>
        <LaunchIcon launch={inst.launch} size={18} className="conv-icon" />
        <span className="conv-title ellipsis">{sessionTitle(inst)}</span>
        <span className="conv-when">{a.busy ? <Spinner size={13} /> : badge ? <span className={`badge ${badge}`}>{t(`sess.state.${state}`)}</span> : when > 0 && shortAgo(t, when)}</span>
        <span className="conv-sub">
          <span className="ellipsis conv-last">{inst.command || t(`launch.${inst.launch}.title`)}</span>
        </span>
      </button>
      <PopMenu items={a.menu(true)} className="btn btn-plain btn-icon proj-inst-more" />
    </div>
  );
}
