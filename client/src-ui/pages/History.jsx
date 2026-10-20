// 以前的对话：电脑上 Claude Code、Codex、OpenCode 自己存的对话（客户端 src/history.rs），点一下在看看里接着聊。
// 两个地方用：开始页里跟着当前项目的一节（PrevConversations），和“所有以前的对话”页面（/history）。
// 点一行先展开看最后几句，再点“继续”；正开在看看会话里的直接打开那个会话；
// 可能正开在别处（文件两分钟内还在变）的默认“复制一份继续”，原来那份不动。
import { AlertCircle, ChevronDown, Copy, ExternalLink, Folder, GitBranch, History, Play, Search } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi } from '../shared/api.js';
import { DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { useRouter } from '../shared/router.jsx';
import { baseName, useSessions } from '../shared/sessions.jsx';
import { relativeTime } from '../shared/time.js';
import { ErrorNote, Spinner, useToast } from '../shared/ui.jsx';
import { useSessionUi } from './Sessions.jsx';

const AGENTS = ['claude', 'codex', 'opencode'];
// 能“复制一份继续”的（OpenCode 没有这个功能）
const CAN_FORK = ['claude', 'codex'];
// 开始页里先列几个
const START_SHOW = 5;
// 页面里每个文件夹先列几个
const GROUP_SHOW = 5;

const enc = encodeURIComponent;

/** 展开后的一行：最后几句和按钮。 */
function ConvPeek({ device, conv }) {
  const { t } = useTranslation();
  const ui = useSessionUi();
  const toast = useToast();
  const [msgs, setMsgs] = useState(null);
  const [busy, setBusy] = useState(false);
  const first = useRef(null);
  useEffect(() => {
    let alive = true;
    deviceApi(device)
      .get(`/history/${conv.agent}/${enc(conv.id)}`)
      .then(
        (r) => alive && setMsgs(Array.isArray(r?.items) ? r.items : []),
        () => alive && setMsgs([]),
      );
    first.current?.focus();
    return () => {
      alive = false;
    };
  }, [device, conv.agent, conv.id]);
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
  const tool = t(`launch.${conv.agent}.title`);
  const fork = conv.live && CAN_FORK.includes(conv.agent);
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
          <button ref={first} type="button" className="btn btn-primary btn-sm" onClick={() => go(false)} disabled={busy}>
            {busy ? <Spinner size={15} /> : <Play size={15} aria-hidden="true" />}
            {t('hist.continue')}
          </button>
        )}
      </div>
    </div>
  );
}

function ConvRow({ device, conv, open, onToggle }) {
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
      {open && <ConvPeek device={device} conv={conv} />}
    </div>
  );
}

function AgentChips({ items, value, onChange }) {
  const { t } = useTranslation();
  const count = (a) => items.filter((c) => a === 'all' || c.agent === a).length;
  const shown = AGENTS.filter((a) => count(a) > 0);
  if (shown.length < 2) return null;
  return (
    <span className="seg seg-xs" role="group" aria-label={t('hist.by_tool')}>
      {['all', ...shown].map((a) => (
        <button key={a} type="button" aria-pressed={value === a} onClick={() => onChange(a)}>
          {a === 'all' ? t('hist.all') : t(`launch.${a}.title`)} {count(a)}
        </button>
      ))}
    </span>
  );
}

/**
 * 开始页里：当前项目里以前的对话。选了 AI 类型时只看那个工具的（也能切到“全部”），
 * 没有对话、普通终端、电脑不支持会话保持时不显示。
 */
