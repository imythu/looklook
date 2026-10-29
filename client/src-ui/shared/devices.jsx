// 多设备（服务端 docs/05 §9）：远程打开时，同一个账户可能有好几台电脑在线。
// 终端页合并显示所有电脑的终端；本机网页、设置、系统等按电脑的页面用顶栏的“电脑”切换，选择记在 sessionStorage。
// 本机 App 也一样：列表由这台电脑向平台取，发给其他电脑的请求经它转过去（客户端 rtc/outbound.rs）。
// 局域网打开时列表为空，只有一台电脑时界面与单设备完全一样。
import { Check, ChevronDown, HelpCircle, Monitor, X } from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { configureDevices, deviceApi, fetchDevices, localApi, rtcMode } from './api.js';
import { DeviceAvatar } from './brands.jsx';
import { linkLabel, PathDot, PathLine, useLinks } from './paths.jsx';
import { useTip } from './tips.jsx';
import { Button, ErrorNote, Field, Modal, Switch, useAction, useToast } from './ui.jsx';

const KEY = 'looklook.device';
const NONE = { ready: true, multi: false, localOnly: false, pinned: null, canLocalOnly: false, setLocalOnly: async () => {}, items: [], def: null, selected: null, max: null, select: () => {}, byId: () => null };
const Ctx = createContext(NONE);

export function useDevices() {
  return useContext(Ctx);
}

/** 记住的电脑；地址带 ?d={id}（“电脑”页、看看网页上指定了某台电脑的链接）时用它，并从地址里去掉。 */
function stored() {
  const u = new URL(window.location.href);
  const want = u.searchParams.get('d');
  if (want) {
    store(want);
    u.searchParams.delete('d');
    window.history.replaceState(window.history.state, '', u.pathname + u.search + u.hash);
    return want;
  }
  // 记在这个浏览器里（关掉再开、新标签页都还是它）；以前只记在当前标签页里，换个标签就回到默认电脑
  try {
    return localStorage.getItem(KEY) ?? sessionStorage.getItem(KEY);
  } catch {
    return null;
  }
}

function store(id) {
  try {
    if (id) localStorage.setItem(KEY, id);
    else localStorage.removeItem(KEY);
    sessionStorage.removeItem(KEY);
  } catch {
    /* 无痕模式等：只是不记住选择 */
  }
}

/**
 * 选中的电脑：记住的那台还在列表里就一直用它（短暂离线也不换，离线时页面显示离线，回来了接着用，
 * 不在几台电脑之间跳来跳去），否则用入口的默认电脑，再否则第一台在线的。
 */
export function pickSelected(items, def, want) {
  const online = items.filter((d) => d.online);
  if (want && items.some((d) => d.id === want)) return want;
  if (def && items.some((d) => d.id === def)) return def;
  return online[0]?.id ?? items[0]?.id ?? null;
}

// 网页的“只看这台电脑”：记在这个浏览器里（公司电脑上开着，家里的手机照常看到全部）。
const PIN_KEY = 'looklook.only-device';
function readPin() {
  try {
    return localStorage.getItem(PIN_KEY);
  } catch {
    return null;
  }
}
function writePin(id) {
  try {
    if (id) localStorage.setItem(PIN_KEY, id);
    else localStorage.removeItem(PIN_KEY);
  } catch {
    /* 无痕模式：不记住 */
  }
}

/**
 * 登录后加载并定时刷新设备列表（`remote` 为假时什么也不做）。
 * `onSelect`：切换电脑后调用（刷新客户端状态）。
 */
