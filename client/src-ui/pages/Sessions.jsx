// 会话（首页）：左侧栏里的项目和会话列表、右边打开的会话。新会话是草稿页（Draft.jsx）：点“新会话”、项目上的 +
// 马上出现在当前项目里，写下第一句、回车才启动；草稿在侧栏这个项目最上面、标签栏最后各有一行。
// 项目下面是这个文件夹里的全部会话：在运行的，加上 Claude Code、Codex、OpenCode 存着的以前的对话（点一下接着聊），
// 侧栏每个项目只列最近几个，其余在项目页（Project.jsx）。
// 项目 = 某台电脑上的一个文件夹（见 shared/sessions.jsx）。电脑用颜色 + 系统图标区分（shared/brands.jsx）。
// 电脑上会话在右边打开，最近打开的几个在后台保持连接；手机上整页打开 /t/{id}（有按键条）。
import { AlertCircle, Check, ChevronDown, Container, Copy, X, ChevronsDownUp, ChevronsUpDown, EyeOff, Folder, FolderPlus, History, Laptop, ListTree, Pin, PinOff, Play, Plus, Search, SlidersHorizontal, PanelLeftClose, Trash2 } from 'lucide-react';
import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { deviceApi, rememberTerminals, terminalDevice } from '../shared/api.js';
import { DeviceAvatar, deviceColor, DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, osLabel, RenameDeviceDialog, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { linkFailure, linkLabel, linkState, useLinks } from '../shared/paths.jsx';
import { isTouch } from '../shared/KeyBar.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { Link, useRouter } from '../shared/router.jsx';
import { baseName, customName, groupProjects, isActive as isLive, isDraftProject, lastUsed, projectEntries, projectPath, sessionDir, sessionState, sessionTitle, startedAt, useSessions } from '../shared/sessions.jsx';
import { isMod, keyLabel } from '../shared/hotkeys.js';
import { ErrorNote, Spinner, useToast } from '../shared/ui.jsx';
import { AddProjectDialog } from './AddProject.jsx';
import { CAN_FORK } from './History.jsx';
import { BackendNote, InstanceDialog, LogsDialog, useInstanceActions } from './Terminals.jsx';
import { TermPane } from './TerminalView.jsx';
import { WebPane } from './WebPane.jsx';

// 项目“⋯”里换个类型新建
export const QUICK_LAUNCHES = ['claude', 'codex', 'opencode', 'shell'];
export const AI_TOOLS = ['codex', 'claude', 'opencode', 'dsh'];
export const FULL_ACCESS = ['codex', 'claude', 'opencode'];

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
const FOLD_KEY = 'looklook.fold';
const GROUP_KEY = 'looklook.groupby';
const DEVF_KEY = 'looklook.devfilter';
const QUERY_KEY = 'looklook.query';
const ONLINE_KEY = 'looklook.devonline';
const STATUS_KEY = 'looklook.sstatus';
// 侧栏每个项目里，在运行的之外再列几个（停了的终端和以前的对话）
const SIDE_SHOW = 5;
// 项目多于这么多时，一个月没用过的收进一组
const OLD_MIN = 8;
const MONTH = 30 * 86400e3;

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
  // 添加项目的弹窗（{ device }：先选中哪台电脑）
  const [adding, setAdding] = useState(null);
  // 草稿页要预先填好的（项目、类型）；nonce 让草稿页重新初始化
  const [start, setStart] = useState({ nonce: 0 });
  // 草稿页开着时它在哪个项目（{ device, workdir, container }）：侧栏、标签栏显示“新会话 · 草稿”
  const [draftAt, setDraftAt] = useState(null);
  const openSession = (id) => {
    // 手机上整页打开；网页类型（DSH）打开的是它的网页
    if (!opensInPage()) return navigate(s.byId(id)?.launch === 'dsh' ? `/t/${id}?web` : `/t/${id}`);
    s.openTab(id);
    s.openRecent(id);
    s.requestFocus(id);
    if (path !== `/s/${id}`) navigate(`/s/${id}`);
  };
  /** 打开草稿页。没说在哪个项目时：在当前会话的项目里，已经在草稿页就留在那个项目。 */
  const newSession = (want = {}) => {
    let at = {};
    if (want.device === undefined && want.workdir === undefined && !want.container) {
      const inst = s.byId(path.match(/^\/s\/([^/]+)/)?.[1]);
      if (inst) {
        const device = devices.multi ? terminalDevice(inst.id) : null;
        at = inst.container ? { device, workdir: '', container: { ...inst.container, dir: inst.container.dir || '/' } } : { device, workdir: inst.workdir || sessionDir(inst) };
      } else if (draftAt) at = draftAt;
    }
    setStart((x) => ({ ...at, ...want, nonce: x.nonce + 1 }));
    navigate(opensInPage() ? '/' : '/new');
  };
  /** 在某个项目里开草稿。 */
  const draftIn = (p) => newSession(p.container ? { device: p.device, workdir: '', container: p.container } : { device: p.device, workdir: p.workdir });
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
      s.reloadFolders();
      openSession(r.id);
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
    // 这台电脑上已经没有这个工具了（卸载了、换了电脑）：去草稿页，那里会引导安装
    const tools = AI_TOOLS.includes(c.agent) ? await deviceApi(device).get('/tools').catch(() => null) : null;
    if (tools && !tools.tools?.[c.agent]) {
      newSession({ device, workdir: c.workdir, launch: c.agent });
      return;
    }
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
    s.reloadFolders();
    openSession(r.id);
    toast(t(fork ? 'hist.forked' : 'hist.continued', { name: r.name }), 'ok');
  };
  const value = {
    openSession,
    newSession,
    draftIn,
    draftAt,
    setDraftAt,
    continueConv,
    quickCreate,
    start,
    edit: (inst) => setDialog({ initial: inst }),
    install: (preset, device, workdir) => setDialog({ preset, device, workdir }),
    logs: setLogs,
    addProject: (want = {}) => setAdding(want),
    openProject: (p) => navigate(projectPath(p)),
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
      {adding && <AddProjectDialog want={adding} onClose={() => setAdding(null)} />}
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
export function useActions(inst) {
  const s = useSessions();
  const ui = useSessionUi();
  return useInstanceActions(inst, { onChanged: s.reload, onEdit: ui.edit, onLogs: ui.logs, onOpenHere: ui.openSession, onHide: s.hide });
}

// ---------------- 侧栏：项目和会话 ----------------

/** 侧栏里的时间写短一点：刚刚、5 分钟、2 小时、3 天。 */
export function shortAgo(t, ms) {
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

function SessionRow({ inst, current, attn, devTag = false }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const s = useSessions();
  const a = useActions(inst);
  // 启动失败：红点，悬停看原因
  const state = inst.error && !inst.running ? 'error' : sessionState(inst);
  const errorTitle = state === 'error' ? t(`instance.error.${inst.error}`, { defaultValue: t('instance.error.other') }) : undefined;
  const p = attn ? s.projectOf(inst) : null;
  const dev = (attn || devTag) && devices.multi ? devices.byId(terminalDevice(inst.id)) : null;
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
          <span className="ellipsis">{title}</span>
          {attn && (
            <span className="srow-sub">
              <span className="ellipsis">{p?.name ?? baseName(dir)}</span>
              {dev && <DeviceTag device={dev} />}
            </span>
          )}
        </span>
        {devTag && dev && <RowDevice device={dev} />}
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

/** 合并的项目里，一行是哪台电脑上的：电脑颜色的短名字（离线的灰着）。 */
function RowDevice({ device }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const name = deviceName(t, device);
  return (
    <span className={`srow-dev ${device.online ? '' : 'off'}`} style={{ '--dc': deviceColor(devices, device.id) }} title={name}>
      {name}
    </span>
  );
}

/**
 * 侧栏里的一个以前的对话：点一下接着聊（新建一个会话接着它）。两分钟内还在写的可能正开在别处，先去项目页看提示再选。
 * 离线电脑上的（上次看到的）只显示。
 */
function ConvSideRow({ p, conv, devTag = false }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const off = Boolean(p.offline);
  const peek = () => navigate(`${projectPath(p)}&c=${encodeURIComponent(`${conv.agent}:${conv.id}`)}`);
  const go = async (fork = false) => {
    if (busy) return;
    if (conv.live && !fork) return peek();
    setBusy(true);
    try {
      await ui.continueConv(p.device, conv, fork);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const hide = () =>
    deviceApi(p.device)
      .put('/history/marks', { agent: conv.agent, id: conv.id, hide: true })
      .then(
        () => {
          s.reloadFolders();
          toast(t('hist.hidden_done'));
        },
        (e) => toast(errorText(t, e), 'error'),
      );
  const disabled = busy || off || !status.account.allowed;
  const press = usePressActivate(() => go(), disabled);
  const when = Date.parse(conv.updated_at) || 0;
  const menu = [
    { icon: Play, label: t('hist.continue'), onClick: () => go(), disabled },
    CAN_FORK.includes(conv.agent) && { icon: Copy, label: t('hist.fork'), onClick: () => go(true), disabled },
    { icon: ListTree, label: t('sess.peek'), onClick: peek },
    'sep',
    { icon: EyeOff, label: t('hist.hide'), onClick: hide },
  ];
  const devices = useDevices();
  const dev = devTag ? devices.byId(p.device) : null;
  return (
    <div className={`srow srow-conv ${off ? 'srow-off' : ''}`}>
      <button type="button" className="srow-main" {...press} disabled={disabled} title={[conv.title, conv.last].filter(Boolean).join('\n')}>
        <LaunchIcon launch={conv.agent} size={16} className="srow-icon" />
        <span className="srow-text">
          <span className="ellipsis">{conv.title}</span>
        </span>
        {dev && <RowDevice device={dev} />}
        {busy ? <Spinner size={13} /> : when > 0 && <span className="srow-when">{shortAgo(t, when)}</span>}
      </button>
      {!off && <PopMenu items={menu} className="btn btn-plain btn-icon srow-more" />}
    </div>
  );
}

/** 侧栏里草稿所在的项目最上面：“新会话 · 草稿”，点一下回到草稿的输入框。 */
function DraftSideRow() {
  const { t } = useTranslation();
  return (
    <div className="srow current srow-draft">
      <button type="button" className="srow-main" onClick={() => window.dispatchEvent(new Event('ll-draft-focus'))} aria-current="page">
        <span className="draft-tile" aria-hidden="true" />
        <span className="srow-text">
          <span className="ellipsis">{t('sess.new')}</span>
        </span>
        <span className="srow-when">{t('draft.tag')}</span>
      </button>
    </div>
  );
}

/** 合并的项目在哪几台电脑上：每台一个颜色点（离线的是空心），后面写“2 台”，有离线的写“1 台在线”。 */
function DeviceDots({ members }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const on = members.filter((m) => !m.offline).length;
  const names = members.map((m) => `${deviceName(t, devices.byId(m.device))}${m.offline ? ` · ${t('devices.offline')}` : ''}`).join('\n');
  return (
    <span className="dev-dots" title={names}>
      {members.map((m) => (
        <span key={m.key} className={`dev-dot ${m.offline ? 'off' : ''}`} style={{ '--dc': deviceColor(devices, m.device) }} aria-hidden="true" />
      ))}
      <span>{on === members.length ? t('sess.n_devices', { count: members.length }) : t('sess.n_online', { count: on })}</span>
    </span>
  );
}

/**
 * 侧栏里的项目。`x` 是合成后的（见 SessionList）：多台电脑上的同一个仓库时 `x.merged`，
 * 标出是哪几台；+ 和菜单用 `p`（在线的里面最近用过的那台），新会话的草稿里能再换。
 */
function ProjectRow({ p, x, open, onToggle, showDevice }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const merged = Boolean(x?.merged);
  const dev = devices.multi && !merged ? devices.byId(p.device) : null;
  const attn = !open && (x?.items ?? p.items).some((i) => sessionState(i) === 'attn');
  const members = x?.members ?? [p];
  const pinned = members.some((m) => m.pinned);
  const allowed = status.account.allowed && !p.offline;
  const menu = [
    ...QUICK_LAUNCHES.map((l) => ({ icon: () => <LaunchIcon launch={l} size={16} />, label: t('sess.new_type', { type: t(`launch.${l}.title`) }), onClick: () => ui.quickCreate(p, l), disabled: !allowed })),
    'sep',
    !p.container && { icon: ListTree, label: t('sess.open_project'), onClick: () => ui.openProject(p) },
    !p.container && { icon: pinned ? PinOff : Pin, label: t(pinned ? 'sess.unpin' : 'sess.pin'), onClick: () => members.filter((m) => !m.offline).forEach((m) => s.pinProject(m.device, m.workdir, !pinned)) },
    !merged && p.items.length === 0 && { icon: Trash2, label: t('sess.remove_project'), onClick: () => s.removeProject(p.device, p.workdir), danger: true },
  ];
  return (
    <div className={`prow ${p.offline ? 'offline' : ''}`} style={dev ? { '--dc': deviceColor(devices, dev.id) } : undefined}>
      <button type="button" className="prow-main" onClick={onToggle} aria-expanded={open} title={merged ? [x.members[0].remote, ...members.map((m) => `${deviceName(t, devices.byId(m.device))}: ${m.workdir}`)].join('\n') : `${p.container ? `${p.container.name}:${p.container.dir}` : p.workdir}${dev ? ` · ${deviceName(t, dev)}` : ''}`}>
        <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
        {p.container ? <Container size={15} className="prow-folder" aria-label={t('sess.in_container', { name: p.container.name })} /> : <Folder size={15} className="prow-folder" aria-hidden="true" />}
        <span className="ellipsis grow">{p.name}</span>
        {pinned && <Pin size={12} className="prow-pin" aria-label={t('sess.pinned')} />}
        {attn && <span className="sdot sdot-attn" aria-label={t('sess.state.attn')} role="img" />}
        {showDevice && dev && <DeviceTag device={dev} />}
        {merged && <DeviceDots members={members} />}
        {p.offline && <span className="badge">{t('devices.offline')}</span>}
      </button>
      {!p.offline && (
        <span className="prow-acts">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => ui.draftIn(p)} disabled={!allowed} aria-label={t('draft.new_in', { name: p.name })} title={t('sess.new')}>
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

/** 搜以前的对话：标题、你说的最后一句、分支、工具。 */
function convMatcher(q, t) {
  const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return (c) => {
    const hay = [c.title, c.last, c.branch, c.agent, t(`launch.${c.agent}.title`)].filter(Boolean).join(' ').toLowerCase();
    return words.every((w) => hay.includes(w));
  };
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

/** 看哪几台电脑：空数组是全部。旧版存的是一个 id 字符串（'all' 或某台电脑）。 */
function readDevFilter() {
  const v = readStore(localStorage, DEVF_KEY, []);
  if (Array.isArray(v)) return v.filter((x) => typeof x === 'string');
  return typeof v === 'string' && v !== 'all' ? [v] : [];
}
const STATUS = ['all', 'attn', 'running', 'rest'];
const GROUPS = ['project', 'device', 'recent'];

/** 点到外面、按 Esc 时关掉弹出的筛选。 */
function usePopover() {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const down = (e) => !ref.current?.contains(e.target) && setOpen(false);
    const key = (e) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('pointerdown', down);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('pointerdown', down);
      document.removeEventListener('keydown', key);
    };
  }, [open]);
  return { open, setOpen, ref };
}

/**
 * 电脑筛选（docs/UI_REDESIGN_2026-10.md §2.3）：一个下拉，可以多选、搜、只看在线的；每行有颜色、系统、连接状态和会话个数。
 * `value` 是选中的电脑（空 = 全部），`counts` 是每台的会话个数。
 */
function DeviceFilter({ value, onChange, online, onOnline, counts }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const links = useLinks();
  const { open, setOpen, ref } = usePopover();
  const [q, setQ] = useState('');
  const all = value.length === 0;
  const picked = devices.items.filter((d) => value.includes(d.id));
  const label = all ? t('sess.all_devices') : picked.length === 1 ? deviceName(t, picked[0]) : t('sess.n_devices_picked', { count: picked.length });
  const words = q.trim().toLowerCase();
  const shown = devices.items.filter((d) => !words || [deviceName(t, d), d.os, osLabel(d.os)].filter(Boolean).join(' ').toLowerCase().includes(words));
  const toggle = (id) => {
    const next = all ? [id] : value.includes(id) ? value.filter((x) => x !== id) : [...value, id];
    onChange(next.length === devices.items.length ? [] : next);
  };
  const active = !all || online;
  return (
    <div className="sfilter" ref={ref}>
      <button type="button" className={`sfilter-chip ${active ? 'on' : ''}`} onClick={() => setOpen(!open)} aria-expanded={open} aria-haspopup="dialog">
        {picked.length === 1 ? <DeviceAvatar device={picked[0]} size={16} /> : <Laptop size={14} aria-hidden="true" />}
        <span className="ellipsis">{label}</span>
        {online && <span className="sfilter-note">{t('sess.online_only_short')}</span>}
        <ChevronDown size={13} aria-hidden="true" />
      </button>
      {active && (
        <button type="button" className="sfilter-clear" onClick={() => { onChange([]); onOnline(false); }} aria-label={t('sess.filter_clear')} title={t('sess.filter_clear')}>
          <X size={12} />
        </button>
      )}
      {open && (
        <div className="sfilter-pop" role="dialog" aria-label={t('sess.dev_filter')}>
          {devices.items.length > 6 && (
            <label className="slist-search sfilter-search">
              <Search size={14} aria-hidden="true" />
              <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('sess.dev_search')} aria-label={t('sess.dev_search')} spellCheck={false} autoFocus />
            </label>
          )}
          <button type="button" className="sfilter-row" role="menuitemcheckbox" aria-checked={all} onClick={() => onChange([])}>
            <span className={`sfilter-box ${all ? 'on' : ''}`} aria-hidden="true">{all && <Check size={12} />}</span>
            <span className="grow">{t('sess.all_devices')}</span>
            <span className="sfilter-count">{Object.values(counts).reduce((a, b) => a + b, 0)}</span>
          </button>
          <div className="sfilter-list">
            {shown.map((d) => {
              const on = !all && value.includes(d.id);
              const state = linkState(d, links);
              return (
                <div key={d.id} className="sfilter-line">
                  <button type="button" className="sfilter-row" role="menuitemcheckbox" aria-checked={on} onClick={() => toggle(d.id)}>
                    <span className={`sfilter-box ${on ? 'on' : ''}`} aria-hidden="true">{on && <Check size={12} />}</span>
                    <DeviceAvatar device={d} size={20} dot />
                    <span className="grow sfilter-name">
                      <span className="ellipsis">{deviceName(t, d)}</span>
                      <span className="sfilter-sub ellipsis">{[osLabel(d.os), state === 'online' ? t('devices.online') : linkLabel(t, d, links)].filter(Boolean).join(' · ')}</span>
                    </span>
                    <span className="sfilter-count">{counts[d.id] ?? 0}</span>
                  </button>
                  <button type="button" className="sfilter-only" onClick={() => { onChange([d.id]); setOpen(false); }} title={t('sess.only_this')}>
                    {t('sess.only')}
                  </button>
                </div>
              );
            })}
            {shown.length === 0 && <p className="slist-note">{t('sess.no_match')}</p>}
          </div>
          <label className="sfilter-toggle">
            <input type="checkbox" checked={online} onChange={(e) => onOnline(e.target.checked)} />
            {t('sess.online_only')}
          </label>
        </div>
      )}
    </div>
  );
}

/** 状态筛选：全部 / 等你处理 / 正在运行 / 停了的和以前的对话。 */
function StatusFilter({ value, onChange }) {
  const { t } = useTranslation();
  const { open, setOpen, ref } = usePopover();
  const on = value !== 'all';
  return (
    <div className="sfilter" ref={ref}>
      <button type="button" className={`sfilter-chip ${on ? 'on' : ''}`} onClick={() => setOpen(!open)} aria-expanded={open} aria-haspopup="menu">
        <span className={`sdot sdot-${value === 'attn' ? 'attn' : value === 'running' ? 'working' : value === 'rest' ? 'stopped' : 'idle'}`} aria-hidden="true" />
        <span className="ellipsis">{t(`sess.status.${value}`)}</span>
        <ChevronDown size={13} aria-hidden="true" />
      </button>
      {on && (
        <button type="button" className="sfilter-clear" onClick={() => onChange('all')} aria-label={t('sess.filter_clear')} title={t('sess.filter_clear')}>
          <X size={12} />
        </button>
      )}
      {open && (
        <div className="sfilter-pop sfilter-pop-sm" role="menu">
          {STATUS.map((k) => (
            <button key={k} type="button" className="sfilter-row" role="menuitemradio" aria-checked={value === k} onClick={() => { onChange(k); setOpen(false); }}>
              <Check size={14} aria-hidden="true" style={{ visibility: value === k ? 'visible' : 'hidden' }} />
              <span className="grow">{t(`sess.status.${k}`)}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** 项目和会话列表（电脑上的会话栏、手机首页共用）：标题、搜索、电脑和状态筛选，下面是等你处理、项目。 */
export function SessionList({ currentId, onCollapse }) {
  const links = useLinks();
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const [query, setQueryState] = useState(() => readStore(sessionStorage, QUERY_KEY, ''));
  const setQuery = (q) => {
    setQueryState(q);
    writeStore(sessionStorage, QUERY_KEY, q);
  };
  const [groupBy, setGroupByState] = useState(() => {
    const v = readStore(localStorage, GROUP_KEY, 'project');
    return GROUPS.includes(v) ? v : 'project';
  });
  const setGroupBy = (v) => {
    setGroupByState(v);
    writeStore(localStorage, GROUP_KEY, v);
  };
  const [devFilter, setDevFilterState] = useState(readDevFilter);
  const setDevFilter = (v) => {
    setDevFilterState(v);
    writeStore(localStorage, DEVF_KEY, v);
  };
  const [onlineOnly, setOnlineState] = useState(() => readStore(localStorage, ONLINE_KEY, false) === true);
  const setOnline = (v) => {
    setOnlineState(v);
    writeStore(localStorage, ONLINE_KEY, v);
  };
  const [status, setStatusState] = useState(() => {
    const v = readStore(sessionStorage, STATUS_KEY, 'all');
    return STATUS.includes(v) ? v : 'all';
  });
  const setStatus = (v) => {
    setStatusState(v);
    writeStore(sessionStorage, STATUS_KEY, v);
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
  const isOpen = (x) => fold[x.key] ?? (Math.max(x.used, x.convAt) > weekAgo || x.items.some((i) => i.id === currentId));
  const match = matcher(query, t);
  const convMatch = convMatcher(query, t);
  const multi = devices.multi;
  // 选中的电脑里已经不在列表里的（换了账户、删掉了）不算
  const picked = multi ? devFilter.filter((id) => devices.items.some((d) => d.id === id)) : [];
  const devOk = (dev) => !multi || ((picked.length === 0 || picked.includes(dev)) && (!onlineOnly || devices.byId(dev)?.online));
  // 状态筛选：等你处理、正在运行只看在运行的会话；“停了的和以前的对话”只看其余的
  const instOk = (i) => (status === 'attn' ? sessionState(i) === 'attn' : status === 'running' ? isLive(i) : status === 'rest' ? !isLive(i) : true) && (!match || match(i));
  const restOk = status !== 'attn' && status !== 'running';
  const filtering = Boolean(match) || status !== 'all';
  // 搜索、按状态筛选时项目默认都展开（符合的都看得到），点了也能收起；只记在这一次筛选里，不改平时的折叠
  const [filterFold, setFilterFold] = useState({});
  const filterKey = `${query.trim()}|${status}`;
  const [seenFilter, setSeenFilter] = useState(filterKey);
  if (seenFilter !== filterKey) {
    setSeenFilter(filterKey);
    setFilterFold({});
  }
  // 按项目看时，多台电脑上的同一个仓库合成一个项目（只在选中的电脑里合，见 groupProjects）
  const mine = s.projects.filter((p) => devOk(p.device));
  const groups = multi && groupBy === 'project' ? groupProjects(mine, true) : mine.map((p) => ({ key: p.key, name: p.name, members: [p], primary: p }));
  // 每个项目合成的会话列表（见 projectEntries），每一行记着是哪台电脑上的（r.m）；离线那台的排在后面。
  // 搜索、按状态筛选时只留符合的，项目名符合搜索的整个留着
  const byAt = (a, b) => b.at - a.at;
  const projects = groups
    .map((g) => {
      const parts = g.members.map((m) => {
        const e = projectEntries(m);
        const tag = (r) => ({ ...r, m });
        const live = e.live.map(tag).filter((r) => instOk(r.inst));
        const rest = restOk ? e.rest.map(tag).filter((r) => (r.kind === 'inst' ? instOk(r.inst) : !match || convMatch(r.conv))) : [];
        return { m, live, rest, count: e.count };
      });
      const ms = g.members;
      return {
        key: g.key,
        p: g.primary,
        members: ms,
        merged: ms.length > 1,
        live: parts.flatMap((x) => x.live),
        rest: [...parts.filter((x) => !x.m.offline).flatMap((x) => x.rest).sort(byAt), ...parts.filter((x) => x.m.offline).flatMap((x) => x.rest).sort(byAt)],
        count: parts.reduce((n, x) => n + x.count, 0),
        pinned: ms.some((m) => m.pinned),
        active: ms.some((m) => m.active),
        used: Math.max(...ms.map((m) => m.used)),
        convAt: Math.max(...ms.map((m) => m.convAt)),
        items: ms.flatMap((m) => m.items),
      };
    })
    .filter((x) => (!filtering ? true : x.live.length + x.rest.length > 0 || (match && status === 'all' && x.p.name.toLowerCase().includes(query.trim().toLowerCase()))));
  const attn = status === 'rest' ? [] : s.items.filter((i) => sessionState(i) === 'attn' && devOk(multi ? terminalDevice(i.id) : null) && (!match || match(i)));
  // 每台电脑的会话个数（电脑筛选里显示）
  const counts = {};
  for (const p of s.projects) counts[p.device] = (counts[p.device] ?? 0) + p.items.length;
  const allOpen = projects.some((x) => isOpen(x));
  const foldAll = () =>
    setFoldState(() => {
      const next = Object.fromEntries(groups.map((g) => [g.key, !allOpen]));
      writeStore(localStorage, FOLD_KEY, next);
      return next;
    });
  // 一个项目：在运行的全列，其余（停了的终端、以前的对话）列最近几个，再多的去项目页
  const renderProject = (x, showDevice) => {
    const { p } = x;
    const drafting = x.members.some((m) => isDraftProject(m, ui.draftAt));
    const open = drafting || (filtering ? (filterFold[x.key] ?? true) : isOpen(x));
    const toggle = () => (filtering ? setFilterFold((f) => ({ ...f, [x.key]: !open })) : setFold(x.key, !open));
    const shown = filtering ? x.rest : x.rest.slice(0, SIDE_SHOW);
    const more = filtering ? 0 : x.count - x.live.length - shown.length;
    // 合并的项目：每行标上是哪台电脑的
    const row = (r) =>
      r.kind === 'inst' ? <SessionRow key={r.key} inst={r.inst} current={r.inst.id === currentId} devTag={x.merged} /> : <ConvSideRow key={r.key} p={r.m} conv={r.conv} devTag={x.merged} />;
    return (
      <section key={x.key} className="sproject" aria-label={p.name}>
        <ProjectRow p={p} x={x} open={open} onToggle={toggle} showDevice={showDevice} />
        {open && p.offline && (
          <p className="slist-note">
            {Date.now() - (p.seenAt ?? 0) > 60000 && p.seenAt ? t('sess.offline_note_at', { count: p.count ?? 0, when: shortAgo(t, p.seenAt) }) : t('sess.offline_note', { count: p.count ?? 0 })}
          </p>
        )}
        {drafting && <DraftSideRow />}
        {open && [...x.live, ...shown].filter((r) => !r.m.offline || r.kind === 'conv').map(row)}
        {open && more > 0 && !p.offline && (
          <Link to={projectPath(p)} className="slist-more">
            {t('sess.all_in_project', { count: x.count })}
          </Link>
        )}
      </section>
    );
  };
  let body;
  if (s.loading) {
    body = <div className="slist-loading"><Spinner size={16} /></div>;
  } else if (groupBy === 'recent') {
    // 按时间：不分项目，在运行的和停了的、以前的对话放在一起按最近用过排
    const rows = projects.flatMap((x) => [...x.live, ...x.rest].map((r) => ({ ...r, x }))).filter((r) => !r.m.offline || r.kind === 'conv');
    rows.sort((a, b) => (a.kind === 'inst' && isLive(a.inst) ? 0 : 1) - (b.kind === 'inst' && isLive(b.inst) ? 0 : 1) || b.at - a.at);
    const max = filtering ? rows.length : 60;
    body = (
      <section className="sproject srecent">
        {rows.slice(0, max).map((r) =>
          r.kind === 'inst' ? <SessionRow key={r.key} inst={r.inst} current={r.inst.id === currentId} attn devTag={multi} /> : <ConvSideRow key={r.key} p={r.m} conv={r.conv} devTag={multi} />,
        )}
        {rows.length > max && (
          <button type="button" className="slist-more" onClick={() => navigate('/history')}>
            {t('sess.recent_more', { count: rows.length - max })}
          </button>
        )}
      </section>
    );
  } else if (groupBy === 'device' && multi) {
    body = devices.items
      .filter((d) => devOk(d.id))
      .map((d) => {
        const ps = projects.filter((x) => x.p.device === d.id);
        const key = `dev:${d.id}`;
        const open = fold[key] ?? true;
        const group = s.groups?.[d.id];
        if (filtering && ps.length === 0) return null;
        return (
          <section key={d.id} className="sdevice">
            <DeviceHead device={d} count={ps.reduce((n, x) => n + x.items.length, 0)} open={open} onToggle={() => setFold(key, !open)} />
            {open && group?.error && <ErrorNote error={group.error} />}
            {open && ps.map((x) => renderProject(x, false))}
            {open && d.online && ps.length === 0 && <p className="slist-note">{t('devices.no_terminals')}</p>}
          </section>
        );
      });
  } else {
    // 项目多了：一个月没用过的（没置顶、没在运行、没有要处理的）收进最后一组
    const monthAgo = Date.now() - MONTH;
    const isOld = (x) => !x.pinned && !x.active && Math.max(x.used, x.convAt) < monthAgo && !x.items.some((i) => i.id === currentId || sessionState(i) === 'attn');
    const old = !filtering && projects.length > OLD_MIN ? projects.filter(isOld) : [];
    const oldOpen = fold.old ?? false;
    const showDevice = multi && picked.length !== 1;
    body = (
      <>
        {projects.filter((x) => !old.includes(x)).map((x) => renderProject(x, showDevice))}
        {old.length > 0 && (
          <button type="button" className="slist-old" onClick={() => setFold('old', !oldOpen)} aria-expanded={oldOpen}>
            <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
            {t('sess.old_projects', { count: old.length })}
          </button>
        )}
        {oldOpen && old.map((x) => renderProject(x, showDevice))}
      </>
    );
  }
  const viewMenu = [
    { head: t('sess.group_label') },
    ...GROUPS.filter((g) => g !== 'device' || multi).map((g) => ({ label: t(`sess.group.${g}`), checked: groupBy === g, onClick: () => setGroupBy(g) })),
    'sep',
    groupBy !== 'recent' && projects.length > 0 && { icon: allOpen ? ChevronsDownUp : ChevronsUpDown, label: t(allOpen ? 'sess.fold_all' : 'sess.unfold_all'), onClick: foldAll },
    { icon: FolderPlus, label: t('sess.add_project'), onClick: () => ui.addProject() },
    { icon: History, label: t('hist.title'), onClick: () => navigate('/history') },
  ];
  const searchable = s.items.length > 0 || s.projects.length > 3 || s.projects.some((p) => p.total > 0);
  return (
    <div className="slist">
      <div className="slist-top">
        <div className="slist-titlebar">
          <h2 className="slist-title grow">{t('sess.title')}</h2>
          <NewProjectButton />
          <PopMenu items={viewMenu} className="btn btn-plain btn-icon" icon={SlidersHorizontal} label={t('sess.view_menu')} sheet />
          {/* 电脑上的会话栏：收起（收起后点图标栏的“会话”展开） */}
          {onCollapse && (
            <button type="button" className="btn btn-plain btn-icon" onClick={onCollapse} aria-label={t('sess.collapse_side')} title={`${t('sess.collapse_side')} (${keyLabel('mod+shift+S')})`}>
              <PanelLeftClose size={16} />
            </button>
          )}
        </div>
        {searchable && (
          <label className="slist-search">
            <Search size={15} aria-hidden="true" />
            <input type="search" value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === 'Escape' && setQuery('')} placeholder={t('sess.search')} aria-label={t('sess.search')} spellCheck={false} />
            <button type="button" className="slist-kbd" onClick={(e) => { e.preventDefault(); window.dispatchEvent(new Event('ll-quick-switch')); }} title={t('qs.title')}>
              {keyLabel('mod+K')}
            </button>
          </label>
        )}
        {(multi || searchable) && (
          <div className="sfilters" role="group" aria-label={t('sess.filters')}>
            {multi && <DeviceFilter value={picked} onChange={setDevFilter} online={onlineOnly} onOnline={setOnline} counts={counts} />}
            <StatusFilter value={status} onChange={setStatus} />
          </div>
        )}
      </div>
      <div className="slist-body">
        {s.error && <ErrorNote error={s.error} />}
        {attn.length > 0 && groupBy !== 'recent' && (
          <section className="sattn" aria-label={t('sess.attention')}>
            <h3 className="slist-label slist-label-warn">
              <AlertCircle size={13} aria-hidden="true" />
              {t('sess.attention')} · {attn.length}
            </h3>
            {attn.map((i) => <SessionRow key={i.id} inst={i} attn current={i.id === currentId} />)}
          </section>
        )}
        {groupBy !== 'recent' && projects.length > 0 && <h3 className="slist-label slist-label-head">{t(groupBy === 'device' && multi ? 'sess.by_device' : 'sess.projects')}</h3>}
        {body}
        {!s.loading && filtering && projects.length === 0 && attn.length === 0 && <p className="slist-note">{t('sess.no_match')}</p>}
        {match && (
          <button type="button" className="btn btn-plain slist-hist" onClick={() => navigate(`/history?q=${encodeURIComponent(query.trim())}`)}>
            <History size={14} aria-hidden="true" />
            <span className="ellipsis">{t('hist.search_in', { q: query.trim() })}</span>
          </button>
        )}
        {!s.loading && !filtering && s.projects.length === 0 && (
          <div className="slist-empty">
            <p className="slist-note">{t('sess.empty_projects')}</p>
            <button type="button" className="btn btn-sm slist-add" onClick={() => ui.addProject()}>
              <FolderPlus size={15} aria-hidden="true" />
              {t('sess.add_project')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function NewProjectButton() {
  const { t } = useTranslation();
  const ui = useSessionUi();
  return (
    <button type="button" className="btn btn-plain btn-icon" onClick={() => ui.addProject()} aria-label={t('sess.add_project')} title={t('sess.add_project_hint')}>
      <FolderPlus size={16} />
    </button>
  );
}

/** 手机首页：会话列表（新会话在底部标签栏中间）。 */
export function SessionHome() {
  return (
    <div className="shome">
      <BackendNote />
      <SessionList />
    </div>
  );
}

// ---------------- 右边打开的会话 ----------------

/** 电脑上会话页由 CSS 撑满（.console-sess，见 styles.css），不用量高度。 */
const DESK = '(min-width: 900px)';

/**
 * 手机上让元素正好填满窗口剩下的高度（上面的横幅、提示有多高都行）。
 * 电脑上量出来的高度会过时（上面的横幅收起时 body 高度不一定变），输入框离底部留一大块空白，改由 CSS 布局撑满。
 */
function useFillHeight(ref, enabled) {
  const [h, setH] = useState(null);
  useLayoutEffect(() => {
    if (!enabled) return undefined;
    const desk = window.matchMedia(DESK);
    const on = () => {
      const el = ref.current;
      if (!el) return;
      if (desk.matches) {
        setH(null);
        return;
      }
      const top = el.getBoundingClientRect().top + window.scrollY;
      setH(Math.max(320, window.innerHeight - top));
    };
    on();
    const ro = new ResizeObserver(on);
    ro.observe(document.body);
    window.addEventListener('resize', on);
    desk.addEventListener?.('change', on);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', on);
      desk.removeEventListener?.('change', on);
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
        if (inst?.launch === 'dsh') {
          return (
            <WebPane
              key={id}
              id={id}
              inst={inst}
              missing={missing(id)}
              active={id === activeId}
              lead={<LaunchIcon launch="dsh" size={16} />}
              sub={<SessionLead inst={inst} />}
              named={!opensInPage()}
              menu={<SessionMenu inst={inst} />}
            />
          );
        }
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
            named={!opensInPage()}
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
  const ctxRef = useRef(null);
  const allRef = useRef(null);
  useDismiss(ctxRef, Boolean(ctx), () => setCtx(null));
  useDismiss(allRef, all, () => setAll(false));
  useEffect(() => {
    bar.current?.querySelector('.stab.on')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }, [activeId, s.tabs.length]);
  // ⌘W / Ctrl+W 关掉当前标签。焦点在终端里时按键进不来（Ctrl+W 留给终端删词）；
  // Mac 桌面版由原生菜单发 `ll-menu`（src/desktop.rs），在终端里也能用。浏览器普通标签页里 Ctrl+W 是浏览器自己的
  const closeRef = useRef(null);
  useEffect(() => {
    const run = (e) => {
      if (!closeRef.current?.()) return;
      e.preventDefault?.();
    };
    const onKey = (e) => isMod(e, 'w') && run(e);
    const onMenu = (e) => e.detail === 'close_tab' && run(e);
    window.addEventListener('keydown', onKey);
    window.addEventListener('ll-menu', onMenu);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('ll-menu', onMenu);
    };
  }, []);
  closeRef.current = () => {
    if (!opensInPage() || !activeId || !s.tabs.includes(activeId)) return false;
    close([activeId]);
    return true;
  };
  if (!opensInPage() || s.tabs.length === 0) return null;
  function close(ids) {
    setCtx(null);
    s.closeTabs(ids);
    if (!activeId || !ids.includes(activeId)) return;
    // 关掉的是当前的：切到右边的，没有就左边的，都没有回开始页
    const at = s.tabs.indexOf(activeId);
    const keep = (x) => !ids.includes(x);
    const next = s.tabs.slice(at + 1).find(keep) ?? s.tabs.slice(0, at).reverse().find(keep);
    if (next) ui.openSession(next);
    else navigate('/');
  }
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
        {ui.draftAt && (
          <div className="stab on stab-draft">
            <span className="stab-main" role="tab" aria-selected="true">
              <span className="draft-tile" aria-hidden="true" />
              <span className="ellipsis">{t('sess.new')}</span>
            </span>
            <button type="button" className="stab-x" onClick={() => s.tabs.length && ui.openSession(s.tabs[s.tabs.length - 1])} aria-label={t('draft.close')} title={t('draft.close')}>
              <X size={13} />
            </button>
          </div>
        )}
      </div>
      <button type="button" className="btn btn-plain btn-icon stabs-new" onClick={() => ui.newSession()} aria-label={t('sess.tab_new')} title={t('sess.tab_new_hint')}>
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
