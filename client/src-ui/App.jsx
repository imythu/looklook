// 看看客户端管理台。未登录时只显示登录页；登录后左侧栏（手机为抽屉）：用户菜单、新会话、工具（文件、SSH 主机、本机网页、
// AI、系统、设置，可收成一行图标）、项目和会话。电脑上侧栏能整条收成图标栏。
// 账户的事（电脑、会员与邀请、账户与安全）也在这里，放在用户菜单里，数据来自看看平台（shared/platform.js）。
// 本机 App 和浏览器远程打开是同一个界面，差别只在顶栏的连接胶囊（ConnChip）。
// 首页 / 是开始页（手机上是会话列表），/s/{id} 在右边打开会话；整页终端 /t/{id} 占满整个窗口（手机、新窗口）。
// 界面不出现“ttyd、tmux、隧道、端口转发”等术语。
import { Activity, BookOpen, Bot, Bug, CalendarClock, ChevronDown, FolderOpen, Github, Globe, History, House, Laptop, Link2, ListTodo, LogOut, Menu, MessageSquare, PanelLeftClose, PanelLeftOpen, Plus, Server, Settings as SettingsIcon, ShieldCheck, X } from 'lucide-react';
import { createContext, Fragment, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import Account from './pages/Account.jsx';
import Board, { useAttnCount } from './pages/Board.jsx';
import AiPage from './pages/Ai.jsx';
import Computers from './pages/Computers.jsx';
import Files from './pages/Files.jsx';
import Login from './pages/Login.jsx';
import Membership from './pages/Membership.jsx';
import Pages from './pages/Pages.jsx';
import Settings from './pages/Settings.jsx';
import SshHosts from './pages/SshHosts.jsx';
import System from './pages/System.jsx';
import { HistoryPage } from './pages/History.jsx';
import { ProjectPage } from './pages/Project.jsx';
import PullRefresh from './shared/pull.jsx';
import { opensInPage, SessionHome, SessionList, SessionStage, SessionTabs, SessionUiProvider, useSessionUi } from './pages/Sessions.jsx';
import { DraftPane } from './pages/Draft.jsx';
import TerminalView from './pages/TerminalView.jsx';
import { api, rtcMode, selectedDevice } from './shared/api.js';
import { DeviceSwitcher, DevicesProvider, deviceName, useDevices, useDeviceState } from './shared/devices.jsx';
import { Link, Router, useCanonicalPath, useRouter } from './shared/router.jsx';
import PopMenu from './shared/PopMenu.jsx';
import { sessionState, SessionsProvider, useSessions } from './shared/sessions.jsx';
import { daysLeft, formatTime } from './shared/time.js';
import { BrandMark, Button, Drawer, FoldNote, LangSwitch, Note, Skeleton, useConfirm, useToast } from './shared/ui.jsx';
import { errorText } from './shared/i18n.js';
import { ReportProvider, useReportDialog } from './shared/Report.jsx';
import { RelayHint } from './shared/RelayPicker.jsx';
import { ConnChip } from './shared/RelayStatus.jsx';
import { JustUpdatedBanner, shouldRemind, UpdateBanner, UpdateProvider } from './shared/Update.jsx';
import { TransferBanner } from './pages/SshTransfer.jsx';

const StatusCtx = createContext({ status: null, reload: () => {} });

/** 客户端状态（账户、能否使用、终端方式、设置），每 10 秒刷新一次。 */
export function useStatus() {
  return useContext(StatusCtx);
}

/** 看看网页上的页面地址，例如续费、注册。 */
export function webUrl(status, path) {
  return `${(status?.account?.server ?? '').replace(/\/$/, '')}${path}`;
}

/** 安全提醒：有公网 IP 的机器不要把管理台端口开放到公网。允许了其他设备却没开访问码时换成红色并说明。
 *  设置页（full）显示全文；登录页、首页折叠成一行，且在手机或其他电脑上打开时只在有风险时出现
 *  （防火墙建议只能在这台电脑上处理）。 */
export function SafetyNote({ full = false }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const c = status?.console;
  if (!c) return null;
  const open = c.allow_lan || c.allowed_ips > 0;
  const risky = open && !c.code_enabled;
  const state = risky ? 'open_no_code' : open ? 'open' : 'local_only';
  if (!full) {
    if (status.access !== 'local' && !risky) return null;
    return (
      <FoldNote kind={risky ? 'error' : 'warn'} summary={`${t('safety.title')} · ${t(`safety.short.${state}`)}`}>
        <p>{t(`safety.${state}`)}</p>
        <p>{t('safety.body', { port: c.port })}</p>
      </FoldNote>
    );
  }
  return (
    <Note kind={risky ? 'error' : 'warn'}>
      <b>{t('safety.title')}</b>
      <span style={{ display: 'block' }}>{t('safety.body', { port: c.port })}</span>
      <span style={{ display: 'block', marginTop: 4 }}>{t(risky ? 'safety.open_no_code' : open ? 'safety.open' : 'safety.local_only')}</span>
    </Note>
  );
}

// 侧栏“工具”里的页面（会话在侧栏下面，账户在用户菜单里）
const TOOLS = [
  { to: '/files', icon: FolderOpen, key: 'nav.files' },
  { to: '/ssh', icon: Server, key: 'nav.ssh' },
  { to: '/pages', icon: Globe, key: 'nav.pages' },
  { to: '/ai', icon: Bot, key: 'nav.ai' },
  { to: '/system', icon: Activity, key: 'nav.system', meta: 'cpu' },
  { to: '/settings', icon: SettingsIcon, key: 'nav.settings', badge: 'update' },
];
// 按电脑的页面：多台电脑时顶栏显示电脑切换（会话合并显示所有电脑，账户页不分电脑）
const DEVICE_PAGES = ['/files', '/ssh', '/pages', '/ai', '/system', '/settings'];
const TERM = /^\/t\/([a-z2-7]{8})$/;
const SESS = /^\/s\/([a-z2-7]{8})$/;
// 用户菜单里的账户页面
const ACCOUNT_PAGES = ['/devices', '/membership', '/account'];
// 从右边滑出来的抽屉（地址照旧，链接、刷新都能直接打开）：盖在会话上面，不换掉它。文件、SSH 这类要干活的工具还是整页
const DRAWER_PAGES = { '/settings': 'nav.settings', '/devices': 'acct.computers', '/account': 'acct.account', '/membership': 'acct.membership' };
const isDrawer = (p) => Object.hasOwn(DRAWER_PAGES, p);
const PAGES = ['/', '/new', '/tasks', '/history', '/project', ...ACCOUNT_PAGES, ...TOOLS.map((x) => x.to)];
// 开源仓库（客户端 AGPL-3.0-or-later）
export const SOURCE_URL = 'https://github.com/imythu/looklook';
const isKnown = (p) => PAGES.includes(p) || TERM.test(p) || SESS.test(p);
// 会话相关的页面铺满主区域（不限宽度）
const isWide = (p) => p === '/' || p === '/new' || p === '/tasks' || p === '/history' || SESS.test(p);
const RAIL_KEY = 'looklook.rail';
const TOOLS_KEY = 'looklook.tools';

function readFlag(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === '1';
  } catch {
    return fallback;
  }
}
function writeFlag(key, v) {
  try {
    localStorage.setItem(key, v ? '1' : '0');
  } catch {
    /* 无痕模式等：只是不记住 */
  }
}

