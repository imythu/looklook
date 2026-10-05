// 多设备（服务端 docs/05 §9）：远程打开时，同一个账户可能有好几台电脑在线。
// 终端页合并显示所有电脑的终端；本机网页、设置、系统等按电脑的页面用顶栏的“电脑”切换，选择记在 sessionStorage。
// 本机/局域网打开不请求设备列表，只有一台电脑时界面与单设备完全一样。
import { Check, ChevronDown, HelpCircle, Monitor, X } from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { configureDevices, deviceApi, fetchDevices } from './api.js';
import { Button, ErrorNote, Field, Modal, useAction, useToast } from './ui.jsx';

const KEY = 'looklook.device';
const NONE = { ready: true, multi: false, items: [], def: null, selected: null, max: null, select: () => {}, byId: () => null };
const Ctx = createContext(NONE);

export function useDevices() {
  return useContext(Ctx);
}

function stored() {
  try {
    return sessionStorage.getItem(KEY);
  } catch {
    return null;
  }
}

function store(id) {
  try {
    if (id) sessionStorage.setItem(KEY, id);
    else sessionStorage.removeItem(KEY);
  } catch {
    /* 无痕模式等：只是不记住选择 */
  }
}

/** 选中的电脑：记住的那台还在线就用它，否则用入口的默认电脑，再否则第一台在线的。 */
export function pickSelected(items, def, want) {
  const online = items.filter((d) => d.online);
  if (want && online.some((d) => d.id === want)) return want;
  if (def && items.some((d) => d.id === def)) return def;
  return online[0]?.id ?? items[0]?.id ?? null;
}

/**
 * 远程打开时加载并定时刷新设备列表（`remote` 为假时什么也不做）。
 * `onSelect`：切换电脑后调用（刷新客户端状态）。
 */
export function useDeviceState(remote, onSelect) {
  const [list, setList] = useState(null); // null = 还没加载
  const [want, setWant] = useState(stored);
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
  const items = list.items;
  const multi = items.length > 1;
  const selected = multi ? pickSelected(items, list.default, want) : null;
  // 在渲染时同步给 api.js（幂等），子组件的请求从第一次渲染起就带上正确的设备
  configureDevices({ multi, selected, devices: items });
  const select = (id) => {
    store(id);
    setWant(id);
    configureDevices({ multi, selected: pickSelected(items, list.default, id), devices: items });
    onSelect?.();
  };
  return { ready: true, multi, items, def: list.default, selected, max: list.max, select, byId: (id) => items.find((d) => d.id === id) ?? null, reload: load };
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

const TIP_KEY = 'looklook.device-tip';

function tipSeen() {
  try {
    return localStorage.getItem(TIP_KEY) === '1';
  } catch {
    return true;
  }
}

function markTipSeen() {
  try {
    localStorage.setItem(TIP_KEY, '1');
  } catch {
    /* 无痕模式等：只是下次还会提示 */
  }
}

/**
 * 顶栏的电脑切换（多台电脑时在所有页面显示）：一个写着当前电脑名的按钮，点开是电脑列表。
 * - 按电脑的页面（本机网页、系统、设置）：选一台就切换到那台；
 * - 终端页（合并显示所有电脑）：按钮写“N 台电脑”，点一台跳到它的终端分组。
 * 第一次看到时在按钮下面提示一次“在这里切换电脑”，免得用户不知道有这个入口。
 * `helpUrl`：多台电脑的说明（看看网页的常见问题）。
 */
export function DeviceSwitcher({ perDevice, helpUrl }) {
  const { t } = useTranslation();
  const d = useDevices();
  const [open, setOpen] = useState(false);
  const [tip, setTip] = useState(() => !tipSeen());
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
  if (!d.multi) return null;
  const current = d.byId(d.selected);
  const online = d.items.filter((x) => x.online).length;
  const dismissTip = () => {
    markTipSeen();
    setTip(false);
  };
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
    <div className="device-switch" ref={ref}>
      <button type="button" className="device-pill" aria-haspopup="menu" aria-expanded={open} onClick={toggle} title={t(perDevice ? 'devices.switch_hint' : 'devices.list_hint')}>
        <Monitor size={16} aria-hidden="true" />
        {perDevice ? (
          <>
            <span className={`device-dot ${current?.online ? 'on' : ''}`} aria-hidden="true" />
            <span className="ellipsis">{deviceName(t, current)}</span>
          </>
        ) : (
          <span className="ellipsis">
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
                <span className={`device-dot ${x.online ? 'on' : ''}`} aria-hidden="true" />
                <span className="device-item-text">
                  <span className="ellipsis">{deviceName(t, x)}</span>
                  <span className="small muted">
                    {[osLabel(x.os), t(x.online ? 'devices.online' : 'devices.offline'), x.id === d.def ? t('devices.default') : null].filter(Boolean).join(' · ')}
                  </span>
                </span>
                {selected && <Check size={16} aria-hidden="true" />}
              </button>
            );
          })}
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
