// 会话（首页）：左侧栏里的项目和会话列表、开始页（新会话）、右边打开的会话。
// 像 ChatGPT：点“新会话”进开始页，选好类型和项目、写一句话（可以不写），回车就启动并打开；项目上的 + 一下就新建。
// 项目 = 某台电脑上的一个文件夹（见 shared/sessions.jsx）。电脑用颜色 + 系统图标区分（shared/brands.jsx）。
// 电脑上会话在右边打开，最近打开的几个在后台保持连接；手机上整页打开 /t/{id}（有按键条）。
import { AlertCircle, ArrowLeft, ArrowUp, ChevronDown, Container, X, ChevronsDownUp, ChevronsUpDown, ExternalLink, Folder, FolderOpen, FolderPlus, History, Plus, Search, ShieldCheck, SlidersHorizontal, Trash2 } from 'lucide-react';
import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { SafetyNote, useStatus } from '../App.jsx';
import { deviceApi, rememberTerminals, terminalDevice } from '../shared/api.js';
import { DeviceAvatar, deviceColor, DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, osLabel, RenameDeviceDialog, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { linkFailure, linkLabel, linkState, useLinks } from '../shared/paths.jsx';
import { isTouch } from '../shared/KeyBar.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { useRouter } from '../shared/router.jsx';
import { remainingOf, useAccountsUsage, usageAgent, Bar as UsageBar } from '../shared/StatusBar.jsx';
import { baseName, customName, lastUsed, sessionDir, sessionState, sessionTitle, startedAt, useSessions } from '../shared/sessions.jsx';
import { spanText } from '../shared/time.js';
import { Button, ErrorNote, Modal, Spinner, Switch, useLoad, useToast } from '../shared/ui.jsx';
import { ClaudeProfilePick } from './ClaudeProfiles.jsx';
import { PrevConversations } from './History.jsx';
import { BackendNote, FolderPicker, InstallProgress, InstanceDialog, LogsDialog, openDsh, Promotions, RemoteCard, ShellPicker, useInstanceActions } from './Terminals.jsx';
import { TermPane } from './TerminalView.jsx';

// 开始页能选的类型（DSH 一台电脑只有一个，已经有了就是打开它）
const START_LAUNCHES = ['claude', 'codex', 'codex_term', 'opencode', 'shell', 'dsh', 'custom'];
// 开始页里 Codex 分成两种：“Codex 对话”（看看的对话页）和“Codex 终端”（只有终端，和自己电脑上一样），开始后不能互换。
// codex_term 只是开始页的叫法，建的还是 launch=codex、chat_ui=false 的会话
const CODEX_TERM = 'codex_term';
const baseLaunch = (l) => (l === CODEX_TERM ? 'codex' : l);
/** 项目记住的类型（Codex 另记是不是对话） */
const projectLaunch = (p) => (p?.launch === 'codex' && p.codex_chat === false ? CODEX_TERM : p?.launch);
// 项目“⋯”里换个类型新建
const QUICK_LAUNCHES = ['claude', 'codex', 'opencode', 'shell'];
const AI_TOOLS = ['codex', 'claude', 'opencode', 'dsh'];
const FULL_ACCESS = ['codex', 'claude', 'opencode'];

/**
 * 新会话默认带不带全部权限（各人习惯不同）：这个项目里最近一个 AI 会话怎么开的就怎么开；项目里没有 AI 会话时，
 * 用在这个项目里新建时选过的；都没有用那台电脑的设置“新 AI 会话默认全部权限”。
 */
export function fullAccessFor(p, settings) {
  const last = (p?.items ?? []).filter((i) => FULL_ACCESS.includes(i.launch)).sort((a, b) => startedAt(b) - startedAt(a))[0];
  if (last) return Boolean(last.full_access);
  if (typeof p?.full_access === 'boolean') return p.full_access;
  return Boolean(settings?.default_full_access);
}
// 第一句话能用在哪些类型上（AI 工具作为第一条消息，普通终端当作启动后运行的命令）
const PROMPT_LAUNCHES = ['claude', 'codex', CODEX_TERM, 'opencode', 'shell'];
// 开始页直接摆出来的类型个数，其余收进“更多”；没用过时先摆这几个
const SHOWN_TYPES = 4;
const DEFAULT_TYPES = ['claude', 'codex', CODEX_TERM, 'shell'];

/**
 * 开始页摆出来的类型：当前选中的 + 用得最多的（按各台电脑上的会话数），不够的用默认的补上，其余收进“更多”。
 * 顺序按 START_LAUNCHES 固定，不随选中的变来变去。
 */
export function shownTypes(launches, items, current) {
  const count = new Map();
  for (const i of items) {
    const l = i.launch === 'codex' && i.chat_ui === false ? CODEX_TERM : i.launch;
    count.set(l, (count.get(l) ?? 0) + 1);
  }
  const ranked = [...launches].sort((a, b) => (count.get(b) ?? 0) - (count.get(a) ?? 0) || DEFAULT_TYPES.indexOf(a) - DEFAULT_TYPES.indexOf(b));
  const pick = new Set(launches.includes(current) ? [current] : []);
  for (const l of ranked) if (pick.size < SHOWN_TYPES && ((count.get(l) ?? 0) > 0 || DEFAULT_TYPES.includes(l))) pick.add(l);
  for (const l of DEFAULT_TYPES) if (pick.size < SHOWN_TYPES && launches.includes(l)) pick.add(l);
  return { shown: launches.filter((l) => pick.has(l)), more: launches.filter((l) => !pick.has(l)) };
}

const FOLD_KEY = 'looklook.fold';
const GROUP_KEY = 'looklook.groupby';
const DEVF_KEY = 'looklook.devfilter';
const QUERY_KEY = 'looklook.query';

function readStore(storage, key, fallback) {
  try {
    return JSON.parse(storage.getItem(key) ?? 'null') ?? fallback;
  } catch {
    return fallback;
  }
}
function writeStore(storage, key, value) {
  try {
    storage.setItem(key, JSON.stringify(value));
  } catch {
    /* 无痕模式等：只是不记住 */
  }
}

/** 会话在本页打开（电脑）还是整页打开（手机、平板，有按键条）。 */
export const opensInPage = () => !isTouch();

// ---------------- 会话页的公共操作：打开、新建、编辑、运行记录 ----------------

const UiCtx = createContext(null);

export function useSessionUi() {
  return useContext(UiCtx);
}

export function SessionUiProvider({ children }) {
  const { t } = useTranslation();
  const s = useSessions();
  const devices = useDevices();
  const { navigate, path } = useRouter();
  const { status } = useStatus();
  const toast = useToast();
  const [dialog, setDialog] = useState(null);
  const [logs, setLogs] = useState(null);
  // 开始页要预先填好的（在某个项目里点“更多选项”时）；nonce 让开始页重新初始化
  const [start, setStart] = useState({ nonce: 0 });
  const openSession = (id) => {
    if (!opensInPage()) return navigate(`/t/${id}`);
    s.openTab(id);
    s.openRecent(id);
    s.requestFocus(id);
    if (path !== `/s/${id}`) navigate(`/s/${id}`);
  };
  const newSession = (want = {}) => {
    setStart((x) => ({ ...want, nonce: x.nonce + 1 }));
    navigate(opensInPage() ? '/' : '/new');
  };
  /** 那台电脑的设置：顶栏选中的那台用已有的状态，别的电脑现取。 */
  const settingsOf = async (device) =>
    devices.multi && device && device !== devices.selected ? ((await deviceApi(device).get('/status').catch(() => null))?.settings ?? null) : status.settings;
  /** 在项目里直接新建一个（类型默认用这个项目上次用的，全部权限见 [`fullAccessFor`]），启动并打开；提示条里可以撤销。 */
  const quickCreate = async (p, launch = p.launch || 'claude') => {
    const client = deviceApi(p.device);
    try {
      const tools = AI_TOOLS.includes(launch) && !p.container ? await client.get('/tools').catch(() => null) : null;
      if (tools && !tools.tools?.[launch]) {
        newSession({ device: p.device, workdir: p.workdir, launch });
        return;
      }
      const decided = p.items.some((i) => FULL_ACCESS.includes(i.launch)) || typeof p.full_access === 'boolean';
      const full = FULL_ACCESS.includes(launch) && fullAccessFor(p, decided ? null : await settingsOf(p.device));
      const r = await client.post('/instances', {
        name: '',
        workdir: p.container ? '' : p.workdir,
        container: p.container ? { name: p.container.name, dir: p.container.dir } : undefined,
        launch,
        command: '',
        shell: p.shell ?? '',
        auto_start: Boolean(p.auto_start),
        full_access: full,
        start: true,
      });
      if (p.device && r.id) rememberTerminals(p.device, [r]);
      if (!p.container) s.touchProject(p.device, p.workdir, { launch });
      s.reload();
      if (launch === 'dsh') openDsh(r, t, toast);
      else openSession(r.id);
      toast(t('sess.quick_done', { name: r.name }), 'ok', {
        label: t('sess.undo'),
        onClick: async () => {
          await client.del(`/instances/${r.id}`).catch(() => {});
          s.closeRecent(r.id);
          s.reload();
          if (window.location.pathname === `/s/${r.id}`) navigate('/');
        },
      });
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  /**
   * 接着以前的对话（History.jsx）：已经开在某个看看会话里的直接打开；否则新建一个会话接着它，
   * 名字用对话标题，全部权限等选项用这个项目上次的。`fork` 为复制一份接着，原来的不动。
   */
  const continueConv = async (device, c, fork = false) => {
    if (c.instance_id && !fork) {
      openSession(c.instance_id);
      return;
    }
    const p = s.projects.find((x) => x.device === device && x.workdir === c.workdir);
    const r = await deviceApi(device).post('/instances', {
      name: c.title.replace(/…$/, '').slice(0, 40),
      workdir: c.workdir,
      launch: c.agent,
      command: '',
      shell: p?.shell ?? '',
      auto_start: Boolean(p?.auto_start),
      full_access: FULL_ACCESS.includes(c.agent) && fullAccessFor(p, await settingsOf(device)),
      resume: { id: c.id, fork },
      start: true,
    });
    if (device && r.id) rememberTerminals(device, [r]);
    s.touchProject(device, c.workdir, { launch: c.agent });
    s.reload();
    openSession(r.id);
    toast(t(fork ? 'hist.forked' : 'hist.continued', { name: r.name }), 'ok');
  };
  const value = {
    openSession,
    newSession,
    continueConv,
    quickCreate,
    start,
    edit: (inst) => setDialog({ initial: inst }),
    install: (preset, device, workdir) => setDialog({ preset, device, workdir }),
    logs: setLogs,
  };
  return (
    <UiCtx.Provider value={value}>
      {children}
      <InstanceDialog
        open={Boolean(dialog)}
        initial={dialog?.initial}
        preset={dialog?.preset}
        workdir={dialog?.workdir}
        device={dialog?.device ?? undefined}
        onClose={() => setDialog(null)}
        onSaved={(row) => {
          s.reload();
          if (row) openSession(row.id);
        }}
      />
      <LogsDialog inst={logs} onClose={() => setLogs(null)} />
    </UiCtx.Provider>
  );
}

/**
 * 鼠标按下就切换，不等 click：macOS App（WKWebView）里触控板“轻点”时按下和松开几乎同时到达，焦点正从终端 iframe
 * 回到管理台，click 有时被吞掉（要点两下或用力按）。按下时已切过的，随后的 click 不再切；键盘回车只有 click，照常切换。
 * 按下时不让按钮拿走焦点，好把焦点交给会话的输入框。
 */
export function usePressActivate(fn, disabled) {
  const downAt = useRef(0);
  return {
    onPointerDown: (e) => {
      if (e.pointerType !== 'mouse' || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || disabled) return;
      downAt.current = Date.now();
      fn();
    },
    onMouseDown: (e) => e.button === 0 && e.preventDefault(),
    onClick: () => {
      if (Date.now() - downAt.current < 1500) return;
      fn();
    },
  };
}

/** 一个会话行 / 会话顶栏的操作（打开、更多菜单）。 */
function useActions(inst) {
  const s = useSessions();
  const ui = useSessionUi();
  return useInstanceActions(inst, { onChanged: s.reload, onEdit: ui.edit, onLogs: ui.logs, onOpenHere: ui.openSession });
}

// ---------------- 侧栏：项目和会话 ----------------

/** 侧栏里的时间写短一点：刚刚、5 分钟、2 小时、3 天。 */
function shortAgo(t, ms) {
  const m = Math.max(0, Math.round((Date.now() - ms) / 60000));
  if (m < 1) return t('sess.ago.now');
  if (m < 60) return t('sess.ago.m', { n: m });
  if (m < 1440) return t('sess.ago.h', { n: Math.round(m / 60) });
  return t('sess.ago.d', { n: Math.round(m / 1440) });
}

function StateDot({ state, title }) {
  const { t } = useTranslation();
  const label = title ?? t(`sess.state.${state}`);
  return <span className={`sdot sdot-${state}`} title={label} aria-label={label} role="img" />;
}

function SessionRow({ inst, current, attn }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const s = useSessions();
  const a = useActions(inst);
  // 启动失败：红点，悬停看原因
  const state = inst.error && !inst.running ? 'error' : sessionState(inst);
  const errorTitle = state === 'error' ? t(`instance.error.${inst.error}`, { defaultValue: t('instance.error.other') }) : undefined;
  const p = attn ? s.projectOf(inst) : null;
  const dev = attn && devices.multi ? devices.byId(terminalDevice(inst.id)) : null;
  const when = lastUsed(inst);
  const press = usePressActivate(a.primary, a.busy || !a.allowed);
  const title = sessionTitle(inst);
  const mine = customName(inst);
  const dir = sessionDir(inst);
  const tip = [title, mine && t('sess.my_name', { name: mine }), inst.container ? `${inst.container.name}:${dir}` : dir].filter(Boolean).join('\n');
  return (
    <div className={`srow ${attn ? 'srow-attn' : ''} ${current ? 'current' : ''}`}>
      <button type="button" className="srow-main" {...press} disabled={a.busy || !a.allowed} aria-current={current ? 'page' : undefined} title={tip}>
        <LaunchIcon launch={inst.launch} size={16} className="srow-icon" />
        <span className="srow-text">
          <span className="srow-title">
            <span className="ellipsis">{title}</span>
            {/* Codex 有对话、终端两种会话，标出来（Claude Code 两边都有，不用标） */}
            {inst.launch === 'codex' && <span className="srow-kind">{t(inst.chat_ui === false ? 'sess.kind_term' : 'sess.kind_chat')}</span>}
          </span>
          {attn && (
            <span className="srow-sub">
              <span className="ellipsis">{p?.name ?? baseName(dir)}</span>
              {dev && <DeviceTag device={dev} />}
            </span>
          )}
        </span>
        {a.busy ? (
          <Spinner size={13} />
        ) : attn ? (
          <span className="badge badge-warn srow-pill">{t(inst.chat?.pending ? `chat.badge_${inst.chat.pending}` : 'chat.badge_tui')}</span>
        ) : (
          <>
            <StateDot state={state} title={errorTitle} />
            {when > 0 && <span className="srow-when">{shortAgo(t, when)}</span>}
          </>
        )}
      </button>
      <PopMenu items={a.menu(true)} className="btn btn-plain btn-icon srow-more" />
    </div>
  );
}

function ProjectRow({ p, open, onToggle, showDevice }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const dev = devices.multi ? devices.byId(p.device) : null;
  const attn = !open && p.items.some((i) => sessionState(i) === 'attn');
  const allowed = status.account.allowed && !p.offline;
  const launchName = t(`launch.${p.launch}.title`);
  const menu = [
    ...QUICK_LAUNCHES.map((l) => ({ icon: () => <LaunchIcon launch={l} size={16} />, label: t('sess.new_type', { type: t(`launch.${l}.title`) }), onClick: () => ui.quickCreate(p, l), disabled: !allowed })),
    { icon: History, label: t('hist.menu'), onClick: () => ui.newSession({ device: p.device, workdir: p.workdir, launch: AI_TOOLS.includes(p.launch) && p.launch !== 'dsh' ? p.launch : 'claude', prev: true }), disabled: !allowed },
    { icon: SlidersHorizontal, label: t('sess.more_options'), onClick: () => ui.newSession({ device: p.device, workdir: p.workdir }), disabled: !allowed },
    p.items.length === 0 && { icon: Trash2, label: t('sess.remove_project'), onClick: () => s.removeProject(p.device, p.workdir), danger: true },
  ];
  return (
    <div className={`prow ${p.offline ? 'offline' : ''}`} style={dev ? { '--dc': deviceColor(devices, dev.id) } : undefined}>
      <button type="button" className="prow-main" onClick={onToggle} aria-expanded={open} title={`${p.container ? `${p.container.name}:${p.container.dir}` : p.workdir}${dev ? ` · ${deviceName(t, dev)}` : ''}`}>
        <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
        {p.container ? <Container size={15} className="prow-folder" aria-label={t('sess.in_container', { name: p.container.name })} /> : <Folder size={15} className="prow-folder" aria-hidden="true" />}
        <span className="ellipsis grow">{p.name}</span>
        {attn && <span className="sdot sdot-attn" aria-label={t('sess.state.attn')} role="img" />}
        {showDevice && dev && <DeviceTag device={dev} />}
        {p.offline && <span className="badge">{t('devices.offline')}</span>}
      </button>
      {!p.offline && (
        <span className="prow-acts">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => ui.quickCreate(p)} disabled={!allowed} aria-label={t('sess.new_type_in', { type: launchName, name: p.name })} title={t('sess.new_type', { type: launchName })}>
            <Plus size={15} />
          </button>
          <PopMenu items={menu} className="btn btn-plain btn-icon" label={t('sess.project_menu')} />
        </span>
      )}
    </div>
  );
}

function DeviceHead({ device, count, open, onToggle }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const [renaming, setRenaming] = useState(false);
  const links = useLinks();
  const state = linkState(device, links);
  const failure = linkFailure(device, links);
  return (
    <>
    <div className="dhead">
      <button type="button" className="dhead-main" onClick={onToggle} aria-expanded={open}>
        <DeviceAvatar device={device} size={26} dot />
        <span className="dhead-text">
          <b className="ellipsis">{deviceName(t, device)}</b>
          <span className="small muted">
            {[osLabel(device.os), state === 'online' ? t('devices.count', { count }) : linkLabel(t, device, links)].filter(Boolean).join(' · ')}
          </span>
        </span>
        <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
      </button>
      {device.online && <PopMenu items={[{ icon: SlidersHorizontal, label: t('devices.rename'), onClick: () => setRenaming(true) }]} className="btn btn-plain btn-icon dhead-more" />}
      {renaming && (
        <RenameDeviceDialog
          device={device}
          onClose={() => setRenaming(false)}
          onDone={() => {
            setRenaming(false);
            setTimeout(() => devices.reload?.(), 1500);
          }}
        />
      )}
    </div>
    {failure && (
      <p className="slist-note">
        {t(`error:${failure.reason}`, { defaultValue: t('devices.unreachable_note') })}{' '}
        <button type="button" className="slist-retry" onClick={failure.retry}>
          {t('devices.retry')}
        </button>
      </p>
    )}
    </>
  );
}

/** 搜索：名称、文件夹、类型、命令都算。 */
function matcher(q, t) {
  const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return null;
  return (i) => {
    const hay = [i.agent_title, i.name, i.workdir, i.container?.name, i.container?.dir, i.command, i.launch, t(`launch.${i.launch}.title`)].filter(Boolean).join(' ').toLowerCase();
    return words.every((w) => hay.includes(w));
  };
}

/** 项目和会话列表（左侧栏、手机首页共用）。 */
export function SessionList({ currentId }) {
  const links = useLinks();
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const devices = useDevices();
  const s = useSessions();
  // 找会话改在命令面板里（侧栏上面的“跳转、命令…”）；以前存下的搜索词不再生效，免得列表被看不见的条件筛掉
  const [query, setQueryState] = useState('');
  const setQuery = (q) => {
    setQueryState(q);
    writeStore(sessionStorage, QUERY_KEY, q);
  };
  const [groupBy, setGroupByState] = useState(() => (readStore(localStorage, GROUP_KEY, 'project') === 'device' ? 'device' : 'project'));
  const setGroupBy = (v) => {
    setGroupByState(v);
    writeStore(localStorage, GROUP_KEY, v);
  };
  const [devFilter, setDevFilterState] = useState(() => readStore(localStorage, DEVF_KEY, 'all'));
  const setDevFilter = (v) => {
    setDevFilterState(v);
    writeStore(localStorage, DEVF_KEY, v);
  };
  // 折叠：用户点过的记住；没点过的，最近一周用过的项目展开，其他收起
  const [fold, setFoldState] = useState(() => readStore(localStorage, FOLD_KEY, {}));
  const setFold = (key, open) =>
    setFoldState((f) => {
      const next = { ...f, [key]: open };
      writeStore(localStorage, FOLD_KEY, next);
      return next;
    });
  const weekAgo = Date.now() - 7 * 86400e3;
  const isOpen = (p) => fold[p.key] ?? (p.used > weekAgo || p.items.some((i) => i.id === currentId));
  const match = matcher(query, t);
  const multi = devices.multi;
  const filter = multi && devices.items.some((d) => d.id === devFilter) ? devFilter : 'all';
  const devOk = (dev) => filter === 'all' || dev === filter;
  const projects = s.projects
    .filter((p) => devOk(p.device))
    .map((p) => (match ? { ...p, items: p.items.filter(match) } : p))
    .filter((p) => !match || p.items.length > 0 || p.name.toLowerCase().includes(query.trim().toLowerCase()));
  const attn = s.items.filter((i) => sessionState(i) === 'attn' && devOk(multi ? terminalDevice(i.id) : null) && (!match || match(i)));
  const allOpen = projects.some(isOpen);
  const foldAll = () =>
    setFoldState(() => {
      const next = Object.fromEntries(s.projects.map((p) => [p.key, !allOpen]));
      writeStore(localStorage, FOLD_KEY, next);
      return next;
    });
  const renderProject = (p, showDevice) => {
    const open = match ? true : isOpen(p);
    return (
      <section key={p.key} className="sproject" aria-label={p.name}>
        <ProjectRow p={p} open={open} onToggle={() => setFold(p.key, !open)} showDevice={showDevice} />
        {open &&
          (p.offline ? (
            <p className="slist-note">{t('sess.offline_note', { count: p.count ?? 0 })}</p>
          ) : p.items.length === 0 ? null : (
            p.items.map((i) => <SessionRow key={i.id} inst={i} current={i.id === currentId} />)
          ))}
      </section>
    );
  };
  let body;
  if (s.loading) {
    body = <div className="slist-loading"><Spinner size={16} /></div>;
  } else if (groupBy === 'device' && multi) {
    body = devices.items
      .filter((d) => devOk(d.id))
      .map((d) => {
        const ps = projects.filter((p) => p.device === d.id);
        const key = `dev:${d.id}`;
        const open = fold[key] ?? true;
        const group = s.groups?.[d.id];
        return (
          <section key={d.id} className="sdevice">
            <DeviceHead device={d} count={ps.reduce((n, p) => n + p.items.length, 0)} open={open} onToggle={() => setFold(key, !open)} />
            {open && group?.error && <ErrorNote error={group.error} />}
            {open && ps.map((p) => renderProject(p, false))}
            {open && d.online && ps.length === 0 && !match && <p className="slist-note">{t('devices.no_terminals')}</p>}
          </section>
        );
      });
  } else {
    body = projects.map((p) => renderProject(p, multi && filter === 'all'));
  }
  return (
    <div className="slist">
      {multi && (
        <div className="devbar" role="group" aria-label={t('sess.dev_filter')}>
          <button type="button" aria-pressed={filter === 'all'} onClick={() => setDevFilter('all')}>
            {t('sess.all_devices')}
          </button>
          {devices.items.map((d) => (
            <button key={d.id} type="button" aria-pressed={filter === d.id} onClick={() => setDevFilter(filter === d.id ? 'all' : d.id)} style={{ '--dc': deviceColor(devices, d.id) }}>
              <DeviceAvatar device={d} size={18} />
              <span className="ellipsis">{deviceName(t, d)}</span>
              {linkState(d, links) !== 'online' && <span className="muted">{linkLabel(t, d, links)}</span>}
            </button>
          ))}
        </div>
      )}
      {s.error && <ErrorNote error={s.error} />}
      {attn.length > 0 && (
        <section className="sattn" aria-label={t('sess.attention')}>
          <h3 className="slist-label slist-label-warn">
            <AlertCircle size={13} aria-hidden="true" />
            {t('sess.attention')} · {attn.length}
          </h3>
          {attn.map((i) => <SessionRow key={i.id} inst={i} attn current={i.id === currentId} />)}
        </section>
      )}
      <div className="slist-head">
        <h3 className="slist-label grow">{t('sess.projects')}</h3>
        {multi && (
          <span className="seg seg-xs" role="group" aria-label={t('sess.group_label')}>
            <button type="button" aria-pressed={groupBy === 'project'} onClick={() => setGroupBy('project')}>
              {t('sess.by_project')}
            </button>
            <button type="button" aria-pressed={groupBy === 'device'} onClick={() => setGroupBy('device')}>
              {t('sess.by_device')}
            </button>
          </span>
        )}
        {projects.length > 0 && (
          <button type="button" className="btn btn-plain btn-icon" onClick={foldAll} aria-label={t(allOpen ? 'sess.fold_all' : 'sess.unfold_all')} title={t(allOpen ? 'sess.fold_all' : 'sess.unfold_all')}>
            {allOpen ? <ChevronsDownUp size={15} /> : <ChevronsUpDown size={15} />}
          </button>
        )}
        <NewProjectButton />
      </div>
      {body}
      {!s.loading && match && projects.length === 0 && <p className="slist-note">{t('sess.no_match')}</p>}
      {match && (
        <button type="button" className="btn btn-plain slist-hist" onClick={() => navigate(`/history?q=${encodeURIComponent(query.trim())}`)}>
          <History size={14} aria-hidden="true" />
          <span className="ellipsis">{t('hist.search_in', { q: query.trim() })}</span>
        </button>
      )}
      {!s.loading && !match && s.projects.length === 0 && <p className="slist-note">{t('sess.empty')}</p>}
    </div>
  );
}

function NewProjectButton() {
  const { t } = useTranslation();
  const ui = useSessionUi();
  return (
    <button type="button" className="btn btn-plain btn-icon" onClick={() => ui.newSession({ pick: true })} aria-label={t('sess.new_project')} title={t('sess.new_project_hint')}>
      <FolderPlus size={15} />
    </button>
  );
}

/** 手机首页：会话列表 + 右下角“新会话”。 */
export function SessionHome() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const ui = useSessionUi();
  return (
    <div className="shome">
      <BackendNote />
      <SessionList />
      <button type="button" className="fab" onClick={() => ui.newSession()} disabled={!status.account.allowed}>
        <Plus size={20} aria-hidden="true" />
        {t('sess.new')}
      </button>
    </div>
  );
}

// ---------------- 开始页（新会话） ----------------

/**
 * Docker 容器：点开后列出这台电脑上运行中的容器（docker ps），可以搜；选中后探测容器里的用户、默认目录和装了的工具。
 * 容器里的会话只有终端，没有对话界面。
 */
function DockerPick({ device, onPick, startOpen = false }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(startOpen);
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(null);
  const [error, setError] = useState(null);
  const [list] = useLoad(() => (open ? deviceApi(device).get('/docker/containers').catch(() => ({ items: [], error: 'DOCKER_FAILED' })) : Promise.resolve(null)), [open, device]);
  const pick = async (c) => {
    setBusy(c.name);
    setError(null);
    try {
      const p = await deviceApi(device).get(`/docker/containers/${encodeURIComponent(c.name)}`);
      onPick({ name: c.name, image: c.image, dir: p.workdir || '/', root: Boolean(p.root), user: p.user, tools: p.tools ?? [] });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };
  if (!open) {
    return (
      <button type="button" className="ppick-item ppick-other" onClick={() => setOpen(true)}>
        <Container size={15} aria-hidden="true" />
        <span className="grow">{t('sess.docker_pick')}</span>
        <ChevronDown size={14} aria-hidden="true" />
      </button>
    );
  }
  const words = q.trim().toLowerCase();
  const items = (list?.items ?? []).filter((c) => !words || `${c.name} ${c.image} ${c.id}`.toLowerCase().includes(words));
  return (
    <div className="ppick-docker">
      <label className="slist-search ppick-search">
        <Search size={14} aria-hidden="true" />
        <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('sess.docker_search')} aria-label={t('sess.docker_search')} spellCheck={false} autoFocus={!isTouch()} />
      </label>
      {!list ? (
        <div className="ppick-note"><Spinner size={14} /></div>
      ) : list.error ? (
        <p className="ppick-note">{t(`sess.docker_err.${list.error}`, { defaultValue: t('sess.docker_err.DOCKER_FAILED') })}</p>
      ) : items.length === 0 ? (
        <p className="ppick-note">{words ? t('sess.no_match') : t('sess.docker_none')}</p>
      ) : (
        items.map((c) => (
          <button key={c.id || c.name} type="button" className="ppick-item" onClick={() => pick(c)} disabled={Boolean(busy)} title={c.status}>
            {busy === c.name ? <Spinner size={15} /> : <Container size={15} aria-hidden="true" />}
            <span className="ellipsis grow">{c.name}</span>
            <span className="ppick-path mono ellipsis">{c.image}</span>
          </button>
        ))
      )}
      {error && <p className="ppick-note warn-text">{errorText(t, error)}</p>}
    </div>
  );
}

/** 文件夹选择弹窗用的“客户端”：把 `/fs/list` 换成容器里的文件夹列表（字段一样）。 */
function containerFs(device, name) {
  return { get: (url) => deviceApi(device).get(url.replace(/^\/fs\/list\?/, `/docker/containers/${encodeURIComponent(name)}/fs?`)) };
}

/** 开始页输入框下面的“Docker”按钮：直接列出当前电脑上运行中的容器。 */
function DockerChip({ device, active, onPick }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const ref = useRef(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const close = (e) => !ref.current?.contains(e.target) && setOpen(false);
    const esc = (e) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', esc);
    };
  }, [open]);
  const d = devices.multi ? devices.byId(device) : null;
  return (
    <span className="start-loc-wrap start-docker" ref={ref}>
      <button type="button" className="btn btn-plain start-adv" aria-pressed={active} aria-expanded={open} onClick={() => setOpen(!open)} title={t('sess.docker_hint')}>
        <Container size={15} aria-hidden="true" />
        Docker
      </button>
      {open && (
        <div className="ppick" role="dialog" aria-label={t('sess.docker_pick')}>
          {d && (
            <div className="ppick-dev">
              <DeviceAvatar device={d} size={18} />
              <b className="ellipsis">{deviceName(t, d)}</b>
              <span className="muted">{osLabel(d.os)}</span>
            </div>
          )}
          <DockerPick
            device={device}
            startOpen
            onPick={(c) => {
              setOpen(false);
              onPick(c);
            }}
          />
        </div>
      )}
    </span>
  );
}

