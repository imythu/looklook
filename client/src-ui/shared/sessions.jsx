// 会话数据：所有电脑上的会话（终端）和项目，侧栏、开始页、会话页共用，每 5 秒刷新一次。
// 项目 = 某台电脑上的一个文件夹（或某个 Docker 容器里的目录）：那台电脑上记着的（GET/PUT /api/projects，上次用的类型和“高级”选项）
// 加上从会话的文件夹推出来的。离线的电脑拿不到列表，用上一次看到的（记在浏览器里）灰着显示。
// 顺序：有会话在运行的在前，再按启动时间从新到旧——不按最后输出的时间，否则 agent 一输出列表就跳。
// 打开的会话在顶部排成标签（tabs，按打开的先后，可以拖动），最近看过的几个在后台保持连接（recent）。
import { createContext, useContext, useEffect, useRef, useState } from 'react';

import { api, ApiError, deviceApi, rememberTerminals, terminalDevice } from './api.js';
import { useDevices } from './devices.jsx';

const Ctx = createContext(null);

export function useSessions() {
  return useContext(Ctx);
}

const GROUP_TIMEOUT_MS = 10000;
const POLL_MS = 5000;
const RECENT_KEY = 'looklook.recent';
const TABS_KEY = 'looklook.tabs';
// 顶部最多几个标签（再多就把最早打开的收起）
export const TABS_MAX = 40;
const CACHE_KEY = 'looklook.devcache';
// 后台保持连接的会话最多几个（每个都连着终端，太多会拖慢页面）
export const RECENT_MAX = 8;

function readJson(key, fallback) {
  try {
    return JSON.parse(localStorage.getItem(key) ?? 'null') ?? fallback;
  } catch {
    return fallback;
  }
}
function writeJson(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* 无痕模式等：只是下次不恢复 */
  }
}

/** 路径的最后一级（项目名）。 */
export function baseName(p) {
  const parts = (p ?? '').split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p ?? '';
}

export const projectKey = (device, workdir) => `${device ?? ''}\n${workdir}`;
const ts = (s) => Date.parse(s || '') || 0;
/** 会话最近一次有动静的时间（毫秒）。 */
export const lastUsed = (i) => Math.max(ts(i.last_activity_at), ts(i.started_at), ts(i.created_at));
/** 会话启动的时间（毫秒），排序用：运行时不会变。 */
export const startedAt = (i) => ts(i.started_at) || ts(i.created_at);
/** 会话在运行（终端或后台任务还在）。 */
export const isActive = (i) => Boolean(i.running || i.task_alive);
/** 会话列表的顺序：运行中的在前，再按启动时间从新到旧。 */
export const bySessionOrder = (a, b) => isActive(b) - isActive(a) || startedAt(b) - startedAt(a);
/** 会话显示的名字：agent 自己的会话名为主，没有时用会话名。 */
export const sessionTitle = (i) => i?.agent_title || i?.name || '';
/** 用户自己起的名字（和 agent 的会话名不一样时作为次要信息显示）。 */
export const customName = (i) => (i?.agent_title && i.name_custom && i.name !== i.agent_title ? i.name : null);
/** 会话的目录：容器里的会话是容器里的目录。 */
export const sessionDir = (i) => i?.container?.dir ?? i?.workdir ?? '';
/** 项目用的“文件夹”键：容器里的目录带上容器名，和电脑上同名的文件夹分开。 */
const dirKey = (i) => (i.container ? `docker://${i.container.name}${i.container.dir}` : i.workdir);

/** 会话现在的状态：等你处理 / 正在干活 / 运行中 / 后台运行中 / 已停止。 */
export function sessionState(i) {
  if (i.running && i.chat?.pending) return 'attn';
  if (i.running && i.chat?.phase === 'tui') return 'attn';
  if (i.running && i.chat?.phase === 'running') return 'working';
  if (i.running) return 'idle';
  return i.task_alive ? 'paused' : 'stopped';
}

/**
 * 多台电脑：分别向每台在线的电脑要会话列表，各自独立。一台卡住（超时）或出错只影响它自己那一组，
 * 其他电脑的先显示出来；同一台电脑上一次还没返回时不重复请求。
 */
