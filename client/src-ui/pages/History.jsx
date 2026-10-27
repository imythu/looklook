// 以前的对话：电脑上 Claude Code、Codex、OpenCode 自己存的对话（客户端 src/history.rs），点一下在看看里接着聊。
// “所有以前的对话”页面（/history）；侧栏、项目页、草稿页里的也从这里接着聊。
// 点一行先展开看最后几句，再点“继续”；正开在看看会话里的直接打开那个会话；
// 可能正开在别处（文件两分钟内还在变）的默认“复制一份继续”，原来那份不动。
import { AlertCircle, ChevronDown, Copy, ExternalLink, Eye, EyeOff, Folder, GitBranch, Play, Search, SlidersHorizontal, Star, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi } from '../shared/api.js';
import { DeviceAvatar, DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { useRouter } from '../shared/router.jsx';
import { baseName, useSessions } from '../shared/sessions.jsx';
import { relativeTime } from '../shared/time.js';
import { ErrorNote, Spinner, useToast } from '../shared/ui.jsx';
import { isTouch } from '../shared/KeyBar.jsx';
import { useSessionUi } from './Sessions.jsx';

// 能“复制一份继续”的（OpenCode 没有这个功能）
export const CAN_FORK = ['claude', 'codex'];
// 页面里每个文件夹先列几个
const GROUP_SHOW = 5;

const enc = encodeURIComponent;

/** 展开后的一行：最后几句和按钮。 */
function ConvPeek({ device, conv, onHide, hidden }) {
  const { t } = useTranslation();
  const [msgs, setMsgs] = useState(null);
  useEffect(() => {
    let alive = true;
    deviceApi(device)
      .get(`/history/${conv.agent}/${enc(conv.id)}`)
      .then(
        (r) => alive && setMsgs(Array.isArray(r?.items) ? r.items : []),
        () => alive && setMsgs([]),
      );
    return () => {
      alive = false;
    };
  }, [device, conv.agent, conv.id]);
  const tool = t(`launch.${conv.agent}.title`);
  return (
    <div className="conv-peek">
      {msgs === null ? (
        <Spinner size={14} />
      ) : (
        msgs.length > 0 && (
          <div className="conv-msgs">
            {msgs.map((m, i) => (
              <p key={i} className={`conv-msg conv-msg-${m.role}`}>
                <b>{m.role === 'user' ? t('hist.you') : tool}</b>
                {m.text}
              </p>
            ))}
          </div>
        )
      )}
      <ConvPeekActions device={device} conv={conv} onHide={onHide} hidden={hidden} />
    </div>
  );
}

/** 继续 / 复制一份继续 / 打开已在看看里的会话，可能正开在别处时先提示。 */
function ConvPeekActions({ device, conv, onHide, hidden }) {
  const { t } = useTranslation();
  const ui = useSessionUi();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const first = useRef(null);
  useEffect(() => {
    if (!isTouch()) first.current?.focus({ preventScroll: true });
  }, [conv.id]);
  const go = async (fork) => {
    if (busy) return;
    setBusy(true);
    try {
      await ui.continueConv(device, conv, fork);
    } catch (e) {
      toast(errorText(t, e), 'error');
      setBusy(false);
    }
  };
  const fork = conv.live && CAN_FORK.includes(conv.agent);
  return (
    <>
      {conv.live && !conv.instance_id && (
        <p className="conv-warn">
          <AlertCircle size={14} aria-hidden="true" />
          {t(fork ? 'hist.live_fork' : 'hist.live')}
        </p>
      )}
      <div className="conv-acts">
        {conv.instance_id ? (
          <button ref={first} type="button" className="btn btn-primary btn-sm" onClick={() => ui.openSession(conv.instance_id)}>
            <ExternalLink size={15} aria-hidden="true" />
            {t('hist.open_session')}
          </button>
        ) : fork ? (
          <>
            <button ref={first} type="button" className="btn btn-primary btn-sm" onClick={() => go(true)} disabled={busy}>
              {busy ? <Spinner size={15} /> : <Copy size={15} aria-hidden="true" />}
              {t('hist.fork')}
            </button>
            <button type="button" className="btn btn-ghost btn-sm" onClick={() => go(false)} disabled={busy}>
              {t('hist.continue_anyway')}
            </button>
          </>
        ) : (
          <>
            <button ref={first} type="button" className="btn btn-primary btn-sm" onClick={() => go(false)} disabled={busy}>
              {busy ? <Spinner size={15} /> : <Play size={15} aria-hidden="true" />}
              {t('hist.continue')}
            </button>
            {CAN_FORK.includes(conv.agent) && (
              <button type="button" className="btn btn-ghost btn-sm" onClick={() => go(true)} disabled={busy}>
                <Copy size={15} aria-hidden="true" />
                {t('hist.fork')}
              </button>
            )}
          </>
        )}
        {onHide && (
          <button type="button" className="btn btn-plain btn-sm" onClick={onHide}>
            {t(hidden ? 'hist.unhide' : 'hist.hide')}
          </button>
        )}
      </div>
    </>
  );
}

/** 一个以前的对话：点开看最后几句和“继续”。`onHide` 给了时多一个“隐藏”（`hidden` 时是“取消隐藏”）。 */
export function ConvRow({ device, conv, open, onToggle, onHide, hidden }) {
  const { t } = useTranslation();
  const status = conv.instance_id ? (
    <span className="badge badge-brand">{t('hist.in_looklook')}</span>
  ) : conv.live ? (
    <span className="badge badge-warn">{t('hist.live_badge')}</span>
  ) : null;
  return (
    <div className={`conv ${open ? 'open' : ''}`}>
      <button type="button" className="conv-row" onClick={onToggle} aria-expanded={open} title={conv.workdir}>
        <LaunchIcon launch={conv.agent} size={18} className="conv-icon" />
        <span className="conv-title ellipsis">{conv.title}</span>
        <span className="conv-when">{status ?? relativeTime(t, conv.updated_at)}</span>
        <span className="conv-sub">
          {conv.branch && (
            <span className="conv-place conv-branch">
              <GitBranch size={12} aria-hidden="true" />
              {conv.branch}
            </span>
          )}
          {conv.last && <span className="ellipsis conv-last">{conv.last}</span>}
        </span>
      </button>
      {open && <ConvPeek device={device} conv={conv} onHide={onHide} hidden={hidden} />}
    </div>
  );
}

// ---------------- 所有以前的对话（/history） ----------------
//
// 左边筛选（每项带数量，可以多选）：工具、电脑、项目、状态、时间、分支、我的标记；
// 中间列表按时间（默认）/ 项目 / 电脑分组；右边看完整的对话（窄屏在原地展开）。
// 搜索框实时过滤标题、你说的话、文件夹、分支，回车搜对话全文（每台电脑扫描记录）；支持 agent:codex dev:名字 branch:main。
// 筛选写在网址里（?q=…&f=…），后退、刷新都还在。收藏、隐藏存在各自的电脑上（PUT /history/marks）。

const PAGE = 500;
const DAY = 86400e3;
const TIMES = { today: 1, week: 7, month: 30 };
const STATES = ['open', 'live', 'ended'];
const GROUPS = ['time', 'project', 'device'];

const convKey = (c) => `${c.dev ?? ''}\n${c.agent}:${c.id}`;
const projKey = (c) => `${c.dev ?? ''}\n${c.workdir}`;
const stateOf = (c) => (c.instance_id ? 'open' : c.live ? 'live' : 'ended');
const updatedMs = (c) => Date.parse(c.updated_at) || 0;

function readFilters(search) {
  const u = new URLSearchParams(search);
  let f = {};
  try {
    f = JSON.parse(u.get('f') ?? '{}') ?? {};
  } catch {
    f = {};
  }
  const arr = (v) => (Array.isArray(v) ? v.filter((x) => typeof x === 'string') : []);
  return {
    q: u.get('q') ?? '',
    agents: arr(f.a),
    devs: arr(f.d),
    projs: arr(f.p),
    states: arr(f.s),
    branches: arr(f.b),
    time: TIMES[f.t] ? f.t : 'all',
    mark: ['star', 'hidden'].includes(f.m) ? f.m : '',
    group: GROUPS.includes(f.g) ? f.g : 'time',
  };
}

function writeFilters(f) {
  const short = { a: f.agents, d: f.devs, p: f.projs, s: f.states, b: f.branches, t: f.time === 'all' ? undefined : f.time, m: f.mark || undefined, g: f.group === 'time' ? undefined : f.group };
  const clean = Object.fromEntries(Object.entries(short).filter(([, v]) => v !== undefined && !(Array.isArray(v) && v.length === 0)));
  const u = new URLSearchParams();
  if (f.q.trim()) u.set('q', f.q);
  if (Object.keys(clean).length) u.set('f', JSON.stringify(clean));
  const qs = u.toString();
  try {
    window.history.replaceState(null, '', `/history${qs ? `?${qs}` : ''}`);
  } catch {
    /* 忽略 */
  }
}

/** 搜索框里的 agent:codex dev:名字 branch:main，其余是要搜的文字。 */
function parseQuery(q) {
  const tokens = { agent: [], dev: [], branch: [] };
  const words = [];
  for (const w of q.trim().split(/\s+/).filter(Boolean)) {
    const m = w.match(/^(agent|dev|branch):(.+)$/i);
    if (m) tokens[m[1].toLowerCase()].push(m[2].toLowerCase());
    else words.push(w.toLowerCase());
  }
  return { ...tokens, text: words.join(' ') };
}

/** 按时间分组的组名。 */
export function timeBucket(ms, now) {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  const today = start.getTime();
  if (ms >= today) return 'today';
  if (ms >= today - DAY) return 'yesterday';
  if (ms >= today - 6 * DAY) return 'week';
  if (ms >= today - 29 * DAY) return 'month';
  return 'earlier';
}

function Facet({ title, items, selected, onToggle, render, search, limit = 8 }) {
  const { t } = useTranslation();
  const [all, setAll] = useState(false);
  const [q, setQ] = useState('');
  if (items.length === 0) return null;
  const words = q.trim().toLowerCase();
  const list = items.filter((x) => !words || (x.text ?? x.label).toLowerCase().includes(words));
  const shown = all || words ? list : list.slice(0, limit);
  return (
    <section className="hfacet">
      <h3 className="hfacet-title">{title}</h3>
      {search && items.length > limit && (
        <input className="hfacet-search" type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('hist.f_search')} aria-label={t('hist.f_search')} spellCheck={false} />
      )}
      {shown.map((x) => (
        <button key={x.key} type="button" className="hfacet-item" aria-pressed={selected.includes(x.key)} onClick={() => onToggle(x.key)} title={x.title ?? x.label}>
          {render ? render(x) : <span className="ellipsis grow">{x.label}</span>}
          <span className="hfacet-n">{x.n}</span>
        </button>
      ))}
      {!all && !words && list.length > limit && (
        <button type="button" className="hfacet-more" onClick={() => setAll(true)}>
          {t('hist.more', { count: list.length - limit })}
        </button>
      )}
    </section>
  );
}

