// 看看客户端管理台。未登录时只显示登录页；登录后左侧菜单（手机为抽屉）：终端、本机网页、设置、账户。
// 终端页 /t/{id} 占满整个窗口。界面不出现“ttyd、tmux、隧道、端口转发”等术语。
import { Activity, Bug, ExternalLink, Globe, LogOut, Menu, Settings as SettingsIcon, SquareTerminal, UserRound, X } from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import Account from './pages/Account.jsx';
import Login from './pages/Login.jsx';
import Pages from './pages/Pages.jsx';
import Settings from './pages/Settings.jsx';
import System from './pages/System.jsx';
import Terminals from './pages/Terminals.jsx';
import TerminalView from './pages/TerminalView.jsx';
import { api, selectedDevice } from './shared/api.js';
import { DeviceSwitcher, DevicesProvider, deviceName, useDevices, useDeviceState } from './shared/devices.jsx';
import { Link, Router, useCanonicalPath } from './shared/router.jsx';
import { daysLeft, formatTime } from './shared/time.js';
import { BrandMark, Button, FoldNote, LangSwitch, Note, Skeleton, useConfirm, useToast } from './shared/ui.jsx';
import { errorText } from './shared/i18n.js';
import { ReportProvider, useReportDialog } from './shared/Report.jsx';
import { RelayHint } from './shared/RelayPicker.jsx';
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

const NAV = [
  { to: '/', icon: SquareTerminal, key: 'nav.terminals' },
  { to: '/pages', icon: Globe, key: 'nav.pages' },
  { to: '/system', icon: Activity, key: 'nav.system', meta: 'cpu' },
  { to: '/settings', icon: SettingsIcon, key: 'nav.settings', badge: 'update' },
  { to: '/account', icon: UserRound, key: 'nav.account' },
];
// 按电脑的页面：多台电脑时顶栏显示电脑切换（终端页合并显示所有电脑，账户页不分电脑）
const DEVICE_PAGES = ['/pages', '/system', '/settings'];
const TERM = /^\/t\/([a-z2-7]{8})$/;
const isKnown = (p) => NAV.some((x) => x.to === p) || TERM.test(p);

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
          <a className="btn btn-danger" href={webUrl(status, '/membership')} target="_blank" rel="noopener noreferrer">
            {t('gate.renew')}
          </a>
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
    return (
      <div className="banner banner-warn">
        <span className="grow">{t('gate.unstable', { deadline: formatTime(acc.session?.offline_deadline) })}</span>
        <Button variant="ghost" onClick={reconnect}>
          {t('gate.reconnect')}
        </Button>
      </div>
    );
  }
  return null;
}

function SideNav({ open, onClose }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const logout = useLogout();
  const openReport = useReportDialog();
  const s = status.account.session;
  const left = daysLeft(s.membership_expires_at);
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);
  return (
    <>
      {open && <div className="side-backdrop" onClick={onClose} aria-hidden="true" />}
      <aside className={`side ${open ? 'open' : ''}`} id="side-nav" aria-label={t('nav.main')}>
        <div className="side-head">
          <Link to="/account" className="side-me">
            <Initial name={s.user.nickname} />
            <span className="grow">
              <b className="ellipsis">{s.user.nickname}</b>
              <span className={`small ${left > 30 ? 'muted' : 'warn-text'}`}>{left > 0 ? t('account.days_left', { n: left }) : t('account.expired')}</span>
            </span>
          </Link>
          <button type="button" className="btn btn-plain btn-icon side-close" onClick={onClose} aria-label={t('nav.close_menu')}>
            <X size={20} />
          </button>
        </div>
        <nav className="side-groups">
          <div className="side-group">
            {NAV.map((x) => (
              <Link key={x.to} to={x.to} className="side-link">
                <x.icon size={19} aria-hidden="true" />
                {t(x.key)}
                {x.badge === 'update' && shouldRemind(status.update) && (
                  <span className="nav-badge" title={t('update.badge')}>
                    {t('update.badge')}
                  </span>
                )}
                {x.meta === 'cpu' && status.metrics && (
                  <span className="nav-meta" title={t('system.cpu')}>
                    {Math.round(status.metrics.cpu)}%
                  </span>
                )}
              </Link>
            ))}
          </div>
        </nav>
        <div className="side-foot">
          <button type="button" className="side-link" onClick={openReport}>
            <Bug size={19} aria-hidden="true" />
            {t('report.title')}
          </button>
          <a className="side-link" href={webUrl(status, '/home')} target="_blank" rel="noopener noreferrer">
            <ExternalLink size={19} aria-hidden="true" />
            {t('nav.web')}
          </a>
          <button type="button" className="side-link" onClick={logout}>
            <LogOut size={19} aria-hidden="true" />
            {t('nav.logout')}
          </button>
        </div>
      </aside>
    </>
  );
}

function ConsoleLayout({ path, stale, children }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const switcher = devices.multi && DEVICE_PAGES.includes(path);
  const [open, setOpen] = useState(false);
  useEffect(() => setOpen(false), [path]);
  const s = status.account.session;
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
          {switcher && <DeviceSwitcher />}
          {status.access === 'remote' && <span className={`badge badge-ok ${switcher ? 'hide-narrow' : ''}`}>{t('nav.remote')}</span>}
          <LangSwitch />
          <Link to="/account" aria-label={t('nav.account')}>
            <Initial name={s.user.nickname} />
          </Link>
        </span>
      </header>
      <GateBanner />
      <UpdateBanner />
      <JustUpdatedBanner />
      <RelayHint />
      <div className="console">
        <SideNav open={open} onClose={() => setOpen(false)} />
        <main className="shell console-main" key={switcher ? devices.selected : 'all'}>
          {/* 刚切换电脑、新电脑的状态还没拿到时先不渲染页面，免得页面用上一台电脑的设置初始化 */}
          {switcher && stale ? (
            <div className="stack">
              <Skeleton height={60} />
              <Skeleton height={180} />
            </div>
          ) : (
            children
          )}
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
  const devices = useDeviceState(status?.access === 'remote');
  devRef.current = devices;
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
    const login = status.account.session?.device_id;
    const page = { '/': <Terminals key={login} />, '/pages': <Pages />, '/system': <System />, '/settings': <Settings />, '/account': <Account /> }[path];
    body = (
      <ConsoleLayout path={path} stale={devices.multi && statusDevice !== devices.selected}>
        {page}
      </ConsoleLayout>
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