function useDeviceGroups(online, key) {
  const [groups, setGroups] = useState({});
  const inflight = useRef(new Set());
  const alive = useRef(true);
  const ids = useRef([]);
  ids.current = online ? online.map((d) => d.id) : [];
  const loadOne = (id) => {
    if (inflight.current.has(id)) return;
    inflight.current.add(id);
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), GROUP_TIMEOUT_MS);
    deviceApi(id)
      .get('/instances', { signal: ctl.signal })
      .then(
        (r) => {
          const items = Array.isArray(r?.items) ? r.items : [];
          rememberTerminals(id, items);
          return { id, items };
        },
        // 超时：保留上一次拿到的列表，免得卡一下就整组消失
        (e) => ({ id, error: e?.name === 'AbortError' ? new ApiError(0, 'DEVICE_TIMEOUT') : e }),
      )
      .then((g) => {
        clearTimeout(timer);
        inflight.current.delete(id);
        if (!alive.current || !ids.current.includes(id)) return;
        setGroups((all) => ({ ...all, [id]: g.error ? { ...all[id], id, items: all[id]?.items ?? [], error: g.error } : g }));
      });
  };
  const reload = () => ids.current.forEach(loadOne);
  useEffect(() => {
    alive.current = true;
    setGroups((all) => Object.fromEntries(Object.entries(all).filter(([id]) => ids.current.includes(id))));
    reload();
    return () => {
      alive.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  if (!online) return [null, reload];
  const list = ids.current.map((id) => groups[id]).filter(Boolean);
  const items = list.flatMap((g) => g.items);
  // 还有电脑没返回、其他电脑也都没有会话时继续显示骨架，免得先闪一下“还没有会话”
  const settled = items.length > 0 || list.length === ids.current.length;
  return [settled ? { items, groups } : null, reload];
}

/** 只有一台电脑（或本机 / 局域网打开）时的会话列表。 */
function useSingle(enabled) {
  const [state, setState] = useState({ data: null, error: null });
  const busy = useRef(false);
  const reload = () => {
    if (!enabled || busy.current) return;
    busy.current = true;
    api
      .get('/instances')
      .then(
        (r) => setState({ data: { items: Array.isArray(r?.items) ? r.items : [] }, error: null }),
        (e) => setState((s) => ({ data: s.data, error: e })),
      )
      .finally(() => {
        busy.current = false;
      });
  };
  useEffect(reload, [enabled]);
  return [state.data, state.error, reload];
}

/** 每台电脑上记着的项目。`dev` 为 null 表示不分电脑。 */
function useStoredProjects(devIds, key) {
  const [store, setStore] = useState({});
  const load = (dev) =>
    deviceApi(dev)
      .get('/projects')
      .then(
        (r) => setStore((s) => ({ ...s, [dev ?? '']: Array.isArray(r?.items) ? r.items : [] })),
        // 旧版客户端没有这个接口：当作没有记录，项目从会话推出来
        () => setStore((s) => ({ ...s, [dev ?? '']: s[dev ?? ''] ?? [] })),
      );
  useEffect(() => {
    devIds.forEach(load);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  const save = (dev, items) => {
    setStore((s) => ({ ...s, [dev ?? '']: items }));
    return deviceApi(dev)
      .put('/projects', { items })
      .then((r) => Array.isArray(r?.items) && setStore((s) => ({ ...s, [dev ?? '']: r.items })))
      .catch(() => {});
  };
  return [store, save];
}

/**
 * 把会话和记着的项目合成项目列表：[{ key, device, workdir, name, launch, shell, full_access, auto_start, items, used, offline }]。
 * 项目按最近用过排序，项目里的会话也是。
 */
function mergeProjects({ devIds, store, items, multi, cache, onlineIds }) {
  const map = new Map();
  const add = (device, workdir, extra) => {
    const key = projectKey(device, workdir);
    if (!map.has(key)) map.set(key, { key, device, workdir, name: baseName(workdir) || workdir, launch: '', shell: '', full_access: false, auto_start: false, items: [], used: 0, ...extra });
    return map.get(key);
  };
  for (const dev of devIds) {
    if (multi && !onlineIds.has(dev)) {
      // 离线的电脑：上一次看到的项目，灰着显示
      for (const p of cache[dev] ?? []) add(dev, p.workdir, { offline: true, count: p.count ?? 0, used: p.used ?? 0 });
      continue;
    }
    for (const p of store[dev ?? ''] ?? []) add(dev, p.workdir, { launch: p.launch, shell: p.shell, full_access: p.full_access, auto_start: p.auto_start, used: ts(p.used_at) });
  }
  for (const i of items) {
    const dev = multi ? terminalDevice(i.id) : null;
    const p = add(dev, dirKey(i), i.container ? { launch: i.launch, container: i.container, name: baseName(i.container.dir) || i.container.dir } : { launch: i.launch });
    p.items.push(i);
  }
  const list = [...map.values()];
  for (const p of list) {
    p.items.sort(bySessionOrder);
    p.active = p.items.some(isActive);
    p.used = Math.max(p.used, ...p.items.map(startedAt), 0);
    if (!p.launch) p.launch = p.items[0]?.launch ?? 'claude';
  }
  return list.sort((a, b) => b.active - a.active || b.used - a.used || a.name.localeCompare(b.name));
}

export function SessionsProvider({ enabled, children }) {
  const devices = useDevices();
  const multi = devices.multi;
  const online = multi ? devices.items.filter((d) => d.online) : [];
  const onlineKey = online.map((d) => d.id).join(',');
  const [single, singleError, reloadSingle] = useSingle(enabled && !multi);
  const [grouped, reloadGroups] = useDeviceGroups(enabled && multi ? online : null, `${enabled}:${multi}:${onlineKey}`);
  const data = multi ? grouped : single;
  const reload = () => (multi ? reloadGroups() : reloadSingle());
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    if (!enabled) return undefined;
    const id = setInterval(() => !document.hidden && reloadRef.current(), POLL_MS);
    const onFocus = () => reloadRef.current();
    window.addEventListener('focus', onFocus);
    return () => {
      clearInterval(id);
      window.removeEventListener('focus', onFocus);
    };
  }, [enabled]);
  const devIds = multi ? devices.items.map((d) => d.id) : [null];
  const onlineDevIds = multi ? online.map((d) => d.id) : [null];
  const [store, saveStore] = useStoredProjects(enabled ? onlineDevIds : [], `${enabled}:${multi}:${onlineKey}`);
  const items = data?.items ?? [];
  const [cache, setCache] = useState(() => readJson(CACHE_KEY, {}));
  const projects = mergeProjects({ devIds, store, items, multi, cache, onlineIds: new Set(onlineDevIds) });
  // 记下每台在线电脑的项目，它离线后还能灰着显示
  const cacheSig = multi && data ? projects.filter((p) => !p.offline).map((p) => `${p.key}:${p.items.length}`).join('|') : '';
  useEffect(() => {
    if (!cacheSig) return;
    const next = { ...cache };
    for (const id of onlineDevIds) {
      if (!data.groups?.[id] || data.groups[id].error) continue;
      next[id] = projects.filter((p) => p.device === id).slice(0, 30).map((p) => ({ workdir: p.workdir, count: p.items.length, used: p.used }));
    }
    setCache(next);
    writeJson(CACHE_KEY, next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheSig]);

  // 在某个项目里新建了会话：记成最近用过，顺便记下这次的类型和选项
  const touchProject = (device, workdir, patch = {}) => {
    const cur = store[device ?? ''] ?? [];
    const old = cur.find((p) => p.workdir === workdir) ?? {};
    const next = [{ ...old, ...patch, workdir, used_at: new Date().toISOString() }, ...cur.filter((p) => p.workdir !== workdir)];
    return saveStore(device, next);
  };
  const removeProject = (device, workdir) => saveStore(device, (store[device ?? ''] ?? []).filter((p) => p.workdir !== workdir));

  // 最近打开的会话（在后台保持连接，切回去不用重新连）
  const [recent, setRecent] = useState(() => {
    const v = readJson(RECENT_KEY, []);
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string').slice(0, RECENT_MAX) : [];
  });
  useEffect(() => writeJson(RECENT_KEY, recent), [recent]);
  const openRecent = (id) => setRecent((r) => [id, ...r.filter((x) => x !== id)].slice(0, RECENT_MAX));
  const closeRecent = (id) => setRecent((r) => r.filter((x) => x !== id));

  // 顶部标签：按打开的先后排，新打开的放最后；关掉只是收起，终端照常运行
  const [tabs, setTabs] = useState(() => {
    const v = readJson(TABS_KEY, null) ?? readJson(RECENT_KEY, []);
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string').slice(0, TABS_MAX) : [];
  });
  useEffect(() => writeJson(TABS_KEY, tabs), [tabs]);
  const openTab = (id) => setTabs((t) => (t.includes(id) ? t : [...t, id].slice(-TABS_MAX)));
  const closeTabs = (ids) => {
    const gone = new Set(ids);
    setTabs((t) => t.filter((x) => !gone.has(x)));
    setRecent((r) => r.filter((x) => !gone.has(x)));
  };
  const moveTab = (id, to) =>
    setTabs((t) => {
      const rest = t.filter((x) => x !== id);
      rest.splice(Math.max(0, Math.min(to, rest.length)), 0, id);
      return rest;
    });
  // 电脑上点会话 / 标签后要把焦点交给它的输入框（终端或对话输入框）；同一个会话再点也要，所以带个计数
  const [focusReq, setFocusReq] = useState({ id: null, n: 0 });
  const requestFocus = (id) => setFocusReq((f) => ({ id, n: f.n + 1 }));

  const value = {
    multi,
    items,
    groups: data?.groups ?? null,
    loading: enabled && !data && !(multi ? false : singleError),
    error: multi ? null : singleError,
    reload,
    projects,
    touchProject,
    removeProject,
    recent,
    openRecent,
    closeRecent,
    tabs,
    openTab,
    closeTabs,
    moveTab,
    focusReq,
    requestFocus,
    byId: (id) => items.find((i) => i.id === id) ?? null,
    projectOf: (i) => projects.find((p) => p.items.includes(i)) ?? null,
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
