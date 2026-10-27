// 网页类型的会话（DSH）：在会话标签里用 iframe 打开它的网页界面 /w/{id}/（和管理台同源，见 src/gateway/dsh.rs）。
// 打开前先请电脑确保 DSH 在运行、网关已经用启动口令登录好；口令和 DSH 的 Cookie 都只在电脑上，浏览器拿不到。
// 电脑上是会话页右边的一块（WebPane），手机上是整页（WebPage，/t/{id}?web）。
import { ArrowLeft, Expand, ExternalLink, Globe, Maximize2, Minimize2, RefreshCw, Shrink } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi, selectedDevice, terminalDevice } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { useRouter } from '../shared/router.jsx';
import { sessionTitle } from '../shared/sessions.jsx';
import { APP_CHROME } from '../shared/termTheme.js';
import { Button, Spinner, useToast } from '../shared/ui.jsx';
import { openDsh } from './Terminals.jsx';

/** 网页界面的地址。多台电脑时带上它所在的电脑（__lldev，启动页 / 本机客户端按它转给那台电脑，之后的请求按会话 id 转）。 */
export function webSrc(id) {
  const device = terminalDevice(id) ?? selectedDevice();
  return device ? `/w/${id}/?__lldev=${encodeURIComponent(device)}` : `/w/${id}/`;
}

/**
 * 准备好再显示 iframe：`state` 是 starting（等电脑把 DSH 启动、登录好）/ ready / slow（还没好，等着重试）/ error。
 * 没打开过（不是当前标签）时不去准备。
 */
function useWebFrame(id, enabled) {
  const [state, setState] = useState('idle');
  const [error, setError] = useState(null);
  const [n, setN] = useState(0);
  useEffect(() => {
    if (!enabled || state === 'ready') return undefined;
    let gone = false;
    let timer = 0;
    let tries = 0;
    setState('starting');
    const go = async () => {
      try {
        const r = await deviceApi(terminalDevice(id)).post(`/instances/${id}/web`);
        if (gone) return;
        if (r.ready) return setState('ready');
        // dsh 第一次运行要初始化配置，会慢很多：一直等，过一会儿告诉用户还在启动
        tries += 1;
        if (tries >= 2) setState('slow');
        timer = setTimeout(go, 2000);
      } catch (e) {
        if (gone) return;
        setError(e);
        setState('error');
      }
    };
    go();
    return () => {
      gone = true;
      clearTimeout(timer);
    };
  }, [id, enabled, n]);
  const retry = () => {
    setError(null);
    setState('idle');
    setN((x) => x + 1);
  };
  return { state, error, retry };
}

function WebBody({ id, inst, web, frame, reloadKey, active }) {
  const { t } = useTranslation();
  const [loaded, setLoaded] = useState(false);
  useEffect(() => setLoaded(false), [reloadKey]);
  // 切回来时把焦点交给网页（DSH 的输入框由它自己决定）
  useEffect(() => {
    if (active && loaded) frame.current?.focus();
  }, [active, loaded]);
  if (web.state === 'error') {
    return (
      <div className="pane-notice">
        <div>
          <b>{t('dsh.open_failed')}</b>
          <p>{errorText(t, web.error)}</p>
          <Button icon={RefreshCw} onClick={web.retry}>
            {t('dsh.retry')}
          </Button>
        </div>
      </div>
    );
  }
  if (web.state !== 'ready') {
    return (
      <div className="term-connecting" role="status" aria-live="polite">
        <div>
          <Spinner size={16} /> {t(web.state === 'slow' ? 'dsh.starting_slow' : 'dsh.connecting')}
        </div>
      </div>
    );
  }
  return (
    <>
      {!loaded && (
        <div className="term-connecting" role="status" aria-live="polite">
          <div>
            <Spinner size={16} /> {t('dsh.connecting')}
          </div>
        </div>
      )}
      <iframe
        key={reloadKey}
        ref={frame}
        className="web-frame"
        src={webSrc(id)}
        title={inst?.name ?? id}
        allow="clipboard-read; clipboard-write; fullscreen"
        onLoad={() => setLoaded(true)}
      />
    </>
  );
}