function HistRow({ conv, selected, mark, onSelect, onOpen, onMark, showDevice, showProject }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const dev = showDevice && devices.multi ? devices.byId(conv.dev) : null;
  return (
    <div className={`hrow ${selected ? 'on' : ''} ${mark.hide ? 'hidden-conv' : ''}`}>
      <button type="button" className="hrow-main" onClick={onSelect}
        onDoubleClick={onOpen}
        // 已经选中的一行再按回车：直接继续（第一次回车是选中、看预览）
        onKeyDown={(e) => {
          if (e.key === 'Enter' && selected && onOpen) {
            e.preventDefault();
            onOpen();
          }
        }}
        aria-pressed={selected} title={`${conv.workdir}\n${t('hist.open_hint')}`}>
        <LaunchIcon launch={conv.agent} size={18} className="conv-icon" />
        <span className="hrow-line">
          {mark.star && <Star size={13} className="hrow-star" aria-label={t('hist.starred')} />}
          <span className="ellipsis hrow-title">{conv.title}</span>
          {conv.instance_id ? <span className="badge badge-brand">{t('hist.in_looklook')}</span> : conv.live ? <span className="badge badge-warn">{t('hist.live_badge')}</span> : null}
        </span>
        <span className="hrow-meta">
          {showProject && (
            <span className="conv-place">
              <Folder size={12} aria-hidden="true" />
              {baseName(conv.workdir) || conv.workdir}
            </span>
          )}
          {conv.branch && (
            <span className="conv-place conv-branch">
              <GitBranch size={12} aria-hidden="true" />
              {conv.branch}
            </span>
          )}
          {dev && <DeviceTag device={dev} />}
          <span className="hrow-when">{relativeTime(t, conv.updated_at)}</span>
        </span>
        {(conv.snippet || conv.last) && <span className="ellipsis hrow-last">{conv.snippet ?? conv.last}</span>}
      </button>
      <span className="hrow-acts">
        <button type="button" className="btn btn-plain btn-icon" onClick={() => onMark({ star: !mark.star })} aria-pressed={Boolean(mark.star)} aria-label={t(mark.star ? 'hist.unstar' : 'hist.star')} title={t(mark.star ? 'hist.unstar' : 'hist.star')}>
          <Star size={15} />
        </button>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => onMark({ hide: !mark.hide })} aria-pressed={Boolean(mark.hide)} aria-label={t(mark.hide ? 'hist.unhide' : 'hist.hide')} title={t(mark.hide ? 'hist.unhide' : 'hist.hide')}>
          {mark.hide ? <Eye size={15} /> : <EyeOff size={15} />}
        </button>
      </span>
    </div>
  );
}