/** 选项目：按电脑列出最近的项目，再下面“选择其他文件夹”“Docker 容器”。 */
function ProjectPicker({ projects, device, workdir, container, onPick, onClose }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const ref = useRef(null);
  // 正在浏览哪台电脑的文件夹：{ dev }，dev 只有一台电脑时是 null，所以不能直接用 null 表示“没打开”
  const [browse, setBrowse] = useState(null);
  useEffect(() => {
    const close = (e) => !ref.current?.contains(e.target) && !browse && onClose();
    const esc = (e) => e.key === 'Escape' && !browse && onClose();
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', esc);
    };
  }, [browse]);
  const devs = devices.multi ? devices.items.filter((d) => d.online) : [null];
  return (
    <div className="ppick" ref={ref} role="dialog" aria-label={t('sess.pick_project')}>
      {devs.map((d) => {
        const ps = projects.filter((p) => p.device === (d?.id ?? null) && !p.offline).slice(0, 12);
        return (
          <div key={d?.id ?? 'one'} className="ppick-group">
            {d && (
              <div className="ppick-dev">
                <DeviceAvatar device={d} size={18} />
                <b className="ellipsis">{deviceName(t, d)}</b>
                <span className="muted">{osLabel(d.os)}</span>
              </div>
            )}
            {ps.map((p) => (
              <button
                key={p.key}
                type="button"
                className="ppick-item"
                aria-pressed={p.device === device && (p.container ? container?.name === p.container.name && container?.dir === p.container.dir : !container && p.workdir === workdir)}
                onClick={() => onPick(p.device, p.container ? '' : p.workdir, p.container ?? null)}
              >
                {p.container ? <Container size={15} aria-hidden="true" /> : <Folder size={15} aria-hidden="true" />}
                <span className="ellipsis grow">{p.name}</span>
                <span className="ppick-path mono ellipsis">{p.container ? `${p.container.name}:${p.container.dir}` : p.workdir}</span>
              </button>
            ))}
            <button type="button" className="ppick-item ppick-other" onClick={() => setBrowse({ dev: d?.id ?? null })}>
              <FolderPlus size={15} aria-hidden="true" />
              <span className="grow">{t('sess.pick_other')}</span>
            </button>
            <DockerPick device={d?.id ?? null} onPick={(c) => onPick(d?.id ?? null, '', c)} />
          </div>
        );
      })}
      <FolderPicker
        open={browse !== null}
        initial={browse && browse.dev === device ? workdir : ''}
        client={deviceApi(browse?.dev ?? null)}
        onClose={() => setBrowse(null)}
        onPick={(path) => onPick(browse.dev, path, null)}
      />
    </div>
  );
}