export function Initial({ name, large }) {
  return <span className={`avatar ${large ? 'avatar-lg' : ''}`}>{(name || '?').trim().slice(0, 1).toUpperCase()}</span>;
}

/** 确认后退出登录；连不上服务器时询问是否只退出这台电脑。 */
export function useLogout() {
  const { t } = useTranslation();
  const { reload } = useStatus();
  const confirm = useConfirm();
  const toast = useToast();
  const devices = useDevices();
  return async () => {
    // 多台电脑时，退出的是当前选中的那台，说清楚是哪台
    const body = devices.multi ? t('devices.logout_body', { name: deviceName(t, devices.byId(devices.selected)) }) : t('logout.body');
    if (!(await confirm({ title: t('logout.title'), body, confirm: t('logout.confirm'), danger: true }))) return;
    try {
      await api.post('/auth/logout', { force: false });
    } catch (e) {
      if (e.code !== 'NETWORK') {
        toast(errorText(t, e), 'error');
        return;
      }
      if (!(await confirm({ title: t('logout.offline_title'), body: t('logout.offline_body'), confirm: t('logout.offline_confirm'), danger: true }))) return;
      await api.post('/auth/logout', { force: true });
    }
    reload();
  };
}

function GateBanner() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const acc = status.account;
  const reconnect = async () => {
    await api.post('/auth/reconnect').catch(() => {});
    setTimeout(reload, 1500);
  };
  if (!acc.logged_in) return null;
  if (!acc.allowed) {
    const expired = acc.reason === 'MEMBERSHIP_EXPIRED';
    return (
      <div className="banner banner-error" role="alert">
        <span className="grow">{t(`gate.${acc.reason}`, { defaultValue: t('gate.other') })}</span>
        {expired && (
          <Link to="/membership" className="btn btn-danger">
            {t('gate.renew')}
          </Link>
        )}
        {acc.reason === 'OFFLINE_TOO_LONG' && (
          <Button variant="danger" onClick={reconnect}>
            {t('gate.reconnect')}
          </Button>
        )}
      </div>
    );
  }
  if (acc.last_error) {
    // 心跳失败不一定是网络不通：服务端报错时给出错误码；远程通道仍连着时只是账户状态没同步上，别和右上角“远程在线”打架。
    const deadline = formatTime(acc.session?.offline_deadline);
    const text =
      acc.last_error !== 'NETWORK'
        ? t('gate.unstable_server', { code: acc.last_error, deadline })
        : status.relay?.state === 'connected'
          ? t('gate.unstable_sync', { deadline })
          : t('gate.unstable', { deadline });
    return (
      <div className="banner banner-warn">
        <span className="grow">{text}</span>
        <Button variant="ghost" onClick={reconnect}>
          {t('gate.reconnect')}
        </Button>
      </div>
    );
  }
  return null;
}

