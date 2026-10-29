// 看看客户端管理台。未登录时只显示登录页；登录后左侧栏（手机为抽屉）：用户菜单、新会话、工具（文件、SSH 主机、本机网页、
// AI、系统、设置，可收成一行图标）、项目和会话。电脑上侧栏能整条收成图标栏。
// 账户的事（电脑、会员与邀请、账户与安全）也在这里，放在用户菜单里，数据来自看看平台（shared/platform.js）。
// 本机 App 和浏览器远程打开是同一个界面，差别只在顶栏的连接胶囊（ConnChip）。
// 首页 / 是开始页（手机上是会话列表），/s/{id} 在右边打开会话；整页终端 /t/{id} 占满整个窗口（手机、新窗口）。
// 界面不出现“ttyd、tmux、隧道、端口转发”等术语。
import { Activity, BookOpen, Bot, Bug, CalendarClock, Compass, FolderOpen, Github, Globe, History, House, Keyboard, Languages, Laptop, Link2, ListTodo, LogIn, LogOut, MessageSquare, Plus, Server, Settings as SettingsIcon, ShieldCheck, UserRound, Wrench } from 'lucide-react';
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
import { useAwayTracker } from './shared/board.js';
import { DeviceSwitcher, DevicesProvider, deviceName, useDevices, useDeviceState } from './shared/devices.jsx';
import ErrorBoundary from './shared/ErrorBoundary.jsx';
import { Link, Router, useCanonicalPath, useRouter } from './shared/router.jsx';
import PopMenu from './shared/PopMenu.jsx';
import { sessionState, SessionsProvider, useSessions } from './shared/sessions.jsx';
import { daysLeft, formatTime } from './shared/time.js';
import { BrandMark, Button, Drawer, FoldNote, LangSwitch, Note, Skeleton, useConfirm, useToast } from './shared/ui.jsx';
import { isMod, keyLabel } from './shared/hotkeys.js';
import { setLocale } from './shared/i18n.js';
import QuickSwitch from './shared/QuickSwitch.jsx';
import ShortcutsHelp from './shared/Shortcuts.jsx';
import { errorText } from './shared/i18n.js';
import { ReportProvider, useReportDialog } from './shared/Report.jsx';
import { TipsProvider, useTips } from './shared/tips.jsx';
import { OnboardTracker } from './pages/Onboarding.jsx';
import { RelayHint } from './shared/RelayPicker.jsx';
import { ConnChip } from './shared/RelayStatus.jsx';
import { JustUpdatedBanner, shouldRemind, UpdateBanner, UpdateProvider } from './shared/Update.jsx';
import { TransferBanner } from './pages/SshTransfer.jsx';
import { remoteAllowed, renewSoon } from './shared/Remote.jsx';

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
  if (name === null) {
    return (
      <span className={`avatar avatar-guest ${large ? 'avatar-lg' : ''}`}>
        <UserRound size={large ? 22 : 16} aria-hidden="true" />
      </span>
    );
  }
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

const RENEW_LATER_KEY = 'looklook.renew_later';

/** 稍后提醒：记下到期时间和当时还剩几天，剩 3 天、1 天时再提醒一次。 */
function useRenewLater(expires, left) {
  const tier = left === null ? null : left <= 1 ? 1 : left <= 3 ? 3 : 14;
  const key = `${expires}:${tier}`;
  const [later, setLater] = useState(() => {
    try {
      return localStorage.getItem(RENEW_LATER_KEY);
    } catch {
      return null;
    }
  });
  const dismiss = () => {
    try {
      localStorage.setItem(RENEW_LATER_KEY, key);
    } catch {
      /* 只是不记住 */
    }
    setLater(key);
  };
  return [later === key, dismiss];
}