/** 开始页：类型、第一句话、项目（电脑 + 文件夹）、高级选项。回车就新建、启动并打开。 */
export function StartPane() {
  const s = useSessions();
  const ui = useSessionUi();
  // 等项目列表到了再初始化（默认选最近用过的项目）
  if (s.loading) return <div className="start"><Spinner size={20} /></div>;
  return <StartForm key={ui.start.nonce} />;
}

/**
 * 开始页的表单。`want` 不给时用 ui.start（开始页）；标签栏的“+”弹窗里给出当前会话的电脑和项目，
 * `onDone` 在新建成功后关掉弹窗，`inModal` 时只留输入框（不显示以前的对话、最近项目、提示卡片）。
 */
function StartForm({ want: wantProp, onDone, inModal = false }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const { status: selectedStatus } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const want = wantProp ?? ui.start;
  // 默认：要求的 > 最近用过的在线项目 > 默认文件夹
  const firstProject = s.projects.find((p) => !p.offline);
  const initDevice = want.device !== undefined ? want.device : firstProject ? firstProject.device : devices.multi ? devices.selected : null;
  const [device, setDevice] = useState(initDevice);
  const [workdir, setWorkdir] = useState(want.workdir ?? (want.device === undefined && firstProject ? firstProject.workdir : ''));
  const project = s.projects.find((p) => p.device === device && p.workdir === workdir);
  // Docker 容器（{ name, dir, root, tools }）：选了时会话在容器里运行，只有终端
  const [container, setContainer] = useState(want.container ?? null);
  const [launch, setLaunchState] = useState(want.launch ?? projectLaunch(project) ?? 'claude');
  // 用户自己点过类型：换项目、换电脑都不再改它
  const chosen = useRef(Boolean(want.launch));
  const setLaunch = (l) => {
    chosen.current = true;
    setLaunchState(l);
  };
  const [text, setText] = useState('');
  // Claude Code 用哪个账号 / 接口：null 用那台电脑上的默认
  const [profile, setProfile] = useState(null);
  const [picking, setPicking] = useState(Boolean(want.pick));
  const [dirPick, setDirPick] = useState(false);
  const [adv, setAdv] = useState(false);
  const [moreKinds, setMoreKinds] = useState(false);
  const [name, setName] = useState('');
  const [shell, setShell] = useState(project?.shell ?? '');
  const [fullAccess, setFullAccessState] = useState(() => fullAccessFor(project, selectedStatus.settings));
  // 用户自己勾过：换项目、那台电脑的设置到了都不再改它
  const fullChosen = useRef(false);
  const setFullAccess = (v) => {
    fullChosen.current = true;
    setFullAccessState(v);
  };
  const [autoStart, setAutoStart] = useState(Boolean(project?.auto_start));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const input = useRef(null);
  const client = deviceApi(device);
  // 不是顶栏选中的那台电脑时，用那台电脑自己的状态（默认文件夹、能力）
  const [devStatus] = useLoad(() => (devices.multi && device && device !== devices.selected ? client.get('/status').catch(() => null) : Promise.resolve(null)), [device]);
  const status = devStatus?.settings && devStatus?.capabilities ? devStatus : selectedStatus;
  const [tools, , reloadTools] = useLoad(() => client.get('/tools').catch(() => null), [device]);
  const [shells] = useLoad(() => (adv ? client.get('/shells').catch(() => null) : Promise.resolve(null)), [device, adv]);
  useEffect(() => {
    if (!workdir && status?.settings?.default_workdir) setWorkdir(status.settings.default_workdir);
  }, [status?.settings?.default_workdir]);
  // 手机上不主动聚焦（会弹出输入法）
  useEffect(() => {
    if (!picking && !isTouch()) input.current?.focus();
  }, [want.nonce, picking]);
  // 换了项目：类型和高级选项跟着这个项目上次的
  const pickProject = (dev, dir, c = null) => {
    setDevice(dev);
    setPicking(false);
    setContainer(c);
    if (c) {
      if (launch === 'dsh') setLaunchState('claude');
      setTimeout(() => !isTouch() && input.current?.focus(), 0);
      return;
    }
    setWorkdir(dir);
    const p = s.projects.find((x) => x.device === dev && x.workdir === dir);
    if (p) {
      if (!chosen.current && p.launch) setLaunchState(projectLaunch(p));
      setShell(p.shell ?? '');
      setFullAccessState(fullAccessFor(p, status?.settings));
      setAutoStart(Boolean(p.auto_start));
    }
    setTimeout(() => !isTouch() && input.current?.focus(), 0);
  };
  // 别的电脑的设置（默认全部权限）晚一点才到：还没勾过时跟着它
  useEffect(() => {
    if (!fullChosen.current) setFullAccessState(fullAccessFor(project, status?.settings));
  }, [device, status?.settings?.default_full_access]);
  const missing = (b) => {
    const l = baseLaunch(b);
    return container ? AI_TOOLS.includes(l) && Array.isArray(container.tools) && !container.tools.includes(l) : AI_TOOLS.includes(l) && tools && !tools.tools?.[l];
  };
  const dshRow = launch === 'dsh' ? s.items.find((i) => i.launch === 'dsh' && (!devices.multi || terminalDevice(i.id) === device)) : null;
  const canFull = FULL_ACCESS.includes(baseLaunch(launch));
  const allowed = selectedStatus.account.allowed;
  const devObj = devices.multi ? devices.byId(device) : null;
  const offline = devices.multi && !devObj?.online;
  const takesText = PROMPT_LAUNCHES.includes(launch) || launch === 'custom';
  // 没装的工具：点开始就先在后台装（npm），装好后自动接着开始，不用另开窗口
  const [job, setJob] = useState(null);
  const [mirror, setMirror] = useState('npmmirror');
  const [afterInstall, setAfterInstall] = useState(false);
  useEffect(() => {
    setJob(null);
    setAfterInstall(false);
  }, [launch, device]);
  useEffect(() => {
    if (job?.state !== 'running') return undefined;
    const timer = setTimeout(async () => {
      const j = await client.get(`/tools/install/${launch}`).catch(() => null);
      // 一次没取到（网络抖了）：接着问
      setJob(j ?? { ...job });
      if (j?.state === 'ok') reloadTools();
    }, 1000);
    return () => clearTimeout(timer);
  }, [job]);
  const canInstall = !container && AI_TOOLS.includes(launch) && Boolean(tools?.tools?.npm);
  const install = async () => {
    setError(null);
    try {
      setJob(await client.post('/tools/install', { tool: launch, mirror }));
      setAfterInstall(true);
    } catch (e) {
      setError(e);
    }
  };
  // 装好了、工具列表也刷新了：接着开始
  const installed = afterInstall && job?.state === 'ok' && !missing(launch);
  useEffect(() => {
    if (!installed) return;
    setAfterInstall(false);
    go();
  }, [installed]);
  // 开始按钮点不了的原因：写在按钮旁边，不让人对着灰按钮猜
  const where = container ? container.dir.trim() : workdir.trim();
  const blocked = !allowed ? t('sess.why_locked') : offline ? t('sess.device_offline') : !where ? t('sess.why_no_folder') : null;
  const go = async () => {
    if (busy || !allowed || offline) return;
    setError(null);
    if (missing(launch)) {
      // 能直接装的就装，装好后自动开始；没有 npm（要先装 Node.js）时打开说明怎么装的窗口
      if (canInstall) {
        if (job?.state !== 'running') install();
      } else if (!container) ui.install(launch, device, workdir);
      return;
    }
    if (dshRow) {
      openDsh(dshRow, t, toast);
      return;
    }
    if (launch === 'custom' && !text.trim()) {
      input.current?.focus();
      return;
    }
    const prompt = PROMPT_LAUNCHES.includes(launch) ? text.trim() : '';
    setBusy(true);
    try {
      const r = await client.post('/instances', {
        // 没填名称时用第一句话的开头（AI 工具），否则交给电脑取（文件夹 · 工具）
        name: (name.trim() || (prompt && launch !== 'shell' ? prompt.replace(/\s+/g, ' ').slice(0, 20) : '')).slice(0, 40),
        name_custom: Boolean(name.trim()),
        workdir: container ? '' : workdir,
        container: container ? { name: container.name, dir: container.dir.trim() || '/' } : undefined,
        launch: baseLaunch(launch),
        // Codex 对话 / Codex 终端（其余类型按电脑的默认）
        chat_ui: launch === CODEX_TERM ? false : launch === 'codex' ? true : undefined,
        command: launch === 'custom' ? text.trim() : '',
        prompt: prompt || undefined,
        shell: shell.trim(),
        auto_start: autoStart,
        full_access: canFull && fullAccess,
        claude_profile: launch === 'claude' && !container && profile ? profile : undefined,
        start: true,
      });
      if (device && r.id) rememberTerminals(device, [r]);
      // 全部权限只在能选的类型（AI 会话）上记：开个终端不该把这个项目记成“不要全部权限”
      if (!container)
        s.touchProject(device, r.workdir ?? workdir, {
          launch: baseLaunch(launch),
          ...(baseLaunch(launch) === 'codex' ? { codex_chat: launch !== CODEX_TERM } : {}),
          shell: shell.trim(),
          auto_start: autoStart,
          ...(canFull ? { full_access: fullAccess } : {}),
        });
      s.reload();
      setText('');
      setName('');
      if (launch === 'dsh') openDsh(r, t, toast);
      else ui.openSession(r.id);
      onDone?.();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const placeholder = dshRow ? t('sess.ph_dsh_open') : t(`sess.ph_${launch}`, { tool: t(`launch.${launch}.title`) });
  const projName = container ? container.name : baseName(workdir) || workdir || '…';
  // 没有会话保持（对话页靠它）或者在容器里时 Codex 只有终端，不分两种
  const chatOk = Boolean(status?.capabilities?.persistent) && !container;
  const launches = START_LAUNCHES.filter((l) => !(container && l === 'dsh') && !(!chatOk && l === CODEX_TERM));
  const typeTitle = (l) => (l === 'codex' && chatOk ? t('launch.codex_chat.title') : t(`launch.${l}.title`));
  const typeDesc = (l) => (l === 'codex' && chatOk ? t('launch.codex_chat.desc') : t(`launch.${l}.desc`));
  const types = shownTypes(launches, s.items, launch);
  // 每种后面写上那个账号 5 小时额度还剩多少，选之前就知道该用哪个。用量只有当前这台电脑的、默认账号的：
  // 给别的电脑开、在容器里、另选了 Claude 账号时不显示，免得数字对不上
  const accUsage = useAccountsUsage();
  const kindUsage = (l) => {
    const a = usageAgent(l);
    if (!a || container || (device && device !== devices.selected) || (a === 'claude' && profile)) return null;
    const r = remainingOf(accUsage?.[a]?.five_hour);
    return r && { ...r, name: a === 'claude' ? 'Claude' : 'Codex', resets: Date.parse(accUsage[a].five_hour.resets_at) - Date.now() };
  };
  const pickedUsage = kindUsage(launch);
  // 列表里每种类型后面的小标签：有没有对话页（Claude Code 两边都有）
  const kindTag = (l) => {
    if (l === 'claude') return chatOk ? 'both' : 'term';
    if (l === 'codex') return chatOk ? 'chat' : 'term';
    return l === 'dsh' ? null : 'term';
  };
  return (
    <div className="start">
      {!opensInPage() && !inModal && (
        <button type="button" className="btn btn-plain start-back" onClick={() => navigate('/')}>
          <ArrowLeft size={18} aria-hidden="true" />
          {t('sess.back')}
        </button>
      )}
      <h1 className={`start-title ${inModal ? 'start-title-sm' : ''}`}>{workdir ? t('sess.start_in', { name: projName }) : t('sess.start_title')}</h1>
      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          go();
        }}
      >
        {takesText && !dshRow ? (
          <textarea
            ref={input}
            className="start-input"
            rows={3}
            value={text}
            onChange={(e) => setText(e.target.value)}
            // 手机上没有 Shift+回车：回车换行，点“开始”发出去（同对话页的输入框）
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing && !isTouch()) {
                e.preventDefault();
                go();
              }
            }}
            enterKeyHint={isTouch() ? 'enter' : 'send'}
            placeholder={placeholder}
            aria-label={placeholder}
            spellCheck={false}
            maxLength={4000}
          />
        ) : (
          <p className="start-input start-note">{placeholder}</p>
        )}
        <div className="start-bar">
          <span className="start-loc-wrap">
            <button type="button" className="start-loc" onClick={() => setPicking(!picking)} aria-expanded={picking} title={container ? `${container.name}:${container.dir}` : workdir}>
              {devObj ? <DeviceAvatar device={devObj} size={18} /> : container ? <Container size={15} aria-hidden="true" /> : <Folder size={15} aria-hidden="true" />}
              {devObj && container && <Container size={14} aria-hidden="true" className="muted" />}
              <span className="ellipsis">{projName}</span>
              {devObj && <span className="muted ellipsis start-loc-dev">{deviceName(t, devObj)}</span>}
              <ChevronDown size={14} aria-hidden="true" />
            </button>
            {picking && <ProjectPicker projects={s.projects} device={device} workdir={workdir} container={container} onPick={pickProject} onClose={() => setPicking(false)} />}
          </span>
          <DockerChip device={device} active={Boolean(container)} onPick={(c) => pickProject(device, '', c)} />
          {/* 全部权限一眼就能看到、点一下就切换（以前藏在“高级”里） */}
          {canFull && (
            <button type="button" className="btn btn-plain start-adv" aria-pressed={fullAccess} onClick={() => setFullAccess(!fullAccess)} title={t(`form.full_access_hint_${launch}`)}>
              <ShieldCheck size={15} aria-hidden="true" />
              {t('sess.full_access_chip')}
            </button>
          )}
          {launch === 'claude' && !container && <ClaudeProfilePick client={client} device={device} value={profile} onChange={setProfile} />}
          {launch !== 'dsh' && (
            <button type="button" className="btn btn-plain start-adv" onClick={() => setAdv(!adv)} aria-expanded={adv}>
              <SlidersHorizontal size={15} aria-hidden="true" />
              {t('sess.advanced')}
            </button>
          )}
          <span className="grow" />
          {blocked ? (
            <span className="small muted">{blocked}</span>
          ) : (
            missing(launch) &&
            job?.state !== 'running' && (
              <span className="small muted start-install">
                {t(container ? 'sess.container_missing' : canInstall ? 'sess.install_then_start' : 'sess.install_first', { tool: t(`launch.${launch}.title`) })}
                {canInstall && (
                  <select value={mirror} onChange={(e) => setMirror(e.target.value)} aria-label={t('tools.mirror')}>
                    {['npmmirror', 'tencent', 'official'].map((m) => (
                      <option key={m} value={m}>
                        {t(`tools.mirrors.${m}`)}
                      </option>
                    ))}
                  </select>
                )}
              </span>
            )
          )}
          <button type="submit" className="start-go" disabled={busy || Boolean(blocked)} aria-label={t('sess.start')} title={blocked ?? t('sess.start_hint')}>
            {busy || job?.state === 'running' ? <Spinner size={18} /> : dshRow ? <ExternalLink size={18} /> : <ArrowUp size={20} />}
          </button>
        </div>
        {job?.tool === launch && job.state !== 'ok' && (
          <div className="start-install-log">
            <InstallProgress job={job} tool={t(`launch.${launch}.title`)} onRetry={install} busy={false} />
            {job.state === 'running' && <p className="small muted">{t('sess.install_auto_start')}</p>}
          </div>
        )}
        {container && (
          <div className="start-container">
            <label className="start-container-dir">
              <span className="muted">{t('sess.container_dir')}</span>
              <input className="mono" value={container.dir} onChange={(e) => setContainer({ ...container, dir: e.target.value })} spellCheck={false} aria-label={t('sess.container_dir')} />
              <Button variant="secondary" icon={FolderOpen} onClick={() => setDirPick(true)} aria-label={t('fs.browse')} title={t('fs.browse')} />
            </label>
            <FolderPicker
              open={dirPick}
              initial={container.dir}
              client={containerFs(device, container.name)}
              onClose={() => setDirPick(false)}
              onPick={(dir) => setContainer((c) => c && { ...c, dir })}
            />
            <p className="small muted">
              {t('sess.container_note')}
              {container.root && canFull && fullAccess && launch === 'claude' ? ` ${t('sess.container_root')}` : ''}
            </p>
          </div>
        )}
        {adv && launch !== 'dsh' && (
          <div className="start-advanced">
            <label className="field">
              <span className="field-label">{t('form.name')}</span>
              <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={t('sess.name_ph')} />
            </label>
            <ShellPicker value={shell} onChange={setShell} data={shells} inherit={status.settings?.default_shell || null} />
            {status.capabilities?.persistent && (
              <div className="between">
                <span className="grow">
                  <b style={{ fontSize: 14 }}>{t('form.auto_start')}</b>
                  <span className="field-hint" style={{ display: 'block' }}>
                    {t('sess.adv_remember')}
                  </span>
                </span>
                <Switch checked={autoStart} onChange={setAutoStart} label={t('form.auto_start')} />
              </div>
            )}
          </div>
        )}
      </form>
      {/* 用什么开始：一行一种，写清楚有没有对话页；常用的直接列出，其余在“更多类型”里 */}
      <div className="start-kinds" role="radiogroup" aria-label={t('sess.kinds_title')}>
        <div className="start-kinds-head">
          <span className="sec">{t('sess.kinds_title')}</span>
          {chatOk && <span className="small muted">{t('sess.kinds_note')}</span>}
        </div>
        {[...types.shown, ...(moreKinds ? types.more : [])].map((l) => (
          <button key={l} type="button" role="radio" aria-checked={launch === l} className="start-kind" onClick={() => setLaunch(l)}>
            <LaunchIcon launch={baseLaunch(l)} size={18} />
            <span className="start-kind-text">
              <span className="start-kind-name">
                <b>{typeTitle(l)}</b>
                {kindTag(l) && <span className="srow-kind">{t(`sess.kind_${kindTag(l)}`)}</span>}
                {missing(l) && <span className="start-miss">{t('sess.not_installed')}</span>}
              </span>
              <span className="start-kind-desc">{t(`sess.kind_desc.${l}`, { defaultValue: typeDesc(l) })}</span>
            </span>
            {kindUsage(l) && (
              <span className="start-kind-usage" data-level={kindUsage(l).level} title={t('sbar.usage_line', { name: kindUsage(l).name, n: kindUsage(l).left })}>
                {t('sbar.left', { n: kindUsage(l).left })}
                <UsageBar {...kindUsage(l)} />
              </span>
            )}
          </button>
        ))}
        {types.more.length > 0 && (
          <button type="button" className="start-kind start-kind-more" onClick={() => setMoreKinds(!moreKinds)} aria-expanded={moreKinds}>
            <ChevronDown size={16} aria-hidden="true" />
            {t(moreKinds ? 'sess.fewer_types' : 'sess.more_types')}
          </button>
        )}
      </div>
      {pickedUsage && pickedUsage.level === 'low' && (
        <p className="start-usage-low small">
          {t('sess.usage_low', { name: pickedUsage.name, n: pickedUsage.left })}
          {pickedUsage.resets > 0 && ` ${t('sbar.resets', { in: spanText(t, pickedUsage.resets) })}`}
        </p>
      )}
      <ErrorNote error={error} />
      {missing(launch) && !container && (
        <button type="button" className="btn btn-plain small" onClick={reloadTools}>
          {t('tools.recheck')}
        </button>
      )}
      {!inModal && (
        <>
          {!container && <PrevConversations device={device} workdir={workdir} launch={launch} focus={Boolean(want.prev)} />}
          <RecentProjects current={project?.key} onPick={pickProject} />
          <div className="start-foot">
            <BackendNote />
            <RemoteCard />
            <SafetyNote />
            <Promotions />
          </div>
        </>
      )}
    </div>
  );
}

