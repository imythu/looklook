// 会话（首页）：左侧栏里的项目和会话列表、开始页（新会话）、右边打开的会话。
// 像 ChatGPT：点“新会话”进开始页，选好类型和项目、写一句话（可以不写），回车就启动并打开；项目上的 + 一下就新建。
// 项目 = 某台电脑上的一个文件夹（见 shared/sessions.jsx）。电脑用颜色 + 系统图标区分（shared/brands.jsx）。
// 电脑上会话在右边打开，最近打开的几个在后台保持连接；手机上整页打开 /t/{id}（有按键条）。
import { AlertCircle, ArrowLeft, ArrowUp, ChevronDown, ChevronsDownUp, ChevronsUpDown, ExternalLink, Folder, FolderPlus, Plus, Search, SlidersHorizontal, Trash2 } from 'lucide-react';
import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { SafetyNote, useStatus } from '../App.jsx';
import { deviceApi, rememberTerminals, terminalDevice } from '../shared/api.js';
import { DeviceAvatar, deviceColor, DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, osLabel, RenameDeviceDialog, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { isTouch } from '../shared/KeyBar.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { useRouter } from '../shared/router.jsx';
import { baseName, lastUsed, sessionState, useSessions } from '../shared/sessions.jsx';
import { Button, ErrorNote, Spinner, Switch, useLoad, useToast } from '../shared/ui.jsx';
import { BackendNote, FolderPicker, InstanceDialog, LogsDialog, openDsh, Promotions, RemoteCard, ShellPicker, useInstanceActions } from './Terminals.jsx';
import { TermPane } from './TerminalView.jsx';

// 开始页能选的类型（DSH 一台电脑只有一个，已经有了就是打开它）
const START_LAUNCHES = ['claude', 'codex', 'opencode', 'shell', 'dsh', 'custom'];
// 项目“⋯”里换个类型新建
const QUICK_LAUNCHES = ['claude', 'codex', 'opencode', 'shell'];
const AI_TOOLS = ['codex', 'claude', 'opencode', 'dsh'];
const FULL_ACCESS = ['codex', 'claude', 'opencode'];
// 第一句话能用在哪些类型上（AI 工具作为第一条消息，普通终端当作启动后运行的命令）
const PROMPT_LAUNCHES = ['claude', 'codex', 'opencode', 'shell'];

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
  const toast = useToast();
  const [dialog, setDialog] = useState(null);
  const [logs, setLogs] = useState(null);
  // 开始页要预先填好的（在某个项目里点“更多选项”时）；nonce 让开始页重新初始化
  const [start, setStart] = useState({ nonce: 0 });
  const openSession = (id) => {
    if (!opensInPage()) return navigate(`/t/${id}`);
    s.openRecent(id);
    if (path !== `/s/${id}`) navigate(`/s/${id}`);
  };
  const newSession = (want = {}) => {
    setStart((x) => ({ ...want, nonce: x.nonce + 1 }));
    navigate(opensInPage() ? '/' : '/new');
  };
  /** 在项目里直接新建一个（类型默认用这个项目上次用的），启动并打开；提示条里可以撤销。 */
  const quickCreate = async (p, launch = p.launch || 'claude') => {
    const client = deviceApi(p.device);
    try {
      const tools = AI_TOOLS.includes(launch) ? await client.get('/tools').catch(() => null) : null;
      if (tools && !tools.tools?.[launch]) {
        newSession({ device: p.device, workdir: p.workdir, launch });
        return;
      }
      const r = await client.post('/instances', {
        name: '',
        workdir: p.workdir,
        launch,
        command: '',
        shell: p.shell ?? '',
        auto_start: Boolean(p.auto_start),
        full_access: FULL_ACCESS.includes(launch) && Boolean(p.full_access),
        start: true,
      });
      if (p.device && r.id) rememberTerminals(p.device, [r]);
      s.touchProject(p.device, p.workdir, { launch });
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
  const value = {
    openSession,
    newSession,
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
  return (
    <div className={`srow ${attn ? 'srow-attn' : ''} ${current ? 'current' : ''}`}>
      <button type="button" className="srow-main" onClick={a.primary} disabled={a.busy || !a.allowed} aria-current={current ? 'page' : undefined} title={`${inst.name}${inst.workdir ? `\n${inst.workdir}` : ''}`}>
        <LaunchIcon launch={inst.launch} size={16} className="srow-icon" />
        <span className="srow-text">
          <span className="ellipsis">{inst.name}</span>
          {attn && (
            <span className="srow-sub">
              <span className="ellipsis">{p?.name ?? baseName(inst.workdir)}</span>
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
    { icon: SlidersHorizontal, label: t('sess.more_options'), onClick: () => ui.newSession({ device: p.device, workdir: p.workdir }), disabled: !allowed },
    p.items.length === 0 && { icon: Trash2, label: t('sess.remove_project'), onClick: () => s.removeProject(p.device, p.workdir), danger: true },
  ];
  return (
    <div className={`prow ${p.offline ? 'offline' : ''}`} style={dev ? { '--dc': deviceColor(devices, dev.id) } : undefined}>
      <button type="button" className="prow-main" onClick={onToggle} aria-expanded={open} title={`${p.workdir}${dev ? ` · ${deviceName(t, dev)}` : ''}`}>
        <ChevronDown size={14} className="prow-chev" aria-hidden="true" />
        <Folder size={15} className="prow-folder" aria-hidden="true" />
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
  return (
    <div className="dhead">
      <button type="button" className="dhead-main" onClick={onToggle} aria-expanded={open}>
        <DeviceAvatar device={device} size={26} dot />
        <span className="dhead-text">
          <b className="ellipsis">{deviceName(t, device)}</b>
          <span className="small muted">
            {[osLabel(device.os), device.online ? t('devices.count', { count }) : t('devices.offline')].filter(Boolean).join(' · ')}
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
  );
}

/** 搜索：名称、文件夹、类型、命令都算。 */
function matcher(q, t) {
  const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return null;
  return (i) => {
    const hay = [i.name, i.workdir, i.command, i.launch, t(`launch.${i.launch}.title`)].filter(Boolean).join(' ').toLowerCase();
    return words.every((w) => hay.includes(w));
  };
}

/** 项目和会话列表（左侧栏、手机首页共用）。 */
export function SessionList({ currentId }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const s = useSessions();
  const [query, setQueryState] = useState(() => readStore(sessionStorage, QUERY_KEY, ''));
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
      {(s.items.length > 0 || s.projects.length > 3) && (
        <label className="slist-search">
          <Search size={15} aria-hidden="true" />
          <input type="search" value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === 'Escape' && setQuery('')} placeholder={t('sess.search')} aria-label={t('sess.search')} spellCheck={false} />
        </label>
      )}
      {multi && (
        <div className="devbar" role="group" aria-label={t('sess.dev_filter')}>
          <button type="button" aria-pressed={filter === 'all'} onClick={() => setDevFilter('all')}>
            {t('sess.all_devices')}
          </button>
          {devices.items.map((d) => (
            <button key={d.id} type="button" aria-pressed={filter === d.id} onClick={() => setDevFilter(filter === d.id ? 'all' : d.id)} style={{ '--dc': deviceColor(devices, d.id) }}>
              <DeviceAvatar device={d} size={18} />
              <span className="ellipsis">{deviceName(t, d)}</span>
              {!d.online && <span className="muted">{t('devices.offline')}</span>}
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

/** 选项目：按电脑列出最近的项目，最下面“选择其他文件夹”。 */
function ProjectPicker({ projects, device, workdir, onPick, onClose }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const ref = useRef(null);
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
              <button key={p.key} type="button" className="ppick-item" aria-pressed={p.device === device && p.workdir === workdir} onClick={() => onPick(p.device, p.workdir)}>
                <Folder size={15} aria-hidden="true" />
                <span className="ellipsis grow">{p.name}</span>
                <span className="ppick-path mono ellipsis">{p.workdir}</span>
              </button>
            ))}
            <button type="button" className="ppick-item ppick-other" onClick={() => setBrowse(d?.id ?? null)}>
              <FolderPlus size={15} aria-hidden="true" />
              <span className="grow">{t('sess.pick_other')}</span>
            </button>
          </div>
        );
      })}
      <FolderPicker
        open={browse !== null}
        initial={browse === device ? workdir : ''}
        client={deviceApi(browse)}
        onClose={() => setBrowse(null)}
        onPick={(path) => onPick(browse, path)}
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

function StartForm() {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const { status: selectedStatus } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const want = ui.start;
  // 默认：要求的 > 最近用过的在线项目 > 默认文件夹
  const firstProject = s.projects.find((p) => !p.offline);
  const initDevice = want.device !== undefined ? want.device : firstProject ? firstProject.device : devices.multi ? devices.selected : null;
  const [device, setDevice] = useState(initDevice);
  const [workdir, setWorkdir] = useState(want.workdir ?? (want.device === undefined && firstProject ? firstProject.workdir : ''));
  const project = s.projects.find((p) => p.device === device && p.workdir === workdir);
  const [launch, setLaunch] = useState(want.launch ?? project?.launch ?? 'claude');
  const [text, setText] = useState('');
  const [picking, setPicking] = useState(Boolean(want.pick));
  const [adv, setAdv] = useState(false);
  const [name, setName] = useState('');
  const [shell, setShell] = useState(project?.shell ?? '');
  const [fullAccess, setFullAccess] = useState(Boolean(project?.full_access));
  const [rootOk, setRootOk] = useState(false);
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
  useEffect(() => {
    if (!picking) input.current?.focus();
  }, [want.nonce, picking]);
  // 换了项目：类型和高级选项跟着这个项目上次的
  const pickProject = (dev, dir) => {
    setDevice(dev);
    setWorkdir(dir);
    setPicking(false);
    const p = s.projects.find((x) => x.device === dev && x.workdir === dir);
    if (p) {
      setLaunch(p.launch || launch);
      setShell(p.shell ?? '');
      setFullAccess(Boolean(p.full_access));
      setAutoStart(Boolean(p.auto_start));
    }
    setTimeout(() => input.current?.focus(), 0);
  };
  const missing = (l) => AI_TOOLS.includes(l) && tools && !tools.tools?.[l];
  const dshRow = launch === 'dsh' ? s.items.find((i) => i.launch === 'dsh' && (!devices.multi || terminalDevice(i.id) === device)) : null;
  const isRoot = Boolean(status.capabilities?.root);
  const canFull = FULL_ACCESS.includes(launch);
  const needRoot = canFull && fullAccess && isRoot && !rootOk;
  const allowed = selectedStatus.account.allowed;
  const devObj = devices.multi ? devices.byId(device) : null;
  const offline = devices.multi && !devObj?.online;
  const takesText = PROMPT_LAUNCHES.includes(launch) || launch === 'custom';
  const go = async () => {
    if (busy || !allowed || offline) return;
    setError(null);
    if (missing(launch)) {
      ui.install(launch, device, workdir);
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
    if (needRoot) {
      setAdv(true);
      return;
    }
    const prompt = PROMPT_LAUNCHES.includes(launch) ? text.trim() : '';
    setBusy(true);
    try {
      const r = await client.post('/instances', {
        // 没填名称时用第一句话的开头（AI 工具），否则交给电脑取（文件夹 · 工具）
        name: (name.trim() || (prompt && launch !== 'shell' ? prompt.replace(/\s+/g, ' ').slice(0, 20) : '')).slice(0, 40),
        workdir,
        launch,
        command: launch === 'custom' ? text.trim() : '',
        prompt: prompt || undefined,
        shell: shell.trim(),
        auto_start: autoStart,
        full_access: canFull && fullAccess,
        root_confirmed: canFull && fullAccess && isRoot && rootOk,
        start: true,
      });
      if (device && r.id) rememberTerminals(device, [r]);
      s.touchProject(device, r.workdir ?? workdir, { launch, shell: shell.trim(), full_access: canFull && fullAccess, auto_start: autoStart });
      s.reload();
      setText('');
      setName('');
      if (launch === 'dsh') openDsh(r, t, toast);
      else ui.openSession(r.id);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const placeholder = dshRow ? t('sess.ph_dsh_open') : t(`sess.ph_${launch}`, { tool: t(`launch.${launch}.title`) });
  const projName = baseName(workdir) || workdir || '…';
  return (
    <div className="start">
      {!opensInPage() && (
        <button type="button" className="btn btn-plain start-back" onClick={() => navigate('/')}>
          <ArrowLeft size={18} aria-hidden="true" />
          {t('sess.back')}
        </button>
      )}
      <h1 className="start-title">{workdir ? t('sess.start_in', { name: projName }) : t('sess.start_title')}</h1>
      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          go();
        }}
      >
        <div className="start-types" role="group" aria-label={t('form.launch')}>
          {START_LAUNCHES.map((l) => (
            <button key={l} type="button" className="start-type" aria-pressed={launch === l} onClick={() => setLaunch(l)} title={t(`launch.${l}.desc`)}>
              <LaunchIcon launch={l} size={16} />
              {t(`launch.${l}.title`)}
              {missing(l) && <span className="start-miss">{t('sess.not_installed')}</span>}
            </button>
          ))}
        </div>
        {takesText && !dshRow ? (
          <textarea
            ref={input}
            className="start-input"
            rows={3}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                go();
              }
            }}
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
            <button type="button" className="start-loc" onClick={() => setPicking(!picking)} aria-expanded={picking} title={workdir}>
              {devObj ? <DeviceAvatar device={devObj} size={18} /> : <Folder size={15} aria-hidden="true" />}
              <span className="ellipsis">{projName}</span>
              {devObj && <span className="muted ellipsis start-loc-dev">{deviceName(t, devObj)}</span>}
              <ChevronDown size={14} aria-hidden="true" />
            </button>
            {picking && <ProjectPicker projects={s.projects} device={device} workdir={workdir} onPick={pickProject} onClose={() => setPicking(false)} />}
          </span>
          {launch !== 'dsh' && (
            <button type="button" className="btn btn-plain start-adv" onClick={() => setAdv(!adv)} aria-expanded={adv}>
              <SlidersHorizontal size={15} aria-hidden="true" />
              {t('sess.advanced')}
            </button>
          )}
          <span className="grow" />
          {missing(launch) && <span className="small muted">{t('sess.install_first', { tool: t(`launch.${launch}.title`) })}</span>}
          <button type="submit" className="start-go" disabled={busy || !allowed || offline || !workdir.trim()} aria-label={t('sess.start')} title={t('sess.start_hint')}>
            {busy ? <Spinner size={18} /> : dshRow ? <ExternalLink size={18} /> : <ArrowUp size={20} />}
          </button>
        </div>
        {adv && launch !== 'dsh' && (
          <div className="start-advanced">
            <label className="field">
              <span className="field-label">{t('form.name')}</span>
              <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={t('sess.name_ph')} />
            </label>
            <ShellPicker value={shell} onChange={setShell} data={shells} inherit={status.settings?.default_shell || null} />
            {canFull && (
              <label className="start-check">
                <input type="checkbox" checked={fullAccess} onChange={(e) => setFullAccess(e.target.checked)} />
                <span>
                  <b>{t('form.full_access')}</b>
                  <span className="field-hint">{t(`form.full_access_hint_${launch}`)}</span>
                </span>
              </label>
            )}
            {canFull && fullAccess && isRoot && (
              <label className="start-check">
                <input type="checkbox" checked={rootOk} onChange={(e) => setRootOk(e.target.checked)} />
                <span>
                  <b>{t('form.root_confirm')}</b>
                  <span className="field-hint">{t('form.root_confirm_hint')}</span>
                </span>
              </label>
            )}
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
      <ErrorNote error={error} />
      {offline && <p className="small muted">{t('sess.device_offline')}</p>}
      {missing(launch) && (
        <button type="button" className="btn btn-plain small" onClick={reloadTools}>
          {t('tools.recheck')}
        </button>
      )}
      <RecentProjects current={project?.key} onPick={pickProject} />
      <div className="start-foot">
        <BackendNote />
        <RemoteCard />
        <SafetyNote />
        <Promotions />
      </div>
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
        <button key={p.key} type="button" onClick={() => onPick(p.device, p.workdir)} title={p.workdir}>
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
      setH(Math.max(320, window.innerHeight - top));
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

/** 会话顶栏左边：电脑、项目。 */
function SessionLead({ inst }) {
  const devices = useDevices();
  const dev = devices.multi && inst ? devices.byId(terminalDevice(inst.id)) : null;
  if (!inst) return null;
  return (
    <span className="slead">
      {dev && <DeviceTag device={dev} />}
      <span className="slead-proj ellipsis" title={inst.workdir}>
        <Folder size={12} aria-hidden="true" />
        {baseName(inst.workdir)}
      </span>
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
  const devices = useDevices();
  const ref = useRef(null);
  const height = useFillHeight(ref, Boolean(activeId));
  const ids = activeId && !s.recent.includes(activeId) ? [activeId, ...s.recent] : s.recent;
  useEffect(() => {
    if (activeId) s.openRecent(activeId);
  }, [activeId]);
  // 被删掉的会话：拿到了它所在电脑的列表、里面却没有（电脑离线、还没加载时不算）
  const missing = (id) => {
    if (s.loading || s.error || s.byId(id)) return false;
    if (!devices.multi) return true;
    const dev = terminalDevice(id);
    return Boolean(dev && s.groups?.[dev] && !s.groups[dev].error);
  };
  useEffect(() => {
    for (const id of s.recent) if (id !== activeId && missing(id)) s.closeRecent(id);
  });
  if (!ids.length) return null;
  return (
    <div className="sstage" ref={ref} hidden={!activeId} style={height ? { height } : undefined}>
      {ids.map((id) => {
        const inst = s.byId(id);
        return (
          <TermPane
            key={id}
            id={id}
            inst={inst}
            missing={missing(id)}
            active={id === activeId}
            lead={inst && <LaunchIcon launch={inst.launch} size={16} />}
            sub={<SessionLead inst={inst} />}
            menu={inst && <SessionMenu inst={inst} />}
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