/** 右边：完整的对话（最近 200 条）和继续按钮。 */
function HistDetail({ conv, onClose }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const [msgs, setMsgs] = useState(null);
  const box = useRef(null);
  useEffect(() => {
    let alive = true;
    setMsgs(null);
    deviceApi(conv.dev)
      .get(`/history/${conv.agent}/${enc(conv.id)}?full=true`)
      .then(
        (r) => alive && setMsgs(Array.isArray(r?.items) ? r.items : []),
        () => alive && setMsgs([]),
      );
    return () => {
      alive = false;
    };
  }, [conv.dev, conv.agent, conv.id]);
  useEffect(() => {
    if (msgs && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [msgs]);
  const dev = devices.multi ? devices.byId(conv.dev) : null;
  const tool = t(`launch.${conv.agent}.title`);
  return (
    <aside className="hdetail" aria-label={conv.title}>
      <header className="hdetail-head">
        <LaunchIcon launch={conv.agent} size={18} />
        <span className="hdetail-titles">
          <b className="hdetail-title">{conv.title}</b>
          <span className="hrow-meta">
            <span className="conv-place">
              <Folder size={12} aria-hidden="true" />
              {conv.workdir}
            </span>
            {conv.branch && (
              <span className="conv-place">
                <GitBranch size={12} aria-hidden="true" />
                {conv.branch}
              </span>
            )}
            {dev && <DeviceTag device={dev} />}
            {conv.started_at && <span>{t('hist.started', { when: relativeTime(t, conv.started_at) })}</span>}
          </span>
        </span>
        <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('hist.close_preview')} title={t('hist.close_preview')}>
          <X size={16} />
        </button>
      </header>
      <div className="hdetail-msgs" ref={box}>
        {msgs === null ? (
          <div className="hist-loading">
            <Spinner size={16} />
          </div>
        ) : msgs.length === 0 ? (
          <p className="conv-empty">{t('hist.no_preview')}</p>
        ) : (
          msgs.map((m, i) => (
            <div key={i} className={`hmsg hmsg-${m.role}`}>
              <b>{m.role === 'user' ? t('hist.you') : tool}</b>
              <p>{m.text}</p>
            </div>
          ))
        )}
      </div>
      <div className="hdetail-foot">
        <ConvPeekActions device={conv.dev} conv={conv} />
      </div>
    </aside>
  );
}

const SCOPE_KEY = 'looklook.hist.scope';
function readScope() {
  try {
    return localStorage.getItem(SCOPE_KEY) === 'all' ? 'all' : 'here';
  } catch {
    return 'here';
  }
}
function writeScope(v) {
  try {
    localStorage.setItem(SCOPE_KEY, v);
  } catch {
    /* 无痕模式：不记住 */
  }
}

/** 滚到列表底部附近时自动加载更多（按钮还在，键盘、读屏照样能点）。 */
function AutoMore({ onMore, busy, children }) {
  const ref = useRef(null);
  const more = useRef(onMore);
  more.current = onMore;
  useEffect(() => {
    if (busy || !ref.current || !window.IntersectionObserver) return undefined;
    const io = new IntersectionObserver((es) => es.some((e) => e.isIntersecting) && more.current(), { rootMargin: '400px 0px' });
    io.observe(ref.current);
    return () => io.disconnect();
  }, [busy]);
  return <div ref={ref}>{children}</div>;
}

/** 窄屏：右边放不下完整预览，改在原地展开。 */
function useNarrowHist() {
  const q = '(max-width: 1179px)';
  const [narrow, setNarrow] = useState(() => window.matchMedia?.(q).matches ?? false);
  useEffect(() => {
    const m = window.matchMedia?.(q);
    if (!m) return undefined;
    const on = () => setNarrow(m.matches);
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  }, []);
  return narrow;
}

export function HistoryPage() {
  const { t } = useTranslation();
  const { search } = useRouter();
  const devices = useDevices();
  const s = useSessions();
  const toast = useToast();
  const [f, setF] = useState(() => readFilters(search));
  const [groups, setGroups] = useState(null);
  const [deepQ, setDeepQ] = useState('');
  const [sel, setSel] = useState(null);
  const [showFilters, setShowFilters] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const ui = useSessionUi();
  // 先看当前这台电脑的（记住选择）；“全部电脑”时这台排在最前、先显示，别的电脑的到了再补上
  const [scope, setScope] = useState(readScope);
  const here = devices.selected ?? devices.def ?? null;
  const online = devices.multi ? devices.items.filter((d) => d.online).map((d) => d.id) : [null];
  const targets = !devices.multi ? [null] : scope === 'here' && here ? [here] : [here, ...online.filter((d) => d !== here)].filter((d) => d && online.includes(d));
  const sig = targets.join(',');
  const pickScope = (v) => {
    setScope(v);
    writeScope(v);
  };
  const update = (patch) =>
    setF((x) => {
      const next = { ...x, ...patch };
      writeFilters(next);
      return next;
    });
  const toggle = (field, key) => update({ [field]: f[field].includes(key) ? f[field].filter((x) => x !== key) : [...f[field], key] });
  const load = (dev, extra = '') =>
    deviceApi(dev)
      .get(`/history?limit=${PAGE}${extra}`)
      .then(
        (r) => ({ dev, items: Array.isArray(r?.items) ? r.items : [], marks: r?.marks ?? {}, persistent: Boolean(r?.persistent) }),
        (e) => ({ dev, items: [], marks: {}, error: e }),
      );
  // 每台电脑的结果到了就先显示，不等最慢的那台；换搜索词时旧结果留着，到了再换，列表不闪
  useEffect(() => {
    let alive = true;
    const extra = deepQ ? `&deep=true&q=${enc(deepQ)}` : '';
    setGroups((prev) => targets.map((dev) => ({ ...(prev?.find((g) => g.dev === dev) ?? { dev, items: [], marks: {} }), loading: true })));
    for (const dev of targets) {
      load(dev, extra).then((g) => alive && setGroups((gs) => (gs ?? []).map((x) => (x.dev === dev ? { ...g, more: g.items.length >= PAGE, loading: false } : x))));
    }
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, deepQ]);
  // 停下打字一会儿就在对话内容里搜（不用按回车）；清空时回到普通列表
  const words = parseQuery(f.q).text;
  useEffect(() => {
    if (words === deepQ) return undefined;
    const timer = setTimeout(() => setDeepQ(words.length >= 2 ? words : ''), 600);
    return () => clearTimeout(timer);
  }, [words]);
  useEffect(() => {
    const next = readFilters(search);
    if (next.q !== f.q) setF((x) => ({ ...x, q: next.q }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search]);
  const loadMore = async () => {
    if (!groups || loadingMore) return;
    setLoadingMore(true);
    const extra = deepQ ? `&deep=true&q=${enc(deepQ)}` : '';
    const next = await Promise.all(
      groups.map(async (g) => {
        if (!g.more || g.error) return g;
        const oldest = Math.min(...g.items.map(updatedMs));
        const r = await load(g.dev, `${extra}&before=${oldest}`);
        return { ...g, items: [...g.items, ...r.items], more: r.items.length >= PAGE };
      }),
    );
    setGroups(next);
    setLoadingMore(false);
  };
  const markOf = (c) => groups?.find((g) => g.dev === c.dev)?.marks?.[`${c.agent}:${c.id}`] ?? {};
  const setMark = async (c, patch) => {
    try {
      const r = await deviceApi(c.dev).put('/history/marks', { agent: c.agent, id: c.id, ...patch });
      setGroups((gs) => gs.map((g) => (g.dev === c.dev ? { ...g, marks: r?.marks ?? g.marks } : g)));
      if (patch.hide) toast(t('hist.hidden_done'), 'ok', { label: t('sess.undo'), onClick: () => setMark(c, { hide: false }) });
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };

  const now = Date.now();
  const all = (groups ?? []).filter((g) => g.persistent).flatMap((g) => g.items.map((c) => ({ ...c, dev: g.dev })));
  const loading = !groups || groups.some((g) => g.loading);
  const firstLoad = !groups || groups.every((g) => g.loading && g.items.length === 0);
  const open = (c) => {
    if (c.instance_id) ui.openSession(c.instance_id);
    else ui.continueConv(c.dev, c, Boolean(c.live && CAN_FORK.includes(c.agent))).catch((e) => toast(errorText(t, e), 'error'));
  };
  const pq = parseQuery(f.q);
  const devName = (id) => (devices.multi ? deviceName(t, devices.byId(id)) : '').toLowerCase();
  const textOk = (c) => {
    if (pq.agent.length && !pq.agent.some((a) => c.agent.includes(a))) return false;
    if (pq.dev.length && !pq.dev.some((d) => devName(c.dev).includes(d))) return false;
    if (pq.branch.length && !pq.branch.some((b) => (c.branch ?? '').toLowerCase().includes(b))) return false;
    if (!pq.text) return true;
    if (deepQ && c.snippet) return true;
    return [c.title, c.last, c.workdir, c.branch].filter(Boolean).join('\n').toLowerCase().includes(pq.text);
  };
  // 每一组筛选只看其他组的条件（数量是“再选上它会有多少”）
  const pass = {
    agents: (c) => !f.agents.length || f.agents.includes(c.agent),
    devs: (c) => !f.devs.length || f.devs.includes(c.dev ?? ''),
    projs: (c) => !f.projs.length || f.projs.includes(projKey(c)),
    states: (c) => !f.states.length || f.states.includes(stateOf(c)),
    branches: (c) => !f.branches.length || f.branches.includes(c.branch ?? ''),
    time: (c) => f.time === 'all' || updatedMs(c) >= now - TIMES[f.time] * DAY,
    mark: (c) => {
      const m = markOf(c);
      if (f.mark === 'star') return Boolean(m.star);
      if (f.mark === 'hidden') return Boolean(m.hide);
      return !m.hide;
    },
  };
  const base = all.filter(textOk);
  const except = (skip) => base.filter((c) => Object.entries(pass).every(([k, fn]) => k === skip || fn(c)));
  const list = except(null).sort((a, b) => updatedMs(b) - updatedMs(a));
  const countBy = (items, keyOf) => {
    const m = new Map();
    for (const c of items) m.set(keyOf(c), (m.get(keyOf(c)) ?? 0) + 1);
    return m;
  };
  const known = new Set(s.projects.map((p) => p.key));
  const agentItems = [...countBy(except('agents'), (c) => c.agent)].map(([key, n]) => ({ key, n, label: t(`launch.${key}.title`) }));
  const devItems = devices.multi ? [...countBy(except('devs'), (c) => c.dev ?? '')].map(([key, n]) => ({ key, n, label: deviceName(t, devices.byId(key)), dev: devices.byId(key) })) : [];
  const projItems = [...countBy(except('projs'), projKey)]
    .map(([key, n]) => {
      const [dev, workdir] = key.split('\n');
      return { key, n, label: baseName(workdir) || workdir, text: workdir, title: workdir, dev: dev || null, fresh: !known.has(key) };
    })
    .sort((a, b) => b.n - a.n);
  const stateItems = STATES.map((key) => ({ key, n: except('states').filter((c) => stateOf(c) === key).length, label: t(`hist.state_${key}`) })).filter((x) => x.n > 0);
  const branchItems = f.projs.length ? [...countBy(except('branches').filter((c) => c.branch), (c) => c.branch)].map(([key, n]) => ({ key, n, label: key })) : [];
  const timeCount = (k) => except('time').filter((c) => k === 'all' || updatedMs(c) >= now - TIMES[k] * DAY).length;
  const allMarks = all.filter(textOk).map(markOf);
  const starN = allMarks.filter((m) => m.star).length;
  const hiddenN = allMarks.filter((m) => m.hide).length;
  const active = f.agents.length + f.devs.length + f.projs.length + f.states.length + f.branches.length + (f.time !== 'all') + Boolean(f.mark);
  const clear = () => update({ agents: [], devs: [], projs: [], states: [], branches: [], time: 'all', mark: '' });

  // 分组
  const byGroup = new Map();
  for (const c of list) {
    const key = f.group === 'project' ? projKey(c) : f.group === 'device' ? c.dev ?? '' : timeBucket(updatedMs(c), now);
    if (!byGroup.has(key)) byGroup.set(key, []);
    byGroup.get(key).push(c);
  }
  const groupHead = (key) => {
    if (f.group === 'time') return t(`hist.when_${key}`);
    if (f.group === 'device') return devices.multi ? deviceName(t, devices.byId(key)) : t('hist.this_computer');
    const [, workdir] = key.split('\n');
    return baseName(workdir) || workdir;
  };
  const selected = sel ? list.find((c) => convKey(c) === sel) ?? all.find((c) => convKey(c) === sel) : null;
  // ↑↓ 切换上一条、下一条（焦点不在输入框里时）
  useEffect(() => {
    const on = (e) => {
      if (!['ArrowUp', 'ArrowDown', 'Enter'].includes(e.key) || e.target.closest?.('input, textarea, button, a, [contenteditable="true"]')) return;
      if (e.key === 'Enter') {
        if (selected) open(selected);
        return;
      }
      const at = list.findIndex((c) => convKey(c) === sel);
      const next = list[e.key === 'ArrowDown' ? at + 1 : Math.max(0, at - 1)];
      if (next) {
        e.preventDefault();
        setSel(convKey(next));
        document.querySelector(`[data-conv="${CSS.escape(convKey(next))}"]`)?.scrollIntoView({ block: 'nearest' });
      }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  });
  const failed = (groups ?? []).filter((g) => g.error);
  const noPersist = groups && !loading && groups.length > 0 && groups.every((g) => !g.error && !g.persistent);
  const anyMore = (groups ?? []).some((g) => g.more);
  const narrow = useNarrowHist();

  const filters = (
    <div className={`hfilters ${showFilters ? 'open' : ''}`}>
      <div className="hfilters-head">
        <b>{t('hist.filters')}</b>
        {active > 0 && (
          <button type="button" className="btn btn-plain btn-sm" onClick={clear}>
            {t('hist.clear')}
          </button>
        )}
      </div>
      <Facet title={t('hist.f_agent')} items={agentItems} selected={f.agents} onToggle={(k) => toggle('agents', k)} render={(x) => (<><LaunchIcon launch={x.key} size={15} /><span className="ellipsis grow">{x.label}</span></>)} />
      {devItems.length > 1 && (
        <Facet title={t('hist.f_device')} items={devItems} selected={f.devs} onToggle={(k) => toggle('devs', k)} render={(x) => (<>{x.dev && <DeviceAvatar device={x.dev} size={16} />}<span className="ellipsis grow">{x.label}</span></>)} />
      )}
      <Facet
        title={t('hist.f_project')}
        items={projItems}
        selected={f.projs}
        onToggle={(k) => toggle('projs', k)}
        search
        render={(x) => (
          <>
            <Folder size={14} aria-hidden="true" />
            <span className="ellipsis grow">{x.label}</span>
            {x.fresh && <span className="hfacet-tag">{t('hist.new_folder')}</span>}
          </>
        )}
      />
      {branchItems.length > 0 && <Facet title={t('hist.f_branch')} items={branchItems} selected={f.branches} onToggle={(k) => toggle('branches', k)} search render={(x) => (<><GitBranch size={14} aria-hidden="true" /><span className="ellipsis grow">{x.label}</span></>)} />}
      <Facet title={t('hist.f_state')} items={stateItems} selected={f.states} onToggle={(k) => toggle('states', k)} />
      <section className="hfacet">
        <h3 className="hfacet-title">{t('hist.f_time')}</h3>
        {['today', 'week', 'month', 'all'].map((k) => (
          <button key={k} type="button" className="hfacet-item" aria-pressed={f.time === k} onClick={() => update({ time: k })}>
            <span className="grow">{t(`hist.time_${k}`)}</span>
            <span className="hfacet-n">{timeCount(k)}</span>
          </button>
        ))}
      </section>
      {(starN > 0 || hiddenN > 0) && (
        <section className="hfacet">
          <h3 className="hfacet-title">{t('hist.f_mark')}</h3>
          {starN > 0 && (
            <button type="button" className="hfacet-item" aria-pressed={f.mark === 'star'} onClick={() => update({ mark: f.mark === 'star' ? '' : 'star' })}>
              <Star size={14} aria-hidden="true" />
              <span className="grow">{t('hist.starred')}</span>
              <span className="hfacet-n">{starN}</span>
            </button>
          )}
          {hiddenN > 0 && (
            <button type="button" className="hfacet-item" aria-pressed={f.mark === 'hidden'} onClick={() => update({ mark: f.mark === 'hidden' ? '' : 'hidden' })}>
              <EyeOff size={14} aria-hidden="true" />
              <span className="grow">{t('hist.show_hidden')}</span>
              <span className="hfacet-n">{hiddenN}</span>
            </button>
          )}
        </section>
      )}
    </div>
  );

  return (
    <div className={`hist-page ${selected && !narrow ? 'with-detail' : ''}`}>
      {filters}
      <div className="hist-main">
        <div className="page-head">
          <h1>{t('hist.title')}</h1>
        </div>
        <div className="hist-bar">
          <label className="slist-search hist-search">
            <Search size={15} aria-hidden="true" />
            <input
              type="search"
              value={f.q}
              onChange={(e) => {
                update({ q: e.target.value });
                if (!e.target.value.trim()) setDeepQ('');
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && pq.text) setDeepQ(pq.text);
                if (e.key === 'Escape') {
                  update({ q: '' });
                  setDeepQ('');
                }
              }}
              placeholder={t('hist.search2')}
              aria-label={t('hist.search2')}
              spellCheck={false}
              autoFocus={Boolean(f.q)}
            />
          </label>
          <button type="button" className={`btn btn-ghost btn-sm hist-filter-btn ${active ? 'on' : ''}`} onClick={() => setShowFilters(!showFilters)} aria-expanded={showFilters}>
            <SlidersHorizontal size={15} aria-hidden="true" />
            {t('hist.filters')}
            {active > 0 && <span className="hfacet-n">{active}</span>}
          </button>
          {devices.multi && (
            <span className="seg seg-xs" role="group" aria-label={t('hist.scope')}>
              {['here', 'all'].map((v) => (
                <button key={v} type="button" aria-pressed={scope === v} onClick={() => pickScope(v)} title={v === 'here' && here ? deviceName(t, devices.byId(here)) : undefined}>
                  {t(`hist.scope_${v}`)}
                </button>
              ))}
            </span>
          )}
          <span className="seg seg-xs" role="group" aria-label={t('hist.group_by')}>
            {GROUPS.filter((g) => g !== 'device' || devices.multi).map((g) => (
              <button key={g} type="button" aria-pressed={f.group === g} onClick={() => update({ group: g })}>
                {t(`hist.group_${g}`)}
              </button>
            ))}
          </span>
        </div>
        <p className="small muted hist-hint">
          {deepQ ? t('hist.deep_on', { q: deepQ }) : t('hist.lead2', { count: list.length })}
          {loading && !firstLoad && (
            <>
              {' · '}
              <Spinner size={12} /> {t('hist.loading_more_devices')}
            </>
          )}
        </p>
        {failed.map((g) => (
          <ErrorNote key={g.dev ?? ''} error={g.error} />
        ))}
        {firstLoad ? (
          <div className="hist-loading">
            <Spinner size={18} />
          </div>
        ) : noPersist ? (
          <p className="muted">{t('hist.needs_persistent')}</p>
        ) : list.length === 0 ? (
          <div className="conv-empty">
            <p>{f.q || active ? t('hist.no_match') : t('hist.empty')}</p>
            {active > 0 && (
              <button type="button" className="btn btn-plain btn-sm" onClick={clear}>
                {t('hist.clear')}
              </button>
            )}
          </div>
        ) : (
          [...byGroup].map(([key, items]) => (
            <section key={key} className="hist-group" aria-label={groupHead(key)}>
              <h2 className="hist-group-head">
                {f.group === 'project' && <Folder size={15} aria-hidden="true" />}
                <span>{groupHead(key)}</span>
                <span className="hfacet-n">{items.length}</span>
                {f.group === 'project' && <span className="hist-path ellipsis">{key.split('\n')[1]}</span>}
              </h2>
              <div className="conv-list">
                {items.map((c) => {
                  const k = convKey(c);
                  const on = sel === k;
                  return (
                    <div key={k} data-conv={k}>
                      <HistRow
                        conv={c}
                        selected={on}
                        mark={markOf(c)}
                        onSelect={() => setSel(on ? null : k)}
                        onOpen={() => open(c)}
                        onMark={(patch) => setMark(c, patch)}
                        showDevice={f.group !== 'device'}
                        showProject={f.group !== 'project'}
                      />
                      {on && narrow && <ConvPeek device={c.dev} conv={c} />}
                    </div>
                  );
                })}
              </div>
            </section>
          ))
        )}
        {groups && anyMore && (
          <AutoMore onMore={loadMore} busy={loadingMore}>
            <button type="button" className="btn btn-ghost btn-sm hist-more" onClick={loadMore} disabled={loadingMore}>
              {loadingMore ? <Spinner size={14} /> : <ChevronDown size={14} aria-hidden="true" />}
              {t('hist.load_more')}
            </button>
          </AutoMore>
        )}
      </div>
      {selected && !narrow && <HistDetail key={sel} conv={selected} onClose={() => setSel(null)} />}
    </div>
  );
}