/** 开始页下面最近的几个项目，点一下换过去。 */
function RecentProjects({ current, onPick }) {
  const devices = useDevices();
  const s = useSessions();
  const list = s.projects.filter((p) => !p.offline && p.key !== current).slice(0, 4);
  if (!list.length) return null;
  return (
    <div className="start-recent">
      {list.map((p) => (
        <button key={p.key} type="button" onClick={() => onPick(p.device, p.container ? "" : p.workdir, p.container ?? null)} title={p.container ? `${p.container.name}:${p.container.dir}` : p.workdir}>
          {devices.multi ? <DeviceAvatar device={devices.byId(p.device)} size={16} /> : <Folder size={14} aria-hidden="true" />}
          <span className="ellipsis">{p.name}</span>
        </button>
      ))}
    </div>
  );
}

// ---------------- 右边打开的会话 ----------------

/** 让元素正好填满窗口剩下的高度（上面的横幅、提示有多高都行）。 */
function useFillHeight(ref, enabled) {
  const [h, setH] = useState(null);
  useLayoutEffect(() => {
    if (!enabled) return undefined;
    const on = () => {
      const el = ref.current;
      if (!el) return;
      const top = el.getBoundingClientRect().top + window.scrollY;
      // 宽屏底部的状态栏占的高度（见 styles.css 的 --sbar-h）
      const bar = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sbar-h')) || 0;
      setH(Math.max(320, window.innerHeight - top - bar));
    };
    on();
    const ro = new ResizeObserver(on);
    ro.observe(document.body);
    window.addEventListener('resize', on);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', on);
    };
  }, [enabled]);
  return h;
}

