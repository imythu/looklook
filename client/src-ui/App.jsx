// 看看客户端管理台。未登录时只显示登录页；登录后左侧栏（手机为抽屉）：用户菜单、新会话、工具（文件、SSH 主机、本机网页、
// AI、系统、设置，可收成一行图标）、项目和会话。电脑上侧栏能整条收成图标栏。
// 账户的事（电脑、会员与邀请、账户与安全）也在这里，放在用户菜单里，数据来自看看平台（shared/platform.js）。
// 本机 App 和浏览器远程打开是同一个界面，差别只在顶栏的连接胶囊（ConnChip）。
// 首页 / 是开始页（手机上是会话列表），/s/{id} 在右边打开会话；整页终端 /t/{id} 占满整个窗口（手机、新窗口）。
// 界面不出现“ttyd、tmux、隧道、端口转发”等术语。
import { Activity, BookOpen, Bot, Bug, CalendarClock, ChevronDown, FolderOpen, Github, Globe, House, Laptop, LogOut, Menu, MessageSquare, PanelLeftClose, PanelLeftOpen, Plus, Server, Settings as SettingsIcon, ShieldCheck, X } from 'lucide-react';
import { createContext, Fragment, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import Account from './pages/Account.jsx';
import AiPage from './pages/Ai.jsx';
import Computers from './pages/Computers.jsx';
import Files from './pages/Files.jsx';
import Login from './pages/Login.jsx';
import Membership from './pages/Membership.jsx';
import Pages from './pages/Pages.jsx';
import Settings from './pages/Settings.jsx';
import SshHosts from './pages/SshHosts.jsx';
import System from './pages/System.jsx';
import { opensInPage, SessionHome, SessionList, SessionStage, SessionUiProvider, StartPane, useSessionUi } from './pages/Sessions.jsx';
import TerminalView from './pages/TerminalView.jsx';
import { api, rtcMode, selectedDevice } from './shared/api.js';
import { configureDirect, probeDirect } from './shared/direct.js';
import { DeviceSwitcher, DevicesProvider, deviceName, useDevices, useDeviceState } from './shared/devices.jsx';
import { Link, Router, useCanonicalPath, useRouter } from './shared/router.jsx';
import PopMenu from './shared/PopMenu.jsx';
import { sessionState, SessionsProvider, useSessions } from './shared/sessions.jsx';
import { daysLeft, formatTime } from './shared/time.js';
import { BrandMark, Button, FoldNote, LangSwitch, Note, Skeleton, useConfirm, useToast } from './shared/ui.jsx';
import { errorText } from './shared/i18n.js';
import { ReportProvider, useReportDialog } from './shared/Report.jsx';
import { RelayHint } from './shared/RelayPicker.jsx';
import { ConnChip } from './shared/RelayStatus.jsx';
import { LanBanner, LanFallback } from './shared/lan.jsx';
import { JustUpdatedBanner, shouldRemind, UpdateBanner, UpdateProvider } from './shared/Update.jsx';

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
const PAGES = ['/', '/new', ...ACCOUNT_PAGES, ...TOOLS.map((x) => x.to)];
// 开源仓库（MIT）
export const SOURCE_URL = 'https://github.com/imythu/looklook';
const isKnown = (p) => PAGES.includes(p) || TERM.test(p) || SESS.test(p);
// 会话相关的页面铺满主区域（不限宽度）
const isWide = (p) => p === '/' || p === '/new' || SESS.test(p);
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
  const open = (url) => () => window.open(url, '_blank', 'noopener');
  const left = daysLeft(status.account.session?.membership_expires_at);
  return [
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

function SideNav({ open, onClose, rail, onRail, path }) {
  const { t } = useTranslation();
  const { status } = useStatus();
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
    return (
      <aside className="side side-rail" id="side-nav" aria-label={t('nav.main')}>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => onRail(false)} aria-label={t('sess.expand_side')} title={`${t('sess.expand_side')} (Ctrl+Shift+S)`}>
          <PanelLeftOpen size={18} />
        </button>
        <PopMenu items={userMenu} className="btn btn-plain btn-icon" label={s.user.nickname}>
          <Initial name={s.user.nickname} />
        </PopMenu>
        <button type="button" className="btn btn-icon rail-new" onClick={() => ui.newSession()} disabled={!allowed} aria-label={t('sess.new')} title={`${t('sess.new')} (Ctrl+Shift+O)`}>
          <Plus size={18} />
        </button>
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
      </aside>
    );
  }
  return (
    <>
      {open && <div className="side-backdrop" onClick={onClose} aria-hidden="true" />}
      <aside className={`side ${open ? 'open' : ''}`} id="side-nav" aria-label={t('nav.main')}>
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
        <button type="button" className="side-new" onClick={() => ui.newSession()} disabled={!allowed}>
          <Plus size={18} aria-hidden="true" />
          {t('sess.new')}
          <kbd className="side-kbd">Ctrl ⇧ O</kbd>
        </button>
        <ToolGroup path={path} />
        <div className="side-sessions">
          <SessionList currentId={current} />
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
  const switcher = devices.multi && DEVICE_PAGES.includes(path);
  const [open, setOpen] = useState(false);
  const [rail, setRailState] = useState(() => readFlag(RAIL_KEY, false));
  const setRail = (v) => {
    writeFlag(RAIL_KEY, v);
    setRailState(v);
  };
  const railRef = useRef(rail);
  railRef.current = rail;
  useEffect(() => setOpen(false), [path]);
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
  return (
    <>
      <header className="topbar">
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
          {switcher && <DeviceSwitcher perDevice helpUrl={webUrl(status, '/docs/faq')} />}
          <ConnChip />
          <LangSwitch />
          <Link to="/account" aria-label={t('nav.account')}>
            <Initial name={s.user.nickname} />
          </Link>
        </span>
      </header>
      <GateBanner />
      <LanFallback />
      {status.access === 'remote' && <LanBanner device={devices.multi ? devices.selected : undefined} />}
      <UpdateBanner />
      <JustUpdatedBanner />
      <RelayHint />
      <div className={`console ${rail ? 'console-rail' : ''}`}>
        <SideNav open={open} onClose={() => setOpen(false)} rail={rail && !open} onRail={setRail} path={path} />
        <main className={`console-main ${wide ? 'console-wide' : 'shell'} ${sess ? 'console-session' : ''}`}>
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

function Shell() {
  const path = useCanonicalPath(isKnown);
  const [status, setStatus] = useState(undefined);
  const [down, setDown] = useState(false);
  // 状态来自哪台电脑（多设备时随顶栏的选择）
  const [statusDevice, setStatusDevice] = useState(null);
  const devRef = useRef(null);
  const relayState = status?.relay?.state;
  const reload = useCallback(() => {
    const device = selectedDevice();
    return api
      .get('/status')
      .then((s) => {
        setStatus(s);
        setStatusDevice(device);
        setDown(false);
      })
      .catch(() => {
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
  // 远程打开时在后台探测能不能直连本机（终端走最快的路，见 shared/direct.js）
  const remote = status?.access === 'remote';
  const directKey = devices.multi ? devices.items.map((d) => `${d.id}:${d.os}:${d.online}`).join(',') : status?.capabilities?.os;
  if (status && devices.ready) configureDirect({ remote: rtcMode, os: status.capabilities?.os, devices: devices.multi ? devices.items : null });
  useEffect(() => {
    if (status && devices.ready) probeDirect(devices.multi ? devices.items : null);
  }, [remote, devices.ready, devices.multi, directKey]);
  // 设备列表加载后、切换电脑后，取选中那台电脑的状态
  useEffect(() => {
    if (devices.multi) reload();
  }, [devices.multi, devices.selected, reload]);
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
    const home = opensInPage() ? <StartPane key={`start:${login}`} /> : <SessionHome />;
    const page = SESS.test(path) ? null : { '/': home, '/new': <StartPane key={`start:${login}`} />, '/files': <Files />, '/ssh': <SshHosts />, '/pages': <Pages />, '/ai': <AiPage />, '/system': <System />, '/settings': <Settings />, '/account': <Account />, '/devices': <Computers />, '/membership': <Membership /> }[path];
    body = (
      <SessionsProvider key={login} enabled>
        <SessionUiProvider>
          <ConsoleLayout path={path} stale={devices.multi && statusDevice !== devices.selected}>
            {page}
          </ConsoleLayout>
        </SessionUiProvider>
      </SessionsProvider>
    );
  }
  return (
    <StatusCtx.Provider value={{ status, reload }}>
      <DevicesProvider value={devices}>
        <UpdateProvider>
          <ReportProvider>{body}</ReportProvider>
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