export function useDeviceState(remote, onSelect) {
  const [list, setList] = useState(null); // null = 还没加载
  const [want, setWant] = useState(stored);
  const [pin, setPin] = useState(() => (rtcMode ? readPin() : null));
  // 一次没拿到（网络抖动、中转重启）时保留上一次的列表：清空会让页面在“单台/多台”之间来回切换，
  // 整个页面重新挂载，打开着的终端小窗也就没了。只有第一次拿不到时才当作没有设备。
  const load = useCallback(() => fetchDevices().then((d) => setList((prev) => d ?? prev ?? { items: [], default: null, max: null })), []);
  useEffect(() => {
    if (!remote) return undefined;
    load();
    const id = setInterval(load, 15000);
    window.addEventListener('focus', load);
    return () => {
      clearInterval(id);
      window.removeEventListener('focus', load);
    };
  }, [remote, load]);
  if (!remote) {
    configureDevices({ multi: false });
    return NONE;
  }
  if (!list) return { ...NONE, ready: false };
  // 只看本机（本机 App，客户端 /api/rtc/local-only）：列表为空，界面按单台电脑显示，所有请求都在本机处理。
  // 网页（WebRTC）里是“只看这台电脑”：只留当前选中的那台，界面按单台显示，请求仍然指定发给它。
  const setLocalOnly = async (on, current) => {
    if (rtcMode) {
      writePin(on ? current : null);
      setPin(on ? current : null);
      return;
    }
    await localApi.put('/rtc/local-only', { on });
    store(null);
    setWant(null);
    await load();
    onSelect?.();
  };
  if (list.localOnly) {
    configureDevices({ multi: false });
    return { ...NONE, localOnly: true, canLocalOnly: true, setLocalOnly, reload: load };
  }
  const pinned = pin && list.items.find((d) => d.id === pin);
  if (pinned) {
    // 只有一台，但不一定是入口的默认电脑：路由仍按多台处理，请求都带上它
    configureDevices({ multi: true, selected: pinned.id, devices: [pinned] });
    return { ...NONE, localOnly: true, canLocalOnly: true, setLocalOnly, pinned: pinned.id, items: [pinned], def: pinned.id, selected: pinned.id, byId: (id) => (id === pinned.id ? pinned : null), reload: load };
  }
  const items = list.items;
  const multi = items.length > 1;
  const selected = multi ? pickSelected(items, list.default, want) : null;
  // 在渲染时同步给 api.js（幂等），子组件的请求从第一次渲染起就带上正确的设备
  configureDevices({ multi, selected, devices: items });
  // remember 为假：只是这次临时换过去（选中的那台连不上），不改记住的选择
  const select = (id, remember = true) => {
    if (remember) store(id);
    setWant(id);
    configureDevices({ multi, selected: pickSelected(items, list.default, id), devices: items });
    onSelect?.();
  };
  // 本机 App（客户端会回 local_only）和网页（WebRTC）都能只看一台；局域网打开时没有其他电脑
  const canLocalOnly = list.localOnly === false || (rtcMode && multi);
  return { ready: true, multi, localOnly: false, canLocalOnly, setLocalOnly, items, def: list.default, selected, max: list.max, select, byId: (id) => items.find((d) => d.id === id) ?? null, reload: load };
}

