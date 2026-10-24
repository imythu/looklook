// 终端页：整页显示一个终端（同源 iframe 打开 /i/{id}/）。手机上底部有按键条（Esc、Tab、Ctrl/Alt/Shift、方向键、组合键、符号、F1–F12…）。
// 顶栏只放最常用的（返回、名称、传文件、按键条开关），其余收进“更多”，窄屏不会挤成一团互相遮挡；
// 终端 / 文件 / Git 的切换在手机和窄屏上放在底部（像 App 的底部标签栏），宽屏放在顶栏。
// 顶栏、按键条、标签栏都跟着终端配色（设置 → 终端配色），不会出现终端是浅色、上下两头还是深色。
// 会话页右边打开的每个会话是一个 TermPane（见 Sessions.jsx）。
import { ArrowLeft, Expand, ExternalLink, FolderOpen, GitBranch, Keyboard, Maximize2, MessagesSquare, Minimize2, Play, PlugZap, RefreshCw, Shrink, SquareTerminal, TextSelect as TextSelectIcon, WifiOff } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api, selectedDevice, terminalDevice } from '../shared/api.js';
import { CHAT_LAUNCHES } from '../shared/chat.js';
import { usePaths } from '../shared/paths.jsx';
import { termFit, termFocus, termOffline, termPaste, termSrc, useFocusWhenReady, useRefitOnReturn, useTermConn } from '../shared/termFrame.js';
import { errorText } from '../shared/i18n.js';
import { AI_LAUNCHES } from '../shared/keys.js';
import KeyBar, { isTouch, keepFocus, useKeybarShown, useTouchTerminal } from '../shared/KeyBar.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { useRouter } from '../shared/router.jsx';
import { customName, sessionTitle } from '../shared/sessions.jsx';
import TextSelect, { useTextSelect } from '../shared/TextSelect.jsx';
import { Button, Card, useLoad, useToast } from '../shared/ui.jsx';
import ChatView from './ChatView.jsx';
import FileBrowser from './FileBrowser.jsx';
import GitPanel from './GitPanel.jsx';
import { useTermFiles } from './TermFiles.jsx';
import { termTheme, termVars } from '../shared/termTheme.js';

/** 终端页面加载期间的提示：跳动的小点 + 一句人话；等得久了给出刷新按钮。 */
function TermConnecting({ onRetry }) {
  const { t } = useTranslation();
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setSlow(true), 8000);
    return () => clearTimeout(timer);
  }, []);
  return (
    <div className="term-connecting" role="status" aria-live="polite">
      <div>
        <span className="dots" aria-hidden="true">
          <i />
          <i />
          <i />
        </span>
        <p>{t('termconn.connecting')}</p>
        <p className="small">{t(slow ? 'termconn.slow' : 'termconn.connecting_hint')}</p>
        {slow && (
          <Button variant="ghost" onClick={onRetry}>
            {t('termconn.retry')}
          </Button>
        )}
      </div>
    </div>
  );
}

/**
 * 终端怎么连的：远程打开、或本机 App 打开另一台电脑的终端时，显示那台电脑实际走的路和往返时间
 * （色点同电脑菜单：绿 局域网直连、蓝 公网直连、黄 中转），慢的时候标黄。本机的终端什么也不显示。
 */
function LinkBadge({ id }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const paths = usePaths();
  const dev = terminalDevice(id) ?? selectedDevice();
  const p = dev ? paths[dev] : status.access === 'remote' ? Object.values(paths)[0] : null;
  if (!p) return null;
  const ms = p.rtt_ms;
  const tip = [t(`path.${p.kind}`), ms != null ? `${ms} ms` : null].filter(Boolean).join(' · ');
  return (
    <span className={`link-badge ${ms != null && ms >= SLOW_MS ? 'link-slow' : ''}`} title={tip}>
      <span className={`dot path-${p.kind}`} aria-hidden="true" style={{ width: 7, height: 7 }} />
      {ms != null ? `${ms} ms` : t(`path.${p.kind}`)}
    </span>
  );
}
const SLOW_MS = 150;
// AI 编程助手的输入框：⇧↵ 换行（term-inject.js 把它改成 Ctrl+J）。电脑上在顶栏空位里小字提示，手机上看按键条。
function NewlineHint({ inst, touch }) {
  const { t } = useTranslation();
  if (touch || !inst?.running || !AI_LAUNCHES.includes(inst.launch)) return null;
  return (
    <span className="term-hint" title={t('term.newline_title')}>
      <kbd>⇧</kbd>
      <kbd>↵</kbd>
      {t('term.newline_hint')}
    </span>
  );
}