function GateBanner() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const acc = status.account;
  const soon = renewSoon(status);
  const [later, dismiss] = useRenewLater(acc.session?.membership_expires_at, soon);
  const reconnect = async () => {
    await api.post('/auth/reconnect').catch(() => {});
    setTimeout(reload, 1500);
  };
  // 没登录：这台电脑照常用，远程访问的入口在顶栏、用户菜单和本机网页里，不在这里打扰
  if (!acc.logged_in) return null;
  if (!remoteAllowed(status)) {
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
  if (soon !== null && !later) {
    return (
      <div className="banner banner-warn">
        <span className="grow">{t('upsell.renew_soon', { count: soon })}</span>
        <Link to="/membership" className="btn btn-primary">
          {t('gate.renew')}
        </Link>
        <Button variant="ghost" onClick={dismiss}>
          {t('upsell.renew_later')}
        </Button>
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
  const { t, i18n } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const logout = useLogout();
  const openReport = useReportDialog();
  const toast = useToast();
  const tips = useTips();
  const open = (url) => () => window.open(url, '_blank', 'noopener');
  const left = daysLeft(status.account.session?.membership_expires_at);
  const en = i18n.language?.startsWith('en');
  const guest = !status.account.logged_in;
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
    // 没登录：第一项就是开通远程访问
    guest && { icon: LogIn, label: t('upsell.menu'), hint: t('upsell.menu_hint'), onClick: () => navigate('/account') },
    guest && 'sep',
    !guest && { icon: Laptop, label: t('acct.computers'), onClick: () => navigate('/devices') },
    !guest && { icon: CalendarClock, label: t('acct.membership'), hint: left > 0 ? t('account.days_left', { n: left }) : t('account.expired'), onClick: () => navigate('/membership') },
    !guest && { icon: ShieldCheck, label: t('acct.account'), onClick: () => navigate('/account') },
    { icon: SettingsIcon, label: t('nav.settings'), hint: shouldRemind(status.update) ? t('update.badge') : undefined, onClick: () => navigate('/settings') },
    'sep',
    { icon: Languages, label: en ? '中文' : 'English', onClick: () => setLocale(en ? 'zh-CN' : 'en-US') },
    { icon: Compass, label: t('onboard.menu'), onClick: () => (tips.unmark('onboard.hide'), navigate('/')) },
    { icon: Keyboard, label: t('keys.title'), hint: keyLabel('mod+/'), onClick: () => window.dispatchEvent(new Event('ll-shortcuts')) },
    // 问题报告经看看服务器转交，要登录；没登录时到 GitHub 提
    !guest && { icon: Bug, label: t('report.title'), onClick: openReport },
    { icon: House, label: t('nav.home_page'), onClick: open(webUrl(status, '/')) },
    { icon: BookOpen, label: t('nav.docs'), onClick: open(webUrl(status, '/docs')) },
    { icon: Github, label: t('nav.github'), onClick: open(SOURCE_URL) },
    !guest && 'sep',
    !guest && { icon: LogOut, label: t('nav.logout'), onClick: logout, danger: true },
  ];
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

// 会话栏只在会话相关的页面出现（docs/UI_REDESIGN_2026-10.md §2.1）；工具页内容区铺满
const isSessionArea = (p) => p === '/' || p === '/project' || SESS.test(p);
const PANE_W_KEY = 'looklook.panew';
const PANE_MIN = 240;
const PANE_MAX = 440;

function readWidth() {
  try {
    const v = Number(localStorage.getItem(PANE_W_KEY));
    return v >= PANE_MIN && v <= PANE_MAX ? v : 288;
  } catch {
    return 288;
  }
}

/** 图标栏上的一个入口：悬停显示名字（和快捷键），右上角可以带个数或红点。 */
function RailItem({ to, onClick, icon: Icon, label, hint, active, count, dot, className = '', disabled }) {
  const title = hint ? `${label} (${hint})` : label;
  const inner = (
    <>
      <Icon size={19} aria-hidden="true" />
      {count > 0 && <span className="rail-count">{count}</span>}
      {dot && <span className="dot-badge" aria-hidden="true" />}
    </>
  );
  const cls = `rail-btn ${active ? 'active' : ''} ${className}`;
  if (to) {
    return (
      <Link to={to} className={cls} aria-label={label} title={title} aria-current={active ? 'page' : undefined}>
        {inner}
      </Link>
    );
  }
  return (
    <button type="button" className={cls} onClick={onClick} aria-label={label} title={title} disabled={disabled} aria-pressed={active}>
      {inner}
    </button>
  );
}

/**
 * 电脑上最左边的图标栏：logo、新会话、会话（显示 / 收起会话栏）、任务看板、以前的对话；工具；底部是设置、连接状态、用户菜单。
 */
function NavRail({ path, paneOpen, onPane }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const ui = useSessionUi();
  const sessions = useSessions();
  const userMenu = useUserMenu();
  const n = useAttnCount();
  const s = status.account.session;
  const area = isSessionArea(path);
  const openSessions = () => {
    if (area) return onPane(!paneOpen);
    onPane(true);
    const last = sessions.recent.find((id) => sessions.byId(id));
    if (last) ui.openSession(last);
    else navigate('/');
  };
  return (
    <nav className="rail" aria-label={t('nav.main')}>
      <Link to="/" className="rail-brand" aria-label={t('nav.start')} title={t('nav.start')}>
        <BrandMark size={30} />
      </Link>
      <RailItem onClick={() => ui.newSession()} icon={Plus} label={t('sess.new')} hint={keyLabel('mod+shift+O')} className="rail-new" disabled={!status.account.allowed} />
      <RailItem onClick={openSessions} icon={MessageSquare} label={t(!area ? 'sess.title' : paneOpen ? 'sess.collapse_side' : 'sess.expand_side')} hint={keyLabel('mod+shift+S')} active={area} />
      <RailItem to="/tasks" icon={ListTodo} label={n > 0 ? t('board.nav_count', { count: n }) : t('board.title')} active={path === '/tasks'} count={n} />
      <RailItem to="/history" icon={History} label={t('hist.title')} active={path === '/history'} />
      <span className="rail-sep" aria-hidden="true" />
      {TOOLS.filter((x) => x.to !== '/settings').map((x) => (
        <RailItem key={x.to} to={x.to} icon={x.icon} label={t(x.key)} active={path === x.to} />
      ))}
      <span className="rail-grow" aria-hidden="true" />
      <RailItem to="/settings" icon={SettingsIcon} label={t('nav.settings')} dot={shouldRemind(status.update)} />
      <span className="rail-conn">
        <ConnChip />
      </span>
      <PopMenu items={userMenu} className="rail-btn rail-me" label={s?.user.nickname ?? t('upsell.menu')}>
        <Initial name={s ? s.user.nickname : null} />
      </PopMenu>
    </nav>
  );
}

/** 会话栏：项目和会话列表，右边缘可以拖动改宽度（记在本机）。 */
function SessionPane({ path, width, onWidth, onCollapse }) {
  const { t } = useTranslation();
  const current = path.match(SESS)?.[1];
  const drag = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = width;
    let w = w0;
    const move = (ev) => {
      w = Math.max(PANE_MIN, Math.min(PANE_MAX, w0 + ev.clientX - x0));
      onWidth(w, false);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      document.body.classList.remove('resizing');
      onWidth(w, true);
    };
    document.body.classList.add('resizing');
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };
  const key = (e) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    onWidth(Math.max(PANE_MIN, Math.min(PANE_MAX, width + (e.key === 'ArrowLeft' ? -16 : 16))), true);
  };
  return (
    <aside className="spane" id="side-nav" aria-label={t('sess.title')}>
      <SessionList currentId={current} onCollapse={onCollapse} />
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- 分隔条可以用方向键调宽度 */}
      <div className="spane-resize" role="separator" aria-orientation="vertical" aria-valuenow={width} aria-valuemin={PANE_MIN} aria-valuemax={PANE_MAX} aria-label={t('sess.pane_resize')} tabIndex={0} onPointerDown={drag} onKeyDown={key} onDoubleClick={() => onWidth(288, true)} />
    </aside>
  );
}