/** 会话顶栏里名字后面的一行小字：电脑（颜色点 + 名字）/ 目录。不超过一行、不太宽，悬停看完整的。 */
function SessionLead({ inst }) {
  const { t } = useTranslation();
  const devices = useDevices();
  if (!inst) return null;
  const dev = devices.multi ? devices.byId(terminalDevice(inst.id)) : null;
  const dir = sessionDir(inst);
  const full = [dev && deviceName(t, dev), inst.container && t('sess.in_container', { name: inst.container.name }), dir].filter(Boolean).join(' · ');
  return (
    <span className="slead" title={full}>
      {dev && (
        <>
          <span className="slead-dot" style={{ background: deviceColor(devices, dev.id) }} aria-hidden="true" />
          <span className="slead-dev ellipsis">{deviceName(t, dev)}</span>
          <span className="slead-sep" aria-hidden="true">/</span>
        </>
      )}
      {inst.container && <Container size={12} aria-hidden="true" />}
      <span className="slead-proj ellipsis">{baseName(dir) || dir}</span>
    </span>
  );
}

function SessionMenu({ inst }) {
  const a = useActions(inst);
  return <PopMenu items={a.menu(false)} className="btn btn-plain btn-icon" />;
}

/**
 * 右边的会话：当前的显示，最近打开的几个隐藏着保持连接（切回来不用重新连）。
 * 不在会话页时（例如去了“文件”）整块隐藏，终端也不断开。
 */