/** 用户卡片的菜单：电脑、会员与邀请、账户与安全；报告问题；看看主页、帮助文档、GitHub；退出登录。
 *  外部链接在 App 里由系统浏览器打开（desktop.rs 拦截新窗口）。 */
function useUserMenu() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const logout = useLogout();
  const openReport = useReportDialog();
  const toast = useToast();
  const open = (url) => () => window.open(url, '_blank', 'noopener');
  const left = daysLeft(status.account.session?.membership_expires_at);
  // 专属地址：手机、外面的电脑打开它就能用；菜单里一直看得到，点一下复制
  const addr = status.account.session?.user_host_url;
  const copyAddr = async () => {
    try {
      await navigator.clipboard.writeText(addr);
      toast(t('acct.address_copied'));
    } catch {
      window.prompt('', addr);
    }
  };
  return [
    addr && { icon: Link2, label: t('acct.address'), hint: addr.replace(/^https?:\/\//, '').replace(/\/$/, ''), onClick: copyAddr },
    addr && 'sep',
    { icon: Laptop, label: t('acct.computers'), onClick: () => navigate('/devices') },
    { icon: CalendarClock, label: t('acct.membership'), hint: left > 0 ? t('account.days_left', { n: left }) : t('account.expired'), onClick: () => navigate('/membership') },
    { icon: ShieldCheck, label: t('acct.account'), onClick: () => navigate('/account') },
    'sep',
    { icon: Bug, label: t('report.title'), onClick: openReport },
    { icon: House, label: t('nav.home_page'), onClick: open(webUrl(status, '/')) },
    { icon: BookOpen, label: t('nav.docs'), onClick: open(webUrl(status, '/docs')) },
    { icon: Github, label: t('nav.github'), onClick: open(SOURCE_URL) },
    'sep',
    { icon: LogOut, label: t('nav.logout'), onClick: logout, danger: true },
  ];
}

function ToolMeta({ x }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  if (x.badge === 'update' && shouldRemind(status.update)) {
    return (
      <span className="nav-badge" title={t('update.badge')}>
        {t('update.badge')}
      </span>
    );
  }
  if (x.meta === 'cpu' && status.metrics) {
    return (
      <span className="nav-meta" title={t('system.cpu')}>
        {Math.round(status.metrics.cpu)}%
      </span>
    );
  }
  return null;
}

/** 工具：收起时一行图标（更新红点照样显示），展开时是列表。 */
function ToolGroup({ path }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const [open, setOpen] = useState(() => readFlag(TOOLS_KEY, false));
  const toggle = () => {
    writeFlag(TOOLS_KEY, !open);
    setOpen(!open);
  };
  return (
    <div className="side-tools">
      <button type="button" className="side-group-head" onClick={toggle} aria-expanded={open} title={t(open ? 'sess.tools_fold' : 'sess.tools_unfold')}>
        <ChevronDown size={14} aria-hidden="true" />
        {t('sess.tools')}
      </button>
      {(devices.multi || devices.localOnly) && <DeviceSwitcher perDevice side helpUrl={webUrl(status, '/docs/faq')} />}
      {open ? (
        <nav className="side-group" aria-label={t('sess.tools')}>
          {TOOLS.map((x) => (
            <Link key={x.to} to={x.to} className="side-link">
              <x.icon size={18} aria-hidden="true" />
              {t(x.key)}
              <ToolMeta x={x} />
            </Link>
          ))}
        </nav>
      ) : (
        <nav className="tool-strip" aria-label={t('sess.tools')}>
          {TOOLS.map((x) => (
            <Link key={x.to} to={x.to} className={`btn btn-plain btn-icon tool-ico ${path === x.to ? 'active' : ''}`} aria-label={t(x.key)} title={t(x.key)}>
              <x.icon size={18} aria-hidden="true" />
              {x.badge === 'update' && shouldRemind(status.update) && <span className="dot-badge" aria-label={t('update.badge')} />}
            </Link>
          ))}
        </nav>
      )}
    </div>
  );
}

/**
 * 任务看板的入口（docs/TASK_BOARD.md §5.1），带“等你处理”的个数：展开的侧栏里一行、收起的侧栏里一个图标、
 * 手机顶栏上一个图标（有要处理的才显示）。
 */
function TasksLink({ rail = false, top = false, active }) {
  const { t } = useTranslation();
  const n = useAttnCount();
  const label = n > 0 ? t('board.nav_count', { count: n }) : t('board.title');
  if (top) {
    if (n === 0 || active) return null;
    return (
      <Link to="/tasks" className="btn btn-plain btn-icon tasks-top" aria-label={label} title={label}>
        <ListTodo size={20} aria-hidden="true" />
        <span className="rail-count">{n}</span>
      </Link>
    );
  }
  if (rail) {
    return (
      <Link to="/tasks" className={`btn btn-plain btn-icon tool-ico rail-tasks ${active ? 'active' : ''}`} aria-label={label} title={label}>
        <ListTodo size={18} aria-hidden="true" />
        {n > 0 && <span className="rail-count">{n}</span>}
      </Link>
    );
  }
  return (
    <Link to="/tasks" className={`side-link side-tasks ${active ? 'active' : ''}`}>
      <ListTodo size={18} aria-hidden="true" />
      {t('board.title')}
      {n > 0 && <span className="side-tasks-count">{n}</span>}
    </Link>
  );
}

/** 标签页标题前加上等你处理的个数：`(2) 看看`。每次重画都对一遍（对话页会临时在标题前加 ●，之后还原） */
function useTitleCount() {
  const n = useAttnCount();
  useEffect(() => {
    const base = document.title.replace(/^\(\d+\) /, '');
    const want = n > 0 ? `(${n}) ${base}` : base;
    if (document.title !== want) document.title = want;
  });
}

function SideNav({ open, onClose, rail, onRail, path }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const ui = useSessionUi();
  const sessions = useSessions();
  const userMenu = useUserMenu();
  const s = status.account.session;
  const left = daysLeft(s.membership_expires_at);
  const allowed = status.account.allowed;
  const current = path.match(SESS)?.[1];
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);
  if (rail) {
    const attn = sessions.items.filter((i) => sessionState(i) === 'attn');
    const switcher = (devices.multi || devices.localOnly) && DEVICE_PAGES.includes(path);
    return (
      <aside className="side side-rail" id="side-nav" aria-label={t('nav.main')}>
        {/* 收起时点 logo 展开侧栏（移上去变成“展开”图标）；展开时点 logo 回首页 */}
        <button type="button" className="side-brand rail-brand" onClick={() => onRail(false)} aria-label={t('sess.expand_side')} title={`${t('sess.expand_side')} (Ctrl+Shift+S)`}>
          <BrandMark size={30} />
          <PanelLeftOpen size={18} className="rail-brand-open" aria-hidden="true" />
        </button>
        <PopMenu items={userMenu} className="btn btn-plain btn-icon" label={s.user.nickname}>
          <Initial name={s.user.nickname} />
        </PopMenu>
        {switcher && <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/faq')} />}
        <button type="button" className="btn btn-icon rail-new" onClick={() => ui.newSession()} disabled={!allowed} aria-label={t('sess.new')} title={`${t('sess.new')} (Ctrl+Shift+O)`}>
          <Plus size={18} />
        </button>
        <Link to="/history" className="btn btn-plain btn-icon" aria-label={t('hist.title')} title={t('hist.title')}>
          <History size={18} aria-hidden="true" />
        </Link>
        <TasksLink rail active={path === '/tasks'} />
        <button
          type="button"
          className="btn btn-plain btn-icon rail-sessions"
          onClick={() => (attn.length ? ui.openSession(attn[0].id) : onRail(false))}
          aria-label={attn.length ? t('sess.rail_attn', { count: attn.length }) : t('sess.expand_side')}
          title={attn.length ? t('sess.rail_attn', { count: attn.length }) : t('sess.expand_side')}
        >
          <MessageSquare size={18} />
          {attn.length > 0 && <span className="rail-count">{attn.length}</span>}
        </button>
        <span className="rail-sep" aria-hidden="true" />
        {TOOLS.map((x) => (
          <Link key={x.to} to={x.to} className={`btn btn-plain btn-icon tool-ico ${path === x.to ? 'active' : ''}`} aria-label={t(x.key)} title={t(x.key)}>
            <x.icon size={18} aria-hidden="true" />
            {x.badge === 'update' && shouldRemind(status.update) && <span className="dot-badge" aria-label={t('update.badge')} />}
          </Link>
        ))}
        <span className="rail-foot">
          <ConnChip />
        </span>
      </aside>
    );
  }
  return (
    <>
      {open && <div className="side-backdrop" onClick={onClose} aria-hidden="true" />}
      <aside className={`side ${open ? 'open' : ''}`} id="side-nav" aria-label={t('nav.main')}>
        <Link to="/" className="brand side-brand">
          <BrandMark size={28} />
          <span className="brand-text">{t('brand')}</span>
        </Link>
        <div className="side-head">
          <PopMenu items={userMenu} className="side-me" label={s.user.nickname}>
            <Initial name={s.user.nickname} />
            <span className="grow">
              <b className="ellipsis">{s.user.nickname}</b>
              <span className={`small ${left > 30 ? 'muted' : 'warn-text'}`}>{left > 0 ? t('account.days_left', { n: left }) : t('account.expired')}</span>
            </span>
          </PopMenu>
          <button type="button" className="btn btn-plain btn-icon side-collapse" onClick={() => onRail(true)} aria-label={t('sess.collapse_side')} title={`${t('sess.collapse_side')} (Ctrl+Shift+S)`}>
            <PanelLeftClose size={18} />
          </button>
          <button type="button" className="btn btn-plain btn-icon side-close" onClick={onClose} aria-label={t('nav.close_menu')}>
            <X size={20} />
          </button>
        </div>
        <div className="side-new-row">
          <button type="button" className="side-new" onClick={() => ui.newSession()} disabled={!allowed}>
            <Plus size={18} aria-hidden="true" />
            {t('sess.new')}
            <kbd className="side-kbd">Ctrl ⇧ O</kbd>
          </button>
          <Link to="/history" className="btn side-hist" aria-label={t('hist.title')} title={t('hist.title')}>
            <History size={18} aria-hidden="true" />
          </Link>
        </div>
        <TasksLink active={path === '/tasks'} />
        <ToolGroup path={path} />
        <div className="side-sessions">
          <SessionList currentId={current} />
        </div>
        <div className="side-foot">
          <ConnChip />
          <LangSwitch />
        </div>
      </aside>
    </>
  );
}

function ConsoleLayout({ path, stale, children }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const ui = useSessionUi();
  const switcher = (devices.multi || devices.localOnly) && DEVICE_PAGES.includes(path);
  const [open, setOpen] = useState(false);
  const [rail, setRailState] = useState(() => readFlag(RAIL_KEY, false));
  const setRail = (v) => {
    writeFlag(RAIL_KEY, v);
    setRailState(v);
  };
  const railRef = useRef(rail);
  railRef.current = rail;
  // 换了页面（包括打开抽屉，它不改主区域的 path）时收起手机上的侧栏
  const { path: here } = useRouter();
  useEffect(() => setOpen(false), [here]);
  // Ctrl+Shift+O 新会话，Ctrl+Shift+S 收起 / 展开侧栏（焦点在终端里时由终端自己处理按键）
  useEffect(() => {
    const onKey = (e) => {
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === 'o') {
        e.preventDefault();
        ui.newSession();
      } else if (k === 's' && window.innerWidth >= 900) {
        e.preventDefault();
        setRail(!railRef.current);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [ui]);
  const s = status.account.session;
  const wide = isWide(path);
  const sess = path.match(SESS)?.[1] ?? null;
  useTitleCount();
  return (
    <>
      {/* 手机上的顶栏；宽屏上这些都在侧栏里（连接、语言在侧栏底部），不再占一整行 */}
      <header className="topbar topbar-console">
        <span className="inline" style={{ flexWrap: 'nowrap', gap: 4 }}>
          <button type="button" className="btn btn-plain btn-icon menu-btn" onClick={() => setOpen(true)} aria-label={t('nav.menu')} aria-expanded={open} aria-controls="side-nav">
            <Menu size={22} />
            {shouldRemind(status.update) && <span className="dot-badge" aria-label={t('update.badge')} />}
          </button>
          <Link to="/" className="brand">
            <BrandMark />
            <span className="brand-text">{t('brand')}</span>
          </Link>
        </span>
        <span className="inline" style={{ flexWrap: 'nowrap' }}>
          {/* 宽屏展开的侧栏里“工具”上方已有电脑切换，顶栏的只在手机和收起侧栏时显示 */}
          {switcher && (
            <span className={rail ? 'inline' : 'inline side-dup'}>
              <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/faq')} />
            </span>
          )}
          <TasksLink top active={path === '/tasks'} />
          <ConnChip />
          <LangSwitch />
          <Link to="/account" aria-label={t('nav.account')}>
            <Initial name={s.user.nickname} />
          </Link>
        </span>
      </header>
      <GateBanner />
      <TransferBanner />
      <UpdateBanner />
      <JustUpdatedBanner />
      <RelayHint />
      <div className={`console ${rail ? 'console-rail' : ''}`}>
        <SideNav open={open} onClose={() => setOpen(false)} rail={rail && !open} onRail={setRail} path={path} />
        <main className={`console-main ${wide ? 'console-wide' : 'shell'} ${sess ? 'console-session' : ''}`}>
          {/* 打开的会话排成顶部标签（电脑上，开始页和会话页） */}
          {(sess || path === '/') && <SessionTabs activeId={sess} />}
          {/* 刚切换电脑、新电脑的状态还没拿到时先不渲染页面，免得页面用上一台电脑的设置初始化 */}
          <Fragment key={switcher ? devices.selected : 'all'}>
            {switcher && stale ? (
              <div className="stack">
                <Skeleton height={60} />
                <Skeleton height={180} />
              </div>
            ) : (
              children
            )}
          </Fragment>
          {/* 打开过的会话一直挂着（不在会话页时隐藏），切回来不用重新连 */}
          <SessionStage activeId={sess} />
        </main>
      </div>
    </>
  );
}

/** 设置、电脑、账户、会员的抽屉。设置按电脑分：多台电脑时标题旁边能切换，换电脑时等那台的状态到了再显示 */
function PageDrawer({ path, behind, onClose, stale, children }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const sessions = useSessions();
  const perDevice = path === '/settings' && (devices.multi || devices.localOnly);
  const close = () => {
    onClose();
    // 回到会话：焦点交还给它的输入框
    const id = behind.match(SESS)?.[1];
    if (id) setTimeout(() => sessions.requestFocus(id), 0);
  };
  return (
    <Drawer open={Boolean(path)} onClose={close} title={path ? t(DRAWER_PAGES[path]) : ''} wide={path === '/settings'} head={perDevice && <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/faq')} />}>
      <Fragment key={perDevice ? devices.selected : 'all'}>
        {perDevice && stale ? (
          <div className="stack">
            <Skeleton height={60} />
            <Skeleton height={180} />
          </div>
        ) : (
          children
        )}
      </Fragment>
    </Drawer>
  );
}

function PublicLayout({ children }) {
  const { t } = useTranslation();
  return (
    <>
      <header className="topbar">
        <span className="brand">
          <BrandMark />
          {t('brand')}
        </span>
        <LangSwitch />
      </header>
      <main className="shell">{children}</main>
    </>
  );
}

function Unreachable({ onRetry }) {
  const { t } = useTranslation();
  return (
    <PublicLayout>
      <div className="auth">
        <Note kind="error">
          <b>{t('unreachable.title')}</b>
          <br />
          {t('unreachable.body')}
        </Note>
        <Button block onClick={onRetry}>
          {t('unreachable.retry')}
        </Button>
      </div>
    </PublicLayout>
  );
}

/**
 * 抽屉页面下面露出来的是哪一页：打开抽屉之前所在的那页（一般是会话）；直接打开抽屉的地址时是首页。
 * 关掉：从那页打开的就后退回去（浏览器的后退、手机的返回手势也一样关掉）；直接打开的换成那页的地址。
 */
function useDrawerRoute(path) {
  const { navigate, pop, replace } = useRouter();
  const behind = useRef(isDrawer(path) ? '/' : path);
  // 从下面那页打开抽屉以来压进历史的条数（在抽屉里点链接到别的抽屉页也算）：关掉时后退这么多步
  const pushed = useRef(0);
  const prev = useRef(path);
  if (!isDrawer(path)) behind.current = path;
  useEffect(() => {
    if (prev.current === path) return;
    if (!isDrawer(path)) pushed.current = 0;
    else if (pop) pushed.current = Math.max(0, pushed.current - 1);
    else if (!replace) pushed.current += 1;
    prev.current = path;
  }, [path]);
  const close = () => {
    const n = pushed.current;
    pushed.current = 0;
    if (n > 0 && window.history.length > n) window.history.go(-n);
    else navigate(behind.current, { replace: true });
  };
  return { drawer: isDrawer(path) ? path : null, behind: behind.current, close };
}

function Shell() {
  const path0 = useCanonicalPath(isKnown);
  const route = useDrawerRoute(path0);
  // 抽屉开着时，主区域显示它下面的那页（会话不卸载、不隐藏）
  const path = route.drawer ? route.behind : path0;
  const [status, setStatus] = useState(undefined);
  const [down, setDown] = useState(false);
  // 状态来自哪台电脑（多设备时随顶栏的选择）
  const [statusDevice, setStatusDevice] = useState(null);
  const devRef = useRef(null);
  const relayState = status?.relay?.state;
  // 状态请求可能交错（定时、切回窗口、操作后各发一次）：只用最后发出的那次的结果，晚回来的旧结果不能盖掉新的
  const reqSeq = useRef(0);
  const reload = useCallback(() => {
    const device = selectedDevice();
    const n = ++reqSeq.current;
    return api
      .get('/status')
      .then((s) => {
        if (n !== reqSeq.current) return;
        setStatus(s);
        setStatusDevice(device);
        setDown(false);
      })
      .catch(() => {
        if (n !== reqSeq.current) return;
        // 选中的电脑连不上（例如刚离线）：回到默认的电脑，不要整页显示“连不上”
        const d = devRef.current;
        if (d?.multi && device && device !== d.def && d.def) {
          d.select(d.def);
          return;
        }
        setDown(true);
      });
  }, []);
  // 远程打开（启动页给的列表）和本机 App（本机客户端连其他电脑，服务端 docs/06 §14）都能切换电脑
  const devices = useDeviceState(Boolean(status?.account?.logged_in));
  devRef.current = devices;
  // 设备列表加载后、切换电脑后，取选中那台电脑的状态
  useEffect(() => {
    if (devices.multi || devices.localOnly) reload();
  }, [devices.multi, devices.selected, devices.localOnly, reload]);
  useEffect(() => {
    reload();
    // 远程通道还在连接或连接失败时刷新得勤一点，好尽快看到“已连接”。
    const id = setInterval(reload, relayState === 'connecting' || relayState === 'failed' ? 3000 : 10000);
    const onFocus = () => reload();
    window.addEventListener('focus', onFocus);
    return () => {
      clearInterval(id);
      window.removeEventListener('focus', onFocus);
    };
  }, [reload, relayState]);

  if (down && !status) return <Unreachable onRetry={reload} />;
  if (!status || !devices.ready) {
    return (
      <main className="shell stack">
        <Skeleton height={60} />
        <Skeleton height={180} style={{ borderRadius: 22 }} />
        <Skeleton height={120} />
      </main>
    );
  }
  let body;
  if (!status.account.logged_in) {
    body = (
      <PublicLayout>
        <Login />
      </PublicLayout>
    );
  } else if (TERM.test(path)) {
    body = <TerminalView id={path.match(TERM)[1]} />;
  } else {
    // 换了登录（退出后重新登录）时重置终端页。远程打开时状态来自选中的那台电脑，选中的电脑一变 device_id 就变，
    // 不能拿它当 key：选中的电脑短暂离线时会自动换到默认电脑，终端页重新挂载，打开着的终端小窗就没了。
    // 多台电脑时状态来自选中的那台，换电脑不能换 key；本机 App 用自己的设备 id（列表的 default）。
    const login = rtcMode ? 'remote' : devices.multi ? `local:${devices.def}` : status.account.session?.device_id;
    const home = opensInPage() ? <DraftPane key={`start:${login}`} /> : <SessionHome />;
    const page = SESS.test(path) ? null : { '/': home, '/new': <DraftPane key={`start:${login}`} />, '/tasks': <Board />, '/history': <HistoryPage />, '/project': <ProjectPage />, '/files': <Files />, '/ssh': <SshHosts />, '/pages': <Pages />, '/ai': <AiPage />, '/system': <System /> }[path];
    const drawerPage = route.drawer && { '/settings': <Settings />, '/account': <Account />, '/devices': <Computers />, '/membership': <Membership /> }[route.drawer];
    body = (
      <SessionsProvider key={login} enabled>
        <SessionUiProvider>
          <ConsoleLayout path={path} stale={devices.multi && statusDevice !== devices.selected}>
            {page}
          </ConsoleLayout>
          <PageDrawer path={route.drawer} behind={route.behind} onClose={route.close} stale={devices.multi && statusDevice !== devices.selected}>
            {drawerPage}
          </PageDrawer>
        </SessionUiProvider>
      </SessionsProvider>
    );
  }
  return (
    <StatusCtx.Provider value={{ status, reload }}>
      <DevicesProvider value={devices}>
        <UpdateProvider>
          <ReportProvider>
            {body}
            <PullRefresh />
          </ReportProvider>
        </UpdateProvider>
      </DevicesProvider>
    </StatusCtx.Provider>
  );
}

export default function App() {
  return (
    <Router>
      <Shell />
    </Router>
  );
}