export function PrevConversations({ device, workdir, launch, focus }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const [data, setData] = useState(null);
  const [agent, setAgent] = useState(AGENTS.includes(launch) && !focus ? launch : 'all');
  const [open, setOpen] = useState(null);
  const [more, setMore] = useState(false);
  const box = useRef(null);
  // 换了类型才跟着换（从项目菜单进来时先看全部）
  const lastLaunch = useRef(launch);
  useEffect(() => {
    if (lastLaunch.current === launch) return;
    lastLaunch.current = launch;
    if (AGENTS.includes(launch)) setAgent(launch);
    setOpen(null);
  }, [launch]);
  useEffect(() => {
    let alive = true;
    setData(null);
    setOpen(null);
    setMore(false);
    if (!workdir) return undefined;
    deviceApi(device)
      .get(`/history?workdir=${enc(workdir)}&limit=50`)
      .then(
        (r) => alive && setData(r),
        // 旧版客户端没有这个接口
        () => alive && setData({ items: [] }),
      );
    return () => {
      alive = false;
    };
  }, [device, workdir]);
  useEffect(() => {
    if (focus && data?.items?.length) box.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [focus, data]);
  const all = Array.isArray(data?.items) ? data.items : [];
  if (!data?.persistent || all.length === 0 || !AGENTS.includes(launch)) return null;
  const list = all.filter((c) => agent === 'all' || c.agent === agent);
  const shown = list.slice(0, more ? 20 : START_SHOW);
  return (
    <section ref={box} className="prev" aria-label={t('hist.title')}>
      <div className="prev-head">
        <h2 className="prev-title">
          <History size={14} aria-hidden="true" />
          {t('hist.in_project', { name: baseName(workdir) })}
        </h2>
        <AgentChips items={all} value={agent} onChange={setAgent} />
      </div>
      <div className="conv-list">
        {shown.map((c) => (
          <ConvRow key={`${c.agent}:${c.id}`} device={device} conv={c} open={open === c.id} onToggle={() => setOpen(open === c.id ? null : c.id)} />
        ))}
        {shown.length === 0 && <p className="conv-empty">{t('hist.none_for', { tool: t(`launch.${agent}.title`) })}</p>}
      </div>
      <div className="prev-foot">
        {!more && list.length > START_SHOW && (
          <button type="button" className="btn btn-plain btn-sm" onClick={() => setMore(true)}>
            <ChevronDown size={14} aria-hidden="true" />
            {t('hist.more', { count: Math.min(list.length, 20) - START_SHOW })}
          </button>
        )}
        <span className="grow" />
        <button type="button" className="btn btn-plain btn-sm prev-all" onClick={() => navigate('/history')}>
          {t('hist.all_link')}
        </button>
      </div>
    </section>
  );
}

/** 所有以前的对话：所有在线电脑、所有文件夹，按文件夹分组，可以搜。 */
export function HistoryPage() {
  const { t } = useTranslation();
  const { search } = useRouter();
  const devices = useDevices();
  const s = useSessions();
  const [query, setQuery] = useState(() => new URLSearchParams(search).get('q') ?? '');
  const [agent, setAgent] = useState('all');
  const [open, setOpen] = useState(null);
  const [expanded, setExpanded] = useState({});
  const [groups, setGroups] = useState(null);
  const targets = devices.multi ? devices.items.filter((d) => d.online).map((d) => d.id) : [null];
  const sig = targets.join(',');
  useEffect(() => {
    let alive = true;
    setGroups(null);
    Promise.all(
      targets.map((dev) =>
        deviceApi(dev)
          .get('/history?limit=500')
          .then(
            (r) => ({ dev, items: Array.isArray(r?.items) ? r.items : [], persistent: Boolean(r?.persistent) }),
            (e) => ({ dev, items: [], error: e }),
          ),
      ),
    ).then((r) => alive && setGroups(r));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig]);
  useEffect(() => setQuery(new URLSearchParams(search).get('q') ?? ''), [search]);
  const q = query.trim().toLowerCase();
  const all = (groups ?? []).filter((g) => g.persistent).flatMap((g) => g.items.map((c) => ({ ...c, dev: g.dev })));
  const list = all.filter((c) => (agent === 'all' || c.agent === agent) && (!q || `${c.title}\n${c.last ?? ''}\n${c.workdir}`.toLowerCase().includes(q)));
  // 按 电脑 + 文件夹 分组，最近有对话的文件夹排前面（列表本来就按时间排好了）
  const byDir = new Map();
  for (const c of list) {
    const key = `${c.dev ?? ''}\n${c.workdir}`;
    if (!byDir.has(key)) byDir.set(key, { key, dev: c.dev, workdir: c.workdir, items: [] });
    byDir.get(key).items.push(c);
  }
  const known = new Set(s.projects.map((p) => p.key));
  const failed = (groups ?? []).filter((g) => g.error);
  const noPersist = groups && groups.length > 0 && groups.every((g) => !g.error && !g.persistent);
  return (
    <div className="hist-page">
      <div className="page-head">
        <h1>{t('hist.title')}</h1>
      </div>
      <p className="muted hist-lead">{t('hist.lead')}</p>
      <div className="hist-bar">
        <label className="slist-search hist-search">
          <Search size={15} aria-hidden="true" />
          <input type="search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('hist.search')} aria-label={t('hist.search')} spellCheck={false} autoFocus={Boolean(query)} />
        </label>
        <AgentChips items={all} value={agent} onChange={setAgent} />
      </div>
      {failed.map((g) => (
        <ErrorNote key={g.dev ?? ''} error={g.error} />
      ))}
      {!groups ? (
        <div className="hist-loading">
          <Spinner size={18} />
        </div>
      ) : noPersist ? (
        <p className="muted">{t('hist.needs_persistent')}</p>
      ) : byDir.size === 0 ? (
        <p className="muted">{q ? t('hist.no_match') : t('hist.empty')}</p>
      ) : (
        [...byDir.values()].map((g) => {
          const dev = devices.multi ? devices.byId(g.dev) : null;
          const more = expanded[g.key] || q;
          const shown = more ? g.items : g.items.slice(0, GROUP_SHOW);
          return (
            <section key={g.key} className="hist-group" aria-label={baseName(g.workdir)}>
              <h2 className="hist-group-head">
                <Folder size={15} aria-hidden="true" />
                <span>{baseName(g.workdir) || g.workdir}</span>
                {dev && <DeviceTag device={dev} />}
                {!known.has(`${g.dev ?? ''}\n${g.workdir}`) && <span className="badge">{t('hist.new_folder')}</span>}
                <span className="hist-path ellipsis">{g.workdir}</span>
              </h2>
              <div className="conv-list">
                {shown.map((c) => {
                  const id = `${g.key}\n${c.agent}:${c.id}`;
                  return <ConvRow key={id} device={g.dev} conv={c} open={open === id} onToggle={() => setOpen(open === id ? null : id)} />;
                })}
              </div>
              {!more && g.items.length > GROUP_SHOW && (
                <button type="button" className="btn btn-plain btn-sm" onClick={() => setExpanded((x) => ({ ...x, [g.key]: true }))}>
                  <ChevronDown size={14} aria-hidden="true" />
                  {t('hist.more', { count: g.items.length - GROUP_SHOW })}
                </button>
              )}
            </section>
          );
        })
      )}
    </div>
  );
}