export function DevicesProvider({ value, children }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

const OS = { linux: 'Linux', windows: 'Windows', macos: 'macOS', darwin: 'macOS' };
export function osLabel(os) {
  return OS[(os || '').toLowerCase()] ?? os ?? '';
}

export function deviceName(t, d) {
  return d?.name || t('devices.unnamed');
}

/**
 * 顶栏的电脑切换（多台电脑时在所有页面显示）：一个写着当前电脑名的按钮，点开是电脑列表。
 * - 按电脑的页面（本机网页、系统、设置）：选一台就切换到那台；
 * - 终端页（合并显示所有电脑）：按钮写“N 台电脑”，点一台跳到它的终端分组。
 * 第一次看到时在按钮下面提示一次“在这里切换电脑”，免得用户不知道有这个入口。
 * `helpUrl`：多台电脑的说明（看看网页的常见问题）。
 * `side`：侧栏“工具”上方的整行样式，表示下面的工具作用在哪台电脑上。
 */
export function DeviceSwitcher({ perDevice, helpUrl, side = false }) {
  const { t } = useTranslation();
  const d = useDevices();
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  // 侧栏里的按钮本身写着“工具作用于这台电脑”，不再弹提示（免得盖住下面的工具）
  const [tip, dismissTip] = useTip('devices.switch', d.multi && !side);
  useEffect(() => {
    if (!open) return undefined;
    const close = (e) => {
      if (e.type === 'keydown' ? e.key === 'Escape' : !ref.current?.contains(e.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', close);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', close);
    };
  }, [open]);
  if (!d.multi && !d.localOnly) return null;
  if (d.localOnly) return <LocalOnlySwitcher side={side} />;
  const current = d.byId(d.selected);
  const online = d.items.filter((x) => x.online).length;
  const links = useLinks();
  const toggle = () => {
    dismissTip();
    setOpen((o) => !o);
  };
  const pick = (x) => {
    setOpen(false);
    if (perDevice) {
      if (x.id !== d.selected) d.select(x.id);
      return;
    }
    document.getElementById(`device-${x.id}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };
  return (
    <div className={`device-switch${side ? ' device-switch-side' : ''}`} ref={ref}>
      <button type="button" className="device-pill" aria-haspopup="menu" aria-expanded={open} onClick={toggle} title={t(perDevice ? 'devices.switch_hint' : 'devices.list_hint')}>
        {perDevice ? (
          <>
            <DeviceAvatar device={current} size={side ? 24 : 20} dot />
            {side ? (
              <span className="device-pill-text">
                <span className="ellipsis">{deviceName(t, current)}</span>
                <span className="small muted ellipsis">{t('devices.tools_on')}</span>
              </span>
            ) : (
              <span className="ellipsis">{deviceName(t, current)}</span>
            )}
          </>
        ) : (
          <span className="ellipsis">
            <Monitor size={16} aria-hidden="true" />
            {online}/{d.items.length}
            <span className="pill-word"> {t('devices.online')}</span>
          </span>
        )}
        <ChevronDown size={15} aria-hidden="true" className="device-chev" />
      </button>
      {tip && !open && (
        <div className="device-tip" role="note">
          <span>{t(perDevice ? 'devices.tip_switch' : 'devices.tip_list', { count: d.items.length })}</span>
          <button type="button" className="btn btn-plain btn-icon" onClick={dismissTip} aria-label={t('action.close')}>
            <X size={14} />
          </button>
        </div>
      )}
      {open && (
        <div className="device-menu" role="menu" aria-label={t('devices.switch')}>
          <div className="device-menu-head">
            <b>{t(perDevice ? 'devices.switch' : 'devices.mine')}</b>
            <span className="small muted">{t(perDevice ? 'devices.switch_hint' : 'devices.list_hint')}</span>
          </div>
          {d.items.map((x) => {
            const selected = perDevice && x.id === d.selected;
            return (
              <button
                key={x.id}
                type="button"
                role={perDevice ? 'menuitemradio' : 'menuitem'}
                aria-checked={perDevice ? selected : undefined}
                className={`device-item ${selected ? 'selected' : ''}`}
                disabled={perDevice && !x.online}
                onClick={() => pick(x)}
              >
                <DeviceAvatar device={x} size={26} dot />
                <span className="device-item-text">
                  <span className="ellipsis">{deviceName(t, x)}</span>
                  <span className="small muted">
                    {[osLabel(x.os), linkLabel(t, x, links), x.id === d.def ? t('devices.default') : null].filter(Boolean).join(' · ')}
                  </span>
                  {x.online && !(!rtcMode && x.id === d.def) && <PathLine device={x.id} />}
                </span>
                {x.online && <PathDot device={x.id} self={!rtcMode && x.id === d.def} />}
                {selected ? <Check size={16} aria-hidden="true" /> : <span className="device-check-gap" aria-hidden="true" />}
              </button>
            );
          })}
          {d.canLocalOnly && <LocalOnlyToggle />}
          {helpUrl && (
            <a className="device-menu-foot small" href={helpUrl} target="_blank" rel="noopener noreferrer">
              <HelpCircle size={14} aria-hidden="true" />
              {t('devices.help')}
            </a>
          )}
        </div>
      )}
    </div>
  );
}

/** 电脑菜单底部的“只看本机”开关（本机 App）。 */
export function LocalOnlyToggle({ menu = true }) {
  const { t } = useTranslation();
  const d = useDevices();
  const toast = useToast();
  const { busy, error, run } = useAction();
  const web = Boolean(d.pinned) || rtcMode;
  const label = t(web ? 'devices.only_this' : 'devices.local_only');
  const set = async (on) => {
    const r = await run(() => d.setLocalOnly(on, d.selected).then(() => true));
    if (r) toast(t(on ? (web ? 'devices.only_this_on' : 'devices.local_only_on') : 'devices.local_only_off'));
  };
  return (
    <div className={menu ? 'device-local-only' : 'stack'} style={menu ? undefined : { gap: 6 }}>
      <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
        <span>
          <b>{label}</b>
          <span className="small muted" style={{ display: 'block' }}>
            {t(web ? (d.localOnly ? 'devices.only_this_hint_on' : 'devices.only_this_hint') : d.localOnly ? 'devices.local_only_hint_on' : 'devices.local_only_hint')}
          </span>
        </span>
        <Switch checked={d.localOnly} disabled={busy} onChange={set} label={label} />
      </div>
      <ErrorNote error={error} />
    </div>
  );
}

/** 只看本机时的电脑按钮：写着“本机 · 只看本机”，点开只有那个开关，随时可以关掉。 */
function LocalOnlySwitcher({ side }) {
  const { t } = useTranslation();
  const d = useDevices();
  const pinned = d.pinned ? d.byId(d.pinned) : null;
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const close = (e) => {
      if (e.type === 'keydown' ? e.key === 'Escape' : !ref.current?.contains(e.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', close);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', close);
    };
  }, [open]);
  return (
    <div className={`device-switch${side ? ' device-switch-side' : ''}`} ref={ref}>
      <button type="button" className="device-pill" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)} title={t(pinned ? 'devices.only_this_hint_on' : 'devices.local_only_hint_on')}>
        {pinned ? <DeviceAvatar device={pinned} size={side ? 24 : 20} dot /> : <Monitor size={side ? 20 : 16} aria-hidden="true" />}
        {side ? (
          <span className="device-pill-text">
            <span className="ellipsis">{pinned ? deviceName(t, pinned) : t('devices.this_computer')}</span>
            <span className="small muted ellipsis">{t(pinned ? 'devices.only_this_short' : 'devices.local_only')}</span>
          </span>
        ) : (
          <span className="ellipsis">{pinned ? deviceName(t, pinned) : t('devices.local_only')}</span>
        )}
        {pinned && <PathDot device={pinned.id} />}
        <ChevronDown size={15} aria-hidden="true" className="device-chev" />
      </button>
      {open && (
        <div className="device-menu" role="menu" aria-label={t('devices.local_only')}>
          <LocalOnlyToggle />
        </div>
      )}
    </div>
  );
}

/**
 * 给电脑改名（远程打开、多台电脑时在终端页每台电脑的标题旁）：请求发给那台电脑，由它保存到本机设置并立即同步到平台。
 * 远程入口的电脑列表由平台下发，稍等一下再刷新。`onDone(name)` 在保存成功后调用。
 */
export function RenameDeviceDialog({ device, onClose, onDone }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [name, setName] = useState(device.name || '');
  const { busy, error, run } = useAction();
  const submit = async (e) => {
    e.preventDefault();
    const r = await run(() => deviceApi(device.id).put('/device/name', { name: name.trim() }));
    if (r) {
      toast(t('devices.renamed'));
      onDone?.(r.name);
    }
  };
  return (
    <Modal open onClose={onClose} title={t('devices.rename_title')}>
      <form onSubmit={submit}>
        <Field label={t('settings.device_name')} hint={t('devices.rename_hint')}>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={64} autoFocus placeholder={t('devices.name_placeholder')} />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy}>
            {t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