export function SessionStage({ activeId }) {
  const s = useSessions();
  const ref = useRef(null);
  const height = useFillHeight(ref, Boolean(activeId));
  const ids = activeId && !s.recent.includes(activeId) ? [activeId, ...s.recent] : s.recent;
  useEffect(() => {
    if (!activeId) return;
    s.openTab(activeId);
    s.openRecent(activeId);
  }, [activeId]);
  // 被删掉的会话（见 sessions.jsx 的 gone）：标签跟着关掉
  const missing = s.gone;
  useEffect(() => {
    const gone = [...new Set([...s.recent, ...s.tabs])].filter((id) => id !== activeId && missing(id));
    if (gone.length) s.closeTabs(gone);
  });
  if (!ids.length) return null;
  return (
    <div className="sstage" ref={ref} hidden={!activeId} style={height ? { height } : undefined}>
      {ids.map((id) => {
        // 电脑离线时用最后一次看到的：终端不卸载，内容还能看
        const inst = s.known(id);
        return (
          <TermPane
            key={id}
            id={id}
            inst={inst}
            missing={missing(id)}
            offline={s.offline(id)}
            active={id === activeId}
            lead={inst && <LaunchIcon launch={inst.launch} size={16} />}
            sub={<SessionLead inst={inst} />}
            menu={inst && <SessionMenu inst={inst} />}
            focusN={s.focusReq.id === id ? s.focusReq.n : 0}
            onStart={async () => {
              await deviceApi(terminalDevice(id)).post(`/instances/${id}/start`);
              s.reload();
            }}
          />
        );
      })}
    </div>
  );
}