/** 工具页顶上：多台电脑时说明工具作用于哪台，可以切换（手机上在顶栏里）。 */
function ToolContext() {
  const { t } = useTranslation();
  const { status } = useStatus();
  return (
    <div className="toolctx">
      <span className="muted small">{t('nav.tools_on')}</span>
      <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/remote')} />
    </div>
  );
}

/**
 * 手机底部的标签栏：会话 · 任务 · ＋新建 · 工具 · 我的。工具和“我的”从底部弹出菜单。
 */
function MobileTabs({ path }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const ui = useSessionUi();
  const userMenu = useUserMenu();
  const n = useAttnCount();
  const s = status.account.session;
  const tools = TOOLS.filter((x) => x.to !== '/settings').map((x) => ({ icon: x.icon, label: t(x.key), onClick: () => navigate(x.to) }));
  const onTool = TOOLS.some((x) => x.to === path && x.to !== '/settings');
  return (
    <nav className="mtabs" aria-label={t('nav.main')}>
      <Link to="/" className={`mtab ${isSessionArea(path) ? 'active' : ''}`}>
        <MessageSquare size={21} aria-hidden="true" />
        <span>{t('sess.title')}</span>
      </Link>
      <Link to="/tasks" className={`mtab ${path === '/tasks' ? 'active' : ''}`}>
        <ListTodo size={21} aria-hidden="true" />
        {n > 0 && <span className="rail-count">{n}</span>}
        <span>{t('board.short')}</span>
      </Link>
      <button type="button" className="mtab mtab-new" onClick={() => ui.newSession()} disabled={!status.account.allowed} aria-label={t('sess.new')}>
        <span className="mtab-plus">
          <Plus size={22} aria-hidden="true" />
        </span>
      </button>
      <PopMenu items={[{ head: t('sess.tools') }, ...tools]} className={`mtab ${onTool ? 'active' : ''}`} label={t('sess.tools')} sheet>
        <Wrench size={21} aria-hidden="true" />
        <span>{t('sess.tools')}</span>
      </PopMenu>
      <PopMenu items={userMenu} className="mtab" label={s?.user.nickname ?? t('upsell.menu')} sheet>
        <Initial name={s ? s.user.nickname : null} />
        {shouldRemind(status.update) && <span className="dot-badge" aria-hidden="true" />}
        <span>{t('nav.me')}</span>
      </PopMenu>
    </nav>
  );
}

