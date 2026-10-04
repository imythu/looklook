// 多设备（服务端 docs/05 §9）：远程打开时，同一个账户可能有好几台电脑在线。
// 终端页合并显示所有电脑的终端；本机网页、设置、系统等按电脑的页面用顶栏的“电脑”切换，选择记在 sessionStorage。
// 本机/局域网打开不请求设备列表，只有一台电脑时界面与单设备完全一样。
import { Monitor } from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { configureDevices, fetchDevices } from './api.js';

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
  const load = useCallback(() => fetchDevices().then((d) => setList(d ?? { items: [], default: null, max: null })), []);
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

/** 顶栏的电脑切换（只在多台电脑、按电脑的页面上显示）。 */
export function DeviceSwitcher() {
  const { t } = useTranslation();
  const d = useDevices();
  if (!d.multi) return null;
  return (
    <label className="device-switch" title={t('devices.switch_hint')}>
      <Monitor size={16} aria-hidden="true" />
      <select value={d.selected ?? ''} onChange={(e) => d.select(e.target.value)} aria-label={t('devices.switch')}>
        {d.items.map((x) => (
          <option key={x.id} value={x.id} disabled={!x.online}>
            {deviceName(t, x)}
            {x.online ? '' : ` · ${t('devices.offline')}`}
          </option>
        ))}
      </select>
    </label>
  );
}