// ---------------- 顶部标签（打开的会话） ----------------

/** 点到外面、按 Esc 时关掉弹出的东西。 */
function useDismiss(ref, open, onClose) {
  useEffect(() => {
    if (!open) return undefined;
    const down = (e) => !ref.current?.contains(e.target) && onClose();
    const key = (e) => e.key === 'Escape' && onClose();
    document.addEventListener('pointerdown', down, true);
    document.addEventListener('keydown', key);
    window.addEventListener('blur', onClose);
    return () => {
      document.removeEventListener('pointerdown', down, true);
      document.removeEventListener('keydown', key);
      window.removeEventListener('blur', onClose);
    };
  }, [open]);
}

function TabState({ inst }) {
  if (!inst) return null;
  const state = inst.error && !inst.running ? 'error' : sessionState(inst);
  return state === 'idle' || state === 'stopped' ? null : <StateDot state={state} />;
}

/**
 * 顶部标签：打开过的会话按打开的先后排成一行（新打开的在最后，不会自己换位置），可以拖动调整；
 * 中键或 × 关掉（只是收起，终端照常运行）；右键：关闭其他 / 右侧 / 全部。多了横向滚动，右边 ▾ 列出全部、可以搜。
 * 只在电脑上显示（手机上会话整页打开）。
 */