function ConsoleLayout({ path, stale, children }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const ui = useSessionUi();
  const { navigate } = useRouter();
  const switcher = (devices.multi || devices.localOnly) && DEVICE_PAGES.includes(path);
  const [paneOpen, setPaneState] = useState(() => !readFlag(RAIL_KEY, false));
  const setPane = (v) => {
    writeFlag(RAIL_KEY, !v);
    setPaneState(v);
  };
  const [width, setWidth] = useState(readWidth);
  const onWidth = (w, save) => {
    setWidth(w);
    if (save) {
      try {
        localStorage.setItem(PANE_W_KEY, String(w));
      } catch {
        /* 无痕模式等：只是不记住 */
      }
    }
  };
  const paneRef = useRef(paneOpen);
  paneRef.current = paneOpen;
  const pathRef = useRef(path);
  pathRef.current = path;
  // Ctrl+Shift+O 新会话，Ctrl+Shift+S 收起 / 展开会话栏（焦点在终端里时由终端自己处理按键）。
  // 工具页上没有会话栏：不改记住的状态，免得回到会话时侧栏莫名其妙没了
  useEffect(() => {
    const onKey = (e) => {
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === 'o') {
        e.preventDefault();
        ui.newSession();
      } else if (k === 's' && window.innerWidth >= 900 && isSessionArea(pathRef.current)) {
        e.preventDefault();
        setPane(!paneRef.current);
      }
    };
    // ⌘, / Ctrl+, 打开设置（Mac 桌面版由原生菜单发 `ll-menu`，见 src/desktop.rs 的 page_command）
    const onSettings = (e) => {
      if (!isMod(e, ',')) return;
      e.preventDefault();
      navigate('/settings');
    };
    const onMenu = (e) => {
      if (e.detail === 'settings') navigate('/settings');
      else if (e.detail === 'new_session') ui.newSession();
    };
    window.addEventListener('keydown', onKey);
    window.addEventListener('keydown', onSettings);
    window.addEventListener('ll-menu', onMenu);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('keydown', onSettings);
      window.removeEventListener('ll-menu', onMenu);
    };
  }, [ui]);
  const wide = isWide(path);
  const sess = path.match(SESS)?.[1] ?? null;
  const pane = paneOpen && isSessionArea(path);
  useTitleCount();
  return (
    <>
      {/* 手机上的顶栏；电脑上这些都在图标栏里 */}
      <header className="topbar topbar-console">
        <Link to="/" className="brand">
          <BrandMark />
          <span className="brand-text">{t('brand')}</span>
        </Link>
        <span className="inline" style={{ flexWrap: 'nowrap' }}>
          {switcher && <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/remote')} />}
          <ConnChip />
        </span>
      </header>
      <div className={`console ${pane ? 'console-pane' : ''} ${sess ? 'console-sess' : ''}`} style={{ '--pane-w': `${width}px` }}>
        <NavRail path={path} paneOpen={paneOpen} onPane={setPane} />
        {pane && <SessionPane path={path} width={width} onWidth={onWidth} onCollapse={() => setPane(false)} />}
        <div className="console-col">
          {/* 提示横幅放在内容区上面：图标栏和会话栏从窗口顶上一直到底 */}
          <GateBanner />
          <TransferBanner />
          <UpdateBanner />
          <JustUpdatedBanner />
          <RelayHint />
        <main className={`console-main ${wide ? 'console-wide' : 'shell'} ${sess ? 'console-session' : ''}`}>
          {switcher && <ToolContext />}
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
              <ErrorBoundary resetKey={path}>{children}</ErrorBoundary>
            )}
          </Fragment>
          {/* 打开过的会话一直挂着（不在会话页时隐藏），切回来不用重新连 */}
          <SessionStage activeId={sess} />
        </main>
        </div>
      </div>
      <MobileTabs path={path} />
      <QuickSwitch />
      <ShortcutsHelp />
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
    <Drawer open={Boolean(path)} onClose={close} title={!path ? '' : !status.account.logged_in && ACCOUNT_PAGES.includes(path) ? t('upsell.menu') : t(DRAWER_PAGES[path])} wide={path === '/settings'} head={perDevice && <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/remote')} />}>
      <Fragment key={perDevice ? devices.selected : 'all'}>
        {perDevice && stale ? (
          <div className="stack">
            <Skeleton height={60} />
            <Skeleton height={180} />
          </div>
        ) : (
          <ErrorBoundary resetKey={path}>{children}</ErrorBoundary>
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
  useAwayTracker();
  // 状态来自哪台电脑（多设备时随顶栏的选择）
  const [statusDevice, setStatusDevice] = useState(null);
  const statusDevRef = useRef(null);
  statusDevRef.current = statusDevice;
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
        const d = devRef.current;
        if (d?.multi && device && device !== d.def && d.def) {
          // 已经有这台电脑的状态：只是一次没取到（网络抖动），留在这台，下一轮再取
          if (statusDevRef.current === device) return;
          // 刚切过去就连不上（例如它离线了）：这次先看默认的电脑，不要整页显示“连不上”；
          // 不改记住的选择，下次打开还是用户选的那台
          d.select(d.def, false);
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
  // 远程打开时一定已经登录；本机、局域网打开不需要登录，没登录时账户、电脑、会员几页显示“开通远程访问”
  if (!status.account.logged_in && status.access !== 'local') {
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
    const login = rtcMode ? 'remote' : devices.multi ? `local:${devices.def}` : (status.account.session?.device_id ?? 'guest');
    const home = opensInPage() ? <DraftPane key={`start:${login}`} /> : <SessionHome />;
    const page = SESS.test(path) ? null : { '/': home, '/new': <DraftPane key={`start:${login}`} />, '/tasks': <Board />, '/history': <HistoryPage />, '/project': <ProjectPage />, '/files': <Files />, '/ssh': <SshHosts />, '/pages': <Pages />, '/ai': <AiPage />, '/system': <System /> }[path];
    const guest = !status.account.logged_in;
    const drawerPage = route.drawer && { '/settings': <Settings />, '/account': guest ? <Login drawer /> : <Account />, '/devices': guest ? <Login drawer /> : <Computers />, '/membership': guest ? <Login drawer /> : <Membership /> }[route.drawer];
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
            <TipsProvider enabled>
              <OnboardTracker />
              {body}
              <PullRefresh />
            </TipsProvider>
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