/** 会话页右边的网页会话：上面一条和终端一样的工具栏（名字、电脑和项目、刷新、最大化、全屏、在浏览器打开、更多），下面是网页。 */
export function WebPane({ id, inst, active, missing, lead, sub, menu, named = true }) {
  const { t } = useTranslation();
  const toast = useToast();
  const wrap = useRef(null);
  const frame = useRef(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [full, setFull] = useState(false);
  const [max, setMax] = useState(false);
  // 打开过一次就一直挂着（切走只是隐藏），切回来不用重新加载
  const [opened, setOpened] = useState(active);
  useEffect(() => {
    if (active) setOpened(true);
  }, [active]);
  const web = useWebFrame(id, opened && !missing && Boolean(inst));
  useEffect(() => {
    const on = () => setFull(document.fullscreenElement === wrap.current);
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  const toggleFull = () => (document.fullscreenElement ? document.exitFullscreen?.() : wrap.current?.requestFullscreen?.())?.catch?.(() => {});
  const iconBtn = 'btn btn-plain btn-icon';
  const showName = named || max || full;
  return (
    <section ref={wrap} className={`ws-pane web-pane ${max ? 'max' : ''}`} hidden={!active} aria-label={inst?.name ?? id} data-pane-id={id} data-scheme="app" style={APP_CHROME}>
      <header className="term-bar ws-pane-bar" onDoubleClick={(e) => !e.target.closest('button, a, input') && !full && setMax((m) => !m)}>
        <span className="term-title">
          {showName && (lead ?? <Globe size={16} aria-hidden="true" />)}
          {showName && <span className="ellipsis term-name">{inst ? sessionTitle(inst) : id}</span>}
          {sub}
        </span>
        {web.state === 'ready' && (
          <button type="button" className={iconBtn} onClick={() => setReloadKey((k) => k + 1)} aria-label={t('term.reload')} title={t('term.reload')}>
            <RefreshCw size={16} />
          </button>
        )}
        {!full && (
          <button type="button" className={iconBtn} onClick={() => setMax((m) => !m)} aria-pressed={max} aria-label={t(max ? 'term.restore' : 'term.maximize')} title={t(max ? 'term.restore' : 'term.maximize')}>
            {max ? <Shrink size={16} /> : <Expand size={16} />}
          </button>
        )}
        <button type="button" className={iconBtn} onClick={toggleFull} aria-label={t(full ? 'term.exit_full' : 'term.full')} title={t(full ? 'term.exit_full' : 'term.full')}>
          {full ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
        </button>
        {inst && (
          <button type="button" className={iconBtn} onClick={() => openDsh(inst, t, toast)} aria-label={t('dsh.open_browser')} title={t('dsh.open_browser')}>
            <ExternalLink size={16} />
          </button>
        )}
        {menu}
      </header>
      <div className="term-frame">
        {missing ? (
          <div className="pane-notice">
            <div>
              <b>{t('term.missing_title')}</b>
              <p>{t('sess.missing_body')}</p>
            </div>
          </div>
        ) : (
          opened && <WebBody id={id} inst={inst} web={web} frame={frame} reloadKey={reloadKey} active={active} />
        )}
      </div>
    </section>
  );
}

/** 手机上的整页网页会话（/t/{id}?web）。 */
export function WebPage({ id, inst }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const toast = useToast();
  const frame = useRef(null);
  const [reloadKey, setReloadKey] = useState(0);
  const web = useWebFrame(id, true);
  return (
    <div className="term-page web-page" data-scheme="app" style={APP_CHROME}>
      <header className="term-bar">
        <button type="button" className="btn btn-plain btn-icon" onClick={() => navigate('/')} aria-label={t('term.back')} title={t('term.back')}>
          <ArrowLeft size={20} />
        </button>
        <span className="term-title">
          <Globe size={16} aria-hidden="true" />
          <span className="ellipsis">{inst ? sessionTitle(inst) : '…'}</span>
        </span>
        {web.state === 'ready' && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => setReloadKey((k) => k + 1)} aria-label={t('term.reload')} title={t('term.reload')}>
            <RefreshCw size={18} />
          </button>
        )}
        {inst && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => openDsh(inst, t, toast)} aria-label={t('dsh.open_browser')} title={t('dsh.open_browser')}>
            <ExternalLink size={18} />
          </button>
        )}
      </header>
      <div className="term-frame">
        <WebBody id={id} inst={inst} web={web} frame={frame} reloadKey={reloadKey} active />
      </div>
    </div>
  );
}