export function SessionTabs({ activeId }) {
  const { t } = useTranslation();
  const s = useSessions();
  const ui = useSessionUi();
  const { navigate } = useRouter();
  const devices = useDevices();
  const bar = useRef(null);
  const [drag, setDrag] = useState(null);
  const [ctx, setCtx] = useState(null);
  const [all, setAll] = useState(false);
  const [q, setQ] = useState('');
  const [create, setCreate] = useState(false);
  const ctxRef = useRef(null);
  const allRef = useRef(null);
  useDismiss(ctxRef, Boolean(ctx), () => setCtx(null));
  useDismiss(allRef, all, () => setAll(false));
  useEffect(() => {
    bar.current?.querySelector('.stab.on')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }, [activeId, s.tabs.length]);
  if (!opensInPage() || s.tabs.length === 0) return null;
  const close = (ids) => {
    setCtx(null);
    s.closeTabs(ids);
    if (!activeId || !ids.includes(activeId)) return;
    // 关掉的是当前的：切到右边的，没有就左边的，都没有回开始页
    const at = s.tabs.indexOf(activeId);
    const keep = (x) => !ids.includes(x);
    const next = s.tabs.slice(at + 1).find(keep) ?? s.tabs.slice(0, at).reverse().find(keep);
    if (next) ui.openSession(next);
    else navigate('/');
  };
  const tip = (inst) => {
    if (!inst) return '';
    const dev = devices.multi ? devices.byId(terminalDevice(inst.id)) : null;
    return [sessionTitle(inst), customName(inst) && t('sess.my_name', { name: customName(inst) }), [dev && deviceName(t, dev), inst.container?.name, sessionDir(inst)].filter(Boolean).join(' · ')].filter(Boolean).join('\n');
  };
  const words = q.trim().toLowerCase();
  const listed = s.tabs.filter((id) => {
    if (!words) return true;
    const i = s.byId(id);
    return [sessionTitle(i), i?.name, sessionDir(i), i?.container?.name, i?.launch].filter(Boolean).join(' ').toLowerCase().includes(words);
  });
  const ctxAt = ctx ? s.tabs.indexOf(ctx.id) : -1;
  return (
    <div className="stabs" role="tablist" aria-label={t('sess.tabs')}>
      <div className="stabs-scroll" ref={bar} onWheel={(e) => e.deltaY && bar.current && (bar.current.scrollLeft += e.deltaY)}>
        {s.tabs.map((id, k) => (
          <SessionTab
            key={id}
            id={id}
            index={k}
            active={id === activeId}
            title={tip(s.byId(id))}
            dragging={drag === id}
            onDragStart={() => setDrag(id)}
            onDragEnd={() => setDrag(null)}
            onDrop={(to) => drag && s.moveTab(drag, to)}
            onClose={() => close([id])}
            onMenu={(x, y) => setCtx({ id, x, y })}
          />
        ))}
      </div>
      <button type="button" className="btn btn-plain btn-icon stabs-new" onClick={() => setCreate(true)} aria-label={t('sess.tab_new')} title={t('sess.tab_new_hint')}>
        <Plus size={16} />
      </button>
      <div className="stabs-all-wrap" ref={allRef}>
        <button type="button" className="btn btn-plain btn-icon stabs-more" onClick={() => setAll(!all)} aria-expanded={all} aria-label={t('sess.tabs_all')} title={t('sess.tabs_all')}>
          <ChevronDown size={15} />
          <span className="stabs-count">{s.tabs.length}</span>
        </button>
        {all && (
          <div className="stabs-all" role="dialog" aria-label={t('sess.tabs_all')}>
            <label className="slist-search">
              <Search size={14} aria-hidden="true" />
              <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('sess.tabs_search')} aria-label={t('sess.tabs_search')} spellCheck={false} autoFocus />
            </label>
            <div className="stabs-list">
              {listed.map((id) => {
                const inst = s.byId(id);
                return (
                  <div key={id} className={`stabs-row ${id === activeId ? 'on' : ''}`}>
                    <button
                      type="button"
                      className="stabs-row-main"
                      onClick={() => {
                        setAll(false);
                        ui.openSession(id);
                      }}
                      title={tip(inst)}
                    >
                      <LaunchIcon launch={inst?.launch} size={15} />
                      <span className="stabs-row-text">
                        <span className="ellipsis">{inst ? sessionTitle(inst) : id}</span>
                        {inst && <SessionLead inst={inst} />}
                      </span>
                      <TabState inst={inst} />
                    </button>
                    <button type="button" className="btn btn-plain btn-icon stab-x" onClick={() => close([id])} aria-label={t('sess.tab_close')} title={t('sess.tab_close')}>
                      <X size={14} />
                    </button>
                  </div>
                );
              })}
              {listed.length === 0 && <p className="slist-note">{t('sess.no_match')}</p>}
            </div>
            <button type="button" className="btn btn-plain btn-sm stabs-close-all" onClick={() => { setAll(false); close([...s.tabs]); }}>
              {t('sess.tabs_close_all')}
            </button>
          </div>
        )}
      </div>
      {create && <QuickStart inst={s.byId(activeId)} onClose={() => setCreate(false)} />}
      {ctx && (
        <div className="pop-menu stabs-ctx" ref={ctxRef} role="menu" style={{ left: ctx.x, top: ctx.y }}>
          <button type="button" role="menuitem" onClick={() => close([ctx.id])}>{t('sess.tab_close')}</button>
          <button type="button" role="menuitem" onClick={() => close(s.tabs.filter((x) => x !== ctx.id))} disabled={s.tabs.length < 2}>{t('sess.tabs_close_others')}</button>
          <button type="button" role="menuitem" onClick={() => close(s.tabs.slice(ctxAt + 1))} disabled={ctxAt === s.tabs.length - 1}>{t('sess.tabs_close_right')}</button>
          <button type="button" role="menuitem" onClick={() => close([...s.tabs])}>{t('sess.tabs_close_all')}</button>
        </div>
      )}
    </div>
  );
}

/** 标签栏“+”：弹出新建框，预先选好当前会话所在的电脑和项目（容器会话就是同一个容器）。 */
function QuickStart({ inst, onClose }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const [want] = useState(() => {
    if (!inst) return { nonce: 0 };
    const device = devices.multi ? terminalDevice(inst.id) : undefined;
    return inst.container
      ? { device, container: { ...inst.container, dir: inst.container.dir || '/' }, nonce: 0 }
      : { device, workdir: inst.workdir || sessionDir(inst), nonce: 0 };
  });
  return (
    <Modal open onClose={onClose} title={t('sess.tab_new')} wide className="start-modal">
      <StartForm want={want} onDone={onClose} inModal />
    </Modal>
  );
}

function SessionTab({ id, index, active, title, dragging, onDragStart, onDragEnd, onDrop, onClose, onMenu }) {
  const { t } = useTranslation();
  const s = useSessions();
  const ui = useSessionUi();
  const devices = useDevices();
  const inst = s.byId(id);
  const dev = devices.multi ? terminalDevice(id) : null;
  // 按下就切换（见 usePressActivate）；要能拖动，所以按下时不拦默认行为，焦点之后交给输入框
  const { onMouseDown: _keep, ...press } = usePressActivate(() => ui.openSession(id), false);
  return (
    <div
      className={`stab ${active ? 'on' : ''} ${dragging ? 'dragging' : ''}`}
      style={dev ? { '--dc': deviceColor(devices, dev) } : undefined}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', id);
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault();
        onDrop(index);
      }}
      onMouseDown={(e) => e.button === 1 && e.preventDefault()}
      onAuxClick={(e) => {
        if (e.button !== 1) return;
        e.preventDefault();
        onClose();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu(e.clientX, e.clientY);
      }}
    >
      <button type="button" role="tab" aria-selected={active} className="stab-main" {...press} title={title}>
        <LaunchIcon launch={inst?.launch} size={14} />
        <span className="ellipsis">{inst ? sessionTitle(inst) : '…'}</span>
        <TabState inst={inst} />
      </button>
      <button type="button" className="stab-x" onClick={onClose} aria-label={t('sess.tab_close')} title={t('sess.tab_close')}>
        <X size={13} />
      </button>
    </div>
  );
}