/** 窄屏（手机竖屏、窄窗口）：顶栏只留图标，次要操作收进“更多”，标签放到底部。 */
export function useNarrow(query = '(max-width: 719px)') {
  const [m, setM] = useState(() => Boolean(window.matchMedia?.(query).matches));
  useEffect(() => {
    const mq = window.matchMedia?.(query);
    if (!mq) return undefined;
    const on = () => setM(mq.matches);
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, [query]);
  return m;
}

/** 终端外框的配色变量；整页终端时浏览器的地址栏 / 状态栏颜色（theme-color）也跟着终端顶栏走。 */
export function useTermChrome(theme, page = false) {
  useEffect(() => {
    if (!page) return undefined;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (!meta) return undefined;
    const old = meta.content;
    meta.content = termTheme(theme).bar;
    return () => {
      meta.content = old;
    };
  }, [theme, page]);
  return termVars(theme);
}

// （对话 /）终端 / 改动 / 文件
const TABS = [
  ['term', SquareTerminal, 'fb.tab_term'],
  ['git', GitBranch, 'fb.tab_git'],
  ['files', FolderOpen, 'fb.tab_files'],
];
const CHAT_TAB = ['chat', MessagesSquare, 'chat.tab'];
const tabsFor = (chat) => (chat ? [CHAT_TAB, ...TABS] : TABS);

/** 带对话界面的 Claude Code 终端（docs/CLAUDE_UI.md） */
export const hasChat = (inst) => Boolean(CHAT_LAUNCHES.includes(inst?.launch) && inst?.chat_ui);

/**
 * 对话模式的 Codex：终端里的 TUI 接在看看起的 app-server 上（--remote）。第一次打开终端标签时自动接上，
 * 之后断了、退出了、换成了本机版，用这个按钮重新接上（终端里还在跑的程序会先退出）。
 */
function useCodexReconnect(id, inst, frame) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const shown = inst?.launch === 'codex' && hasChat(inst) && !inst?.container;
  const run = async () => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/codex-terminal`);
      termFocus(frame);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const button = shown && (
    <button type="button" className="btn btn-plain btn-sm" onClick={run} disabled={busy} title={t('term.codex_reconnect_hint')}>
      <PlugZap size={15} />
      {t('term.codex_reconnect')}
    </button>
  );
  return { shown, busy, run, button };
}

/**
 * 当前标签，按终端记在浏览器里。带对话界面的终端默认打开“对话”。
 */
function useTab(id, chat) {
  const key = `ll-tab:${id}`;
  const read = () => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  };
  const [tab, setTabState] = useState(() => read() ?? 'term');
  const chosen = useRef(read() != null);
  useEffect(() => {
    if (chat === undefined) return;
    if (chat && !chosen.current) setTabState('chat');
    if (!chat && tab === 'chat') setTabState('term');
  }, [chat]);
  const setTab = (v) => {
    chosen.current = true;
    setTabState(v);
    try {
      localStorage.setItem(key, v);
    } catch {
      /* 存不了就只记在这次打开里 */
    }
  };
  return [tab, setTab];
}

/** 宽屏：顶栏里的分段标签。切到别的标签时终端 iframe 不卸载，会话保持连接。 */
export function TermTabs({ tab, onChange, chat }) {
  const { t } = useTranslation();
  return (
    <div className="term-tabs" role="tablist" aria-label={t('fb.tabs')}>
      {tabsFor(chat).map(([key, Icon, label]) => (
        <button key={key} type="button" role="tab" aria-selected={tab === key} className={`term-tab ${tab === key ? 'on' : ''}`} onClick={() => onChange(key)} title={t(label)}>
          <Icon size={16} aria-hidden="true" />
          <span className="term-tab-label">{t(label)}</span>
          {key === 'chat' && <span className="beta-tag">Beta</span>}
        </button>
      ))}
    </div>
  );
}

/** 手机、窄屏：底部标签栏（图标在上、文字在下），拇指够得着，也不和顶栏的按钮抢地方。 */
function TermTabBar({ tab, onChange, chat }) {
  const { t } = useTranslation();
  return (
    <nav className="term-tabbar" role="tablist" aria-label={t('fb.tabs')}>
      {tabsFor(chat).map(([key, Icon, label]) => (
        <button key={key} type="button" role="tab" aria-selected={tab === key} onClick={() => onChange(key)}>
          <Icon size={20} aria-hidden="true" />
          <span>
            {t(label)}
            {key === 'chat' && <span className="beta-tag">Beta</span>}
          </span>
        </button>
      ))}
    </nav>
  );
}

/**
 * 终端所在的电脑离线了：终端（和对话）照样显示、能翻看，但不能输入；告诉终端页面别再显示它自己的重连提示，
 * 电脑回来后让它马上重连。`offline`：电脑连不上；终端自己的连接还通着（例如局域网直连）时不提示、照常能输入。
 */
function useOffline(frame, offline, loaded) {
  const conn = useTermConn(frame);
  useEffect(() => {
    if (loaded) termOffline(frame, offline);
  }, [offline, loaded]);
  return offline && (!conn || !loaded);
}

function OfflineNote() {
  const { t } = useTranslation();
  return (
    <div className="term-offline" role="status">
      <WifiOff size={15} aria-hidden="true" />
      <span>{t('term.offline')}</span>
    </div>
  );
}

/** 终端没在运行 / 找不到时，终端区域里的提示。 */
function PaneNotice({ title, body, children }) {
  return (
    <div className="pane-notice">
      <div>
        {title && <b>{title}</b>}
        {body && <p>{body}</p>}
        {children}
      </div>
    </div>
  );
}

/**
 * 会话页右边的一个会话：上面一条细的工具栏（类型、名称、电脑和项目、连接方式、标签、刷新、传文件、最大化、全屏、新窗口、更多），下面是终端。
 * 不是当前标签时只是隐藏（iframe 不卸载、会话保持连接），切回来时让终端重新适配大小并聚焦。
 * 最大化：铺满整个浏览器窗口（盖住侧栏和标签栏），但不进系统全屏；双击工具栏空白处也能切换。
 */
export function TermPane({ id, inst, active, missing, offline = false, onStart, lead, sub, menu, focusN = 0 }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const wrap = useRef(null);
  const frame = useRef(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [starting, setStarting] = useState(false);
  const [full, setFull] = useState(false);
  const [max, setMax] = useState(false);
  const chat = inst ? hasChat(inst) : undefined;
  const [tab0, setTab] = useTab(id, chat);
  // 容器里的会话只有终端（文件、Git 看的是这台电脑上的目录，对话界面拿不到容器里的记录）
  const tab = inst?.container ? 'term' : tab0;
  const link = { src: termSrc(id) };
  const allowed = status.account.allowed;
  const running = Boolean(inst?.running && allowed);
  const cut = useOffline(frame, Boolean(offline && running), loaded);
  // 把焦点交给当前标签的输入框：对话标签是对话输入框，其他是终端
  const focusInput = () => {
    if (isTouch()) return;
    const box = tab === 'chat' ? wrap.current?.querySelector('.chat-tab textarea') : null;
    if (box) box.focus({ preventScroll: true });
    else if (tab === 'term') termFocus(frame);
  };
  // “对话”标签有自己的发图片按钮：终端的上传按钮和提示只在其他标签出现
  const files = useTermFiles({ id, inst, frame, enabled: running && active && tab !== 'chat', touch: false, hostOs: status.capabilities.os, keybar: false });
  useRefitOnReturn(frame, running && active);
  useFocusWhenReady(frame, running && active && tab === 'term');
  const [selecting, setSelecting] = useTextSelect(frame, running && active && tab === 'term');
  const reconnect = useCodexReconnect(id, inst, frame);
  useEffect(() => {
    if (!active) return undefined;
    const timer = setTimeout(() => {
      termFit(frame);
      focusInput();
    }, 30);
    return () => clearTimeout(timer);
  }, [active, full, max, tab]);
  // 点了侧栏里的会话、顶部标签（同一个会话再点也算）：焦点交给输入框
  useEffect(() => {
    if (!active || !focusN) return undefined;
    const timer = setTimeout(focusInput, 30);
    return () => clearTimeout(timer);
  }, [focusN]);
  useEffect(() => {
    const on = () => setFull(document.fullscreenElement === wrap.current);
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  const reload = () => {
    setLoaded(false);
    setReloadKey((k) => k + 1);
  };
  const start = async () => {
    setStarting(true);
    try {
      await onStart?.();
      reload();
    } finally {
      setStarting(false);
    }
  };
  const toggleFull = () => (document.fullscreenElement ? document.exitFullscreen?.() : wrap.current?.requestFullscreen?.())?.catch?.(() => {});
  const toggleMax = () => setMax((m) => !m);
  const iconBtn = 'btn btn-plain btn-icon';
  let body;
  if (missing) {
    body = <PaneNotice title={t('term.missing_title')} body={t('sess.missing_body')} />;
  } else if (!inst && offline) {
    // 电脑离线、这次打开页面后还没见过这个会话：没有可显示的内容，说明一下
    body = <PaneNotice title={t('term.offline_title')} body={t('term.offline_body')} />;
  } else if (!inst) {
    body = <TermConnecting onRetry={reload} />;
  } else if (!allowed) {
    body = <PaneNotice title={t('term.locked_title')} body={t(`gate.${status.account.reason}`, { defaultValue: t('gate.other') })} />;
  } else if (!inst.running) {
    body = (
      <PaneNotice title={t('term.stopped_title')} body={t('term.stopped_body')}>
        <Button icon={Play} busy={starting} onClick={start}>
          {t('instance.start_open')}
        </Button>
      </PaneNotice>
    );
  } else {
    body = (
      <>
        {!loaded && <TermConnecting onRetry={reload} />}
        {link.src && (
          <iframe
            key={`${reloadKey}:${link.src}`}
            ref={frame}
            src={link.src}
            title={inst.name}
            allow="clipboard-read; clipboard-write; fullscreen; local-network-access"
            onLoad={() => {
              setLoaded(true);
              if (active) termFocus(frame);
            }}
          />
        )}
      </>
    );
  }
  return (
    <section ref={wrap} className={`ws-pane ${max ? 'max' : ''}`} hidden={!active} aria-label={inst?.name ?? id} data-pane-id={id}>
      <header className="term-bar ws-pane-bar" onDoubleClick={(e) => !e.target.closest('button, a, input') && !full && toggleMax()}>
        <span className="term-title">
          {lead ?? <span className={`dot ${running ? 'on' : ''}`} />}
          <span className="ellipsis term-name" title={inst ? sessionTitle(inst) : id}>{inst ? sessionTitle(inst) : id}</span>
          {customName(inst) && <span className="ellipsis term-mine">{customName(inst)}</span>}
          {sub}
          {running && <LinkBadge id={id} />}
        </span>
        {running && !inst?.container && <TermTabs tab={tab} onChange={setTab} chat={chat} />}
        {tab === 'term' && <NewlineHint inst={inst} touch={false} />}
        {running && tab === 'term' && reconnect.button}
        {running && (
          <button type="button" className={iconBtn} onClick={reload} aria-label={t('term.reload')} title={t('term.reload')}>
            <RefreshCw size={16} />
          </button>
        )}
        {running && tab === 'term' && (
          <button type="button" className={iconBtn} onClick={() => setSelecting(!selecting)} aria-pressed={selecting} aria-label={t('select.toggle')} title={t('select.toggle')}>
            <TextSelectIcon size={16} />
          </button>
        )}
        {tab !== 'chat' && files.button}
        {!full && (
          <button type="button" className={iconBtn} onClick={toggleMax} aria-pressed={max} aria-label={t(max ? 'term.restore' : 'term.maximize')} title={t(max ? 'term.restore' : 'term.maximize')}>
            {max ? <Shrink size={16} /> : <Expand size={16} />}
          </button>
        )}
        <button type="button" className={iconBtn} onClick={toggleFull} aria-label={t(full ? 'term.exit_full' : 'term.full')} title={t(full ? 'term.exit_full' : 'term.full')}>
          {full ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
        </button>
        <button type="button" className={iconBtn} onClick={() => window.open(`/t/${id}`, '_blank', 'noopener')} aria-label={t('instance.new_window')} title={t('instance.new_window')}>
          <ExternalLink size={16} />
        </button>
        {menu}
      </header>
      <div className="term-frame">
        {body}
        {cut && <OfflineNote />}
        {tab !== 'chat' && files.layer}
        {selecting && <TextSelect id={id} frame={frame} onClose={() => { setSelecting(false); termFocus(frame); }} />}
        {running && tab === 'git' && (
          <div className="term-files-tab">
            <GitPanel id={id} active={active && tab === 'git'} />
          </div>
        )}
        {running && tab === 'files' && (
          <div className="term-files-tab">
            <FileBrowser id={id} active={active && tab === 'files'} onInsert={(text) => { setTab('term'); termPaste(frame, text); }} />
          </div>
        )}
        {/* 对话一直挂着（切到别的标签时只是隐藏）：切回来时内容和滚动位置都还在 */}
        {running && chat && (
          <div className="term-files-tab chat-tab" hidden={tab !== 'chat'}>
            <ChatView id={id} launch={inst?.launch} active={active && tab === 'chat'} touch={false} offline={offline} onTab={setTab} />
          </div>
        )}
      </div>
    </section>
  );
}

export default function TerminalView({ id }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { navigate } = useRouter();
  const toast = useToast();
  const frame = useRef(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [touch] = useState(isTouch);
  const narrow = useNarrow();
  const [keys, setKeys] = useKeybarShown();
  const mobile = useTouchTerminal(frame, touch);
  const [full, setFull] = useState(false);
  const [starting, setStarting] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const link = { src: termSrc(id) };
  const [loadedInst, loadError, reload] = useLoad(() => api.get(`/instances/${id}`), [id]);
  // 连不上电脑（重连中、网络断了）时轮询会失败：沿用上一次的终端信息，不把终端换成错误页（那样会关掉终端、要点“返回”）
  const last = useRef(null);
  if (loadedInst) last.current = { id, inst: loadedInst };
  const transient = Boolean(loadError) && (!loadError.status || loadError.status >= 500);
  const inst = loadedInst ?? (transient && last.current?.id === id ? last.current.inst : null);
  const error = transient ? null : loadError;
  const chat = inst ? hasChat(inst) : undefined;
  const [tab0, setTab] = useTab(id, chat);
  const tab = inst?.container ? 'term' : tab0;
  const allowed = status.account.allowed;
  const running = inst?.running;
  const live = Boolean(running && allowed);
  // 连不上电脑（重连中、网络断了、电脑离线）：终端内容照样显示，只是不能输入
  const offline = Boolean(transient && inst);
  const cut = useOffline(frame, offline && live, loaded);
  // 底部标签栏：手机和窄屏用；键盘弹起时让给终端
  const bottomTabs = live && (touch || narrow) && !inst?.container;
  const tabbarShown = bottomTabs && !mobile.ime.up;
  // 对话下面展开着终端时也要按键条（选择框要用方向键、回车、Esc）
  const keybarShown = touch && keys && live && tab === 'term';
  const files = useTermFiles({ id, inst, frame, enabled: live && tab !== 'chat', touch, hostOs: status.capabilities.os, keybar: keybarShown || tabbarShown });
  const vars = useTermChrome(status.settings.theme, true);
  useRefitOnReturn(frame, live);
  useFocusWhenReady(frame, live && tab === 'term' && !touch);
  const [selecting, setSelecting] = useTextSelect(frame, live && tab === 'term');
  const reconnect = useCodexReconnect(id, inst, frame);
  // 选择文字时收起手机键盘，屏幕留给文字
  useEffect(() => {
    if (selecting) mobile.ime.hide();
  }, [selecting]);

  useEffect(() => {
    const iv = setInterval(reload, 5000);
    return () => clearInterval(iv);
  }, []);
  useEffect(() => {
    if (inst) document.title = `${sessionTitle(inst)} · ${t('brand')}`;
    return () => {
      document.title = t('brand');
    };
  }, [inst?.name, inst?.agent_title]);
  useEffect(() => {
    const on = () => setFull(Boolean(document.fullscreenElement));
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  // 终端所在页面尺寸变化（按键条、标签栏显示/隐藏、从“文件”切回来）时让终端重新适配。
  useEffect(() => {
    termFit(frame);
    if (tab === 'term' && !touch) termFocus(frame);
  }, [keys, tab, tabbarShown]);
  const insertPath = (text) => {
    setTab('term');
    termPaste(frame, text);
  };
  const reloadTerm = () => {
    setLoaded(false);
    setReloadKey((k) => k + 1);
  };

  const start = async () => {
    setStarting(true);
    try {
      await api.post(`/instances/${id}/start`);
      reload();
      setReloadKey((k) => k + 1);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setStarting(false);
    }
  };
  const toggleFull = () => (document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen?.())?.catch?.(() => {});
  const newWindow = () => window.open(`/t/${id}`, '_blank', 'noopener');

  let body;
  if (!inst && transient) {
    body = <TermConnecting onRetry={reload} />;
  } else if (error) {
    body = (
      <div className="term-empty">
        <Card title={t('term.missing_title')}>
          <p className="muted">{errorText(t, error)}</p>
          <Button onClick={() => navigate('/')}>{t('term.back')}</Button>
        </Card>
      </div>
    );
  } else if (!allowed) {
    body = (
      <div className="term-empty">
        <Card title={t('term.locked_title')}>
          <p className="muted">{t(`gate.${status.account.reason}`, { defaultValue: t('gate.other') })}</p>
          <Button onClick={() => navigate('/')}>{t('term.back')}</Button>
        </Card>
      </div>
    );
  } else if (inst && !running) {
    body = (
      <div className="term-empty">
        <Card title={t('term.stopped_title')}>
          <p className="muted">{t('term.stopped_body')}</p>
          <Button icon={Play} busy={starting} onClick={start}>
            {t('instance.start_open')}
          </Button>
        </Card>
      </div>
    );
  } else if (inst) {
    body = (
      <>
        {!loaded && <TermConnecting onRetry={reloadTerm} />}
        {link.src && (
          <iframe
            key={`${reloadKey}:${link.src}`}
            ref={frame}
            src={link.src}
            title={inst.name}
            allow="clipboard-read; clipboard-write; fullscreen; local-network-access"
            onLoad={() => {
              setLoaded(true);
              if (!touch) termFocus(frame);
            }}
          />
        )}
      </>
    );
  }

  // 窄屏时刷新、全屏、新窗口收进“更多”；手机上按住菜单不抢键盘焦点
  const compact = touch || narrow;
  const tap = touch ? keepFocus : undefined;
  const more = [
    tab === 'term' && live && { icon: TextSelectIcon, label: t('select.toggle'), onClick: () => setSelecting(true) },
    tab === 'term' && live && { icon: RefreshCw, label: t('term.reload'), onClick: reloadTerm },
    tab === 'term' && live && reconnect.shown && { icon: PlugZap, label: t('term.codex_reconnect'), onClick: reconnect.run },
    { icon: full ? Minimize2 : Maximize2, label: t(full ? 'term.exit_full' : 'term.full'), onClick: toggleFull },
    { icon: ExternalLink, label: t('instance.new_window'), onClick: newWindow },
  ];
  return (
    <div className="term-page" data-scheme={termTheme(status.settings.theme).scheme} data-tabbar={tabbarShown ? '' : undefined} style={{ ...vars, ...mobile.style }}>
      {/* 横屏打字时屏幕极矮：先藏起顶栏，键盘收起后回来 */}
      <header className="term-bar" hidden={mobile.cramped}>
        <button type="button" className="btn btn-plain btn-icon" onClick={() => navigate('/')} aria-label={t('term.back')} title={t('term.back')}>
          <ArrowLeft size={20} />
        </button>
        <span className="term-title">
          <span className={`dot ${live ? 'on' : ''}`} />
          <span className="ellipsis">{inst ? sessionTitle(inst) : "…"}</span>
          {live && <LinkBadge id={id} />}
        </span>
        {tab === 'term' && <NewlineHint inst={inst} touch={touch} />}
        {live && !bottomTabs && !inst?.container && <TermTabs tab={tab} onChange={setTab} chat={chat} />}
        {!compact && tab === 'term' && live && reconnect.button}
        {!compact && tab === 'term' && live && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => setSelecting(!selecting)} aria-pressed={selecting} aria-label={t('select.toggle')} title={t('select.toggle')}>
            <TextSelectIcon size={18} />
          </button>
        )}
        {!compact && tab === 'term' && live && (
          <button type="button" className="btn btn-plain btn-icon" onClick={reloadTerm} aria-label={t('term.reload')} title={t('term.reload')}>
            <RefreshCw size={18} />
          </button>
        )}
        {tab !== 'chat' && files.button}
        {touch && tab === 'term' && live && (
          <button type="button" className="btn btn-plain btn-icon" onPointerDown={keepFocus} onClick={() => setKeys(!keys)} aria-pressed={keys} aria-label={t('term.keys')} title={t('term.keys')}>
            <Keyboard size={19} />
          </button>
        )}
        {compact ? (
          <PopMenu items={more} className="btn btn-plain btn-icon" onPointerDown={tap} />
        ) : (
          <>
            <button type="button" className="btn btn-plain btn-icon" onClick={toggleFull} aria-label={t(full ? 'term.exit_full' : 'term.full')} title={t(full ? 'term.exit_full' : 'term.full')}>
              {full ? <Minimize2 size={18} /> : <Maximize2 size={18} />}
            </button>
            <button type="button" className="btn btn-plain btn-icon" onClick={newWindow} aria-label={t('instance.new_window')} title={t('instance.new_window')}>
              <ExternalLink size={18} />
            </button>
          </>
        )}
      </header>
      <div className="term-frame">
        {body}
        {cut && <OfflineNote />}
        {tab !== 'chat' && files.layer}
        {live && tab === 'files' && (
          <div className="term-files-tab">
            <FileBrowser id={id} active={tab === 'files'} onInsert={insertPath} />
          </div>
        )}
        {selecting && <TextSelect id={id} frame={frame} onClose={() => { setSelecting(false); if (!touch) termFocus(frame); }} />}
        {live && tab === 'git' && (
          <div className="term-files-tab">
            <GitPanel id={id} active={tab === 'git'} />
          </div>
        )}
        {live && chat && (
          <div className="term-files-tab chat-tab" hidden={tab !== 'chat'}>
            <ChatView id={id} launch={inst?.launch} active={tab === 'chat'} touch={touch} offline={offline} onTab={setTab} />
          </div>
        )}
      </div>
      {keybarShown && !selecting && <KeyBar frame={frame} os={status.capabilities.os} ime={mobile.ime} tight={mobile.tight} ai={AI_LAUNCHES.includes(inst?.launch)} />}
      {tabbarShown && <TermTabBar tab={tab} onChange={setTab} chat={chat} />}
    </div>
  );
}
