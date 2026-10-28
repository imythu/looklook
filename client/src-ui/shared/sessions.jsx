// 会话数据：所有电脑上的会话（终端）和项目，侧栏、开始页、会话页共用，每 5 秒刷新一次。
// 项目 = 某台电脑上的一个文件夹（或某个 Docker 容器里的目录）：那台电脑上记着的（GET/PUT /api/projects，上次用的类型和“高级”选项）
// 加上从会话的文件夹推出来的。离线的电脑拿不到列表，用上一次看到的（记在浏览器里）灰着显示。
// 每个项目还带着这个文件夹里以前的对话（GET /history/folders：各工具多少个、最近 5 个），侧栏里和会话合在一起列。
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
// 以前的对话变得慢，而且要扫记录文件：半分钟取一次（新建、接着聊之后马上取）
const FOLDERS_POLL_MS = 30000;
// 侧栏每个项目带几个以前的对话
const FOLDER_PER = 5;
const RECENT_KEY = 'looklook.recent';
const TABS_KEY = 'looklook.tabs';
// 标签所在的电脑（会话 id → 电脑 id，单台时为 'self'）：重新打开页面后还没拿到各台的会话列表时，
// 据此判断标签的会话是不是已经删掉了
const TABDEV_KEY = 'looklook.tabdev';
// 顶部最多几个标签（再多就把最早打开的收起）
export const TABS_MAX = 40;
const CACHE_KEY = 'looklook.devcache';
// 每台电脑最后一次拿到会话列表的时间：离线后显示“多久前看到的”
const SEEN_KEY = 'looklook.devseen';
const SEEN_EVERY_MS = 60000;
const seenWrote = {};
function markSeen(id) {
  const now = Date.now();
  if (now - (seenWrote[id] ?? 0) < SEEN_EVERY_MS) return;
  seenWrote[id] = now;
  writeJson(SEEN_KEY, { ...readJson(SEEN_KEY, {}), [id]: now });
}
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
/** 比较文件夹用：去掉末尾的分隔符；Windows 路径（`C:\…`）不分大小写、`/` 和 `\` 一样（同客户端 history.rs 的 dir_key）。 */
export function dirKey(p) {
  const s = /^[a-z]:[\\/]/i.test(p ?? '') || (p ?? '').includes('\\') ? p.replace(/\//g, '\\').toLowerCase() : p ?? '';
  return s.replace(/[\\/]+$/, '');
}
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
const instDir = (i) => (i.container ? `docker://${i.container.name}${i.container.dir}` : i.workdir);

/** 能接着以前的对话的工具（客户端 history.rs 的 AGENTS）。 */
export const AGENTS = ['claude', 'codex', 'opencode'];
/** 项目页的地址。 */
export const projectPath = (p) => `/project?${new URLSearchParams({ d: p.device ?? '', w: p.workdir })}`;

/**
 * 项目里的会话合成一个列表：在运行的（`live`）+ 停了的终端和以前的对话（`rest`，从新到旧）。
 * 那台电脑能接着以前的对话时，停了的 Claude Code / Codex / OpenCode 会话不单列（`stopped`）：它的对话在以前的对话里，
 * 点一下接着聊；不能接着、或者不知道能不能（旧版客户端）时照旧列出来。正开在会话里的对话只列那个会话。
 * `count` 是一共多少个（以前的对话按总数算，不只是带着的这几个）。`convs` 不给时用项目上带着的最近几个。
 */
export function projectEntries(p, convs = p.convs) {
  const live = p.items.filter(isActive);
  const liveIds = new Set(live.map((i) => i.id));
  const stopped = p.persistent === true ? p.items.filter((i) => !isActive(i) && AGENTS.includes(i.launch)) : [];
  const terms = p.items.filter((i) => !isActive(i) && !stopped.includes(i));
  const open = convs.filter((c) => !(c.instance_id && liveIds.has(c.instance_id)));
  const inst = (i) => ({ kind: 'inst', key: i.id, inst: i, at: lastUsed(i) });
  const rest = [...terms.map(inst), ...open.map((c) => ({ kind: 'conv', key: `${c.agent}:${c.id}`, conv: c, at: ts(c.updated_at) }))].sort((a, b) => b.at - a.at);
  return { live: live.map(inst), rest, stopped, count: live.length + terms.length + Math.max(p.total, convs.length) - (convs.length - open.length) };
}

/** 草稿所在的项目和侧栏里的项目是不是同一个（侧栏在这个项目最上面显示“新会话 · 草稿”）。 */
export function isDraftProject(p, at) {
  if (!at || p.device !== at.device) return false;
  return at.container ? p.container?.name === at.container.name && p.container?.dir === at.container.dir : !p.container && p.workdir === at.workdir;
}

/** 会话现在的状态：等你处理 / 正在干活 / 做完了（没看过）/ 出错停下（没看过）/ 运行中 / 后台运行中 / 已停止。 */
export function sessionState(i) {
  if (i.running && i.chat?.board === 'attn') return 'attn';
  if (i.running && i.chat?.pending) return 'attn';
  if (i.running && i.chat?.phase === 'tui') return 'attn';
  if (i.running && i.chat?.phase === 'running') return 'working';
  // 这一轮做完了、还没看过：完成蓝点，出错红点（打开看过就回到普通的运行中）
  if (i.running && i.chat?.board === 'ended') return i.chat.turn?.outcome === 'failed' ? 'failed' : 'done';
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
    if (inflight.current.has(id)) return undefined;
    inflight.current.add(id);
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), GROUP_TIMEOUT_MS);
    return deviceApi(id)
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
        if (!g.error) markSeen(id);
        setGroups((all) => ({ ...all, [id]: g.error ? { ...all[id], id, items: all[id]?.items ?? [], error: g.error } : g }));
      });
  };
  // 返回的 Promise 在这一轮都取完时完成（新建后要等它出现在列表里再打开）
  const reload = () => Promise.all(ids.current.map(loadOne));
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
    if (!enabled || busy.current) return undefined;
    busy.current = true;
    return api
      .get('/instances')
      .then(
        (r) => setState({ data: { items: Array.isArray(r?.items) ? r.items : [] }, error: null }),
        (e) => setState((s) => ({ data: s.data, error: e })),
      )
      .finally(() => {
        busy.current = false;
      });
  };
  useEffect(() => {
    // 停用（换成多台）时丢掉旧列表：再换回单台时那份已经过时，拿它判断“会话已删除”会误关标签
    if (!enabled) setState({ data: null, error: null });
    else reload();
  }, [enabled]);
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
 * 每台电脑上项目文件夹里以前的对话（只要记着的项目和会话所在的文件夹）。旧版客户端没有这个接口：当作没有。
 * `{ [电脑]: { items: [{ workdir, counts, total, updated_at, items }], persistent } }`
 */
function useFolders(devIds, key) {
  const [store, setStore] = useState({});
  const ids = useRef(devIds);
  ids.current = devIds;
  const inflight = useRef(new Set());
  const load = (dev) => {
    const k = dev ?? '';
    if (inflight.current.has(k)) return;
    inflight.current.add(k);
    deviceApi(dev)
      .get(`/history/folders?scope=projects&per=${FOLDER_PER}`)
      .then(
        (r) => setStore((s) => ({ ...s, [k]: { items: Array.isArray(r?.items) ? r.items : [], persistent: Boolean(r?.persistent) } })),
        // 出错：保留上一次的（离线、超时），没有过就当作没有（旧版客户端）
        () => setStore((s) => (s[k] ? s : { ...s, [k]: { items: [], persistent: null } })),
      )
      .finally(() => inflight.current.delete(k));
  };
  const reload = () => ids.current.forEach(load);
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    reload();
    const id = setInterval(() => !document.hidden && reloadRef.current(), FOLDERS_POLL_MS);
    const onFocus = () => reloadRef.current();
    window.addEventListener('focus', onFocus);
    return () => {
      clearInterval(id);
      window.removeEventListener('focus', onFocus);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return [store, reload];
}

/**
 * 每台在线电脑上各个项目文件夹的 git 远程地址（`POST /git/remotes`，规范后的 `host/path`）：`{ [电脑]: { [文件夹]: 地址 } }`。
 * 文件夹有变化时才重新问；旧版客户端没有这个接口，当作都没有（不合并）。
 */
function useRemotes(dirsByDev, enabled) {
  const [store, setStore] = useState({});
  // 每台电脑上次问过的文件夹：只有变了的那台再问
  const asked = useRef({});
  const sigOf = (dirs) => [...dirs].sort().join('\n');
  const sig = enabled ? Object.entries(dirsByDev).map(([d, dirs]) => `${d}=${sigOf(dirs)}`).sort().join('\t') : '';
  useEffect(() => {
    if (!sig) return;
    for (const [dev, dirs] of Object.entries(dirsByDev)) {
      const one = sigOf(dirs);
      if (!dirs.length || asked.current[dev] === one) continue;
      asked.current[dev] = one;
      deviceApi(dev)
        .post('/git/remotes', { dirs })
        .then(
          (r) => setStore((s) => ({ ...s, [dev]: r?.remotes && typeof r.remotes === 'object' ? r.remotes : {} })),
          (e) => {
            // 旧版客户端没有这个接口：不再问；网络出错下次再问
            if (e?.status !== 404 && e?.status !== 405) delete asked.current[dev];
            setStore((s) => (s[dev] ? s : { ...s, [dev]: {} }));
          },
        );
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig]);
  return store;
}

/**
 * 多台电脑上的同一个 git 仓库（远程地址相同）合成一组：`[{ key, name, members, primary }]`，顺序跟着项目列表。
 * 每台电脑只取一个文件夹（同一台上的两份克隆各自是项目）；合不起来的项目自己一组（members 只有它）。
 * `primary` 是新建时默认用的那个：在线的里面排在最前（最近用过）的。
 */
export function groupProjects(projects, multi) {
  const groups = [];
  const byRemote = new Map();
  for (const p of projects) {
    const r = multi && !p.container ? p.remote : null;
    const g = r ? byRemote.get(r) : null;
    if (g && !g.members.some((m) => m.device === p.device)) {
      g.members.push(p);
      continue;
    }
    const ng = { key: p.key, name: p.name, remote: r, members: [p] };
    if (r && !g) byRemote.set(r, ng);
    groups.push(ng);
  }
  for (const g of groups) {
    if (g.members.length > 1) g.key = `git:${g.remote}`;
    g.primary = g.members.find((m) => !m.offline) ?? g.members[0];
  }
  return groups;
}

/**
 * 把会话和记着的项目合成项目列表：[{ key, device, workdir, name, launch, shell, full_access, auto_start, pinned, items, used, offline,
 * convs, total, counts, convAt, persistent, remote }]。
 * `remote` 是 git 远程地址（规范后的，见 groupProjects），不是仓库或不知道时为 null；离线电脑的项目 `seenAt` 是最后一次拿到那台电脑会话列表的时间（不知道时 0）。
 * `full_access` 是在这个项目里新建时选过的，没选过为 null（见 Sessions.jsx 的 fullAccessFor）。
 * `convs` 是这个文件夹里最近的几个以前的对话，`total` / `counts` 是一共多少个（各工具多少个），`convAt` 是最近一个的时间；
 * `persistent` 为 false 时那台电脑不能接着以前的对话（没有会话保持），null 是不知道（旧版客户端）。
 * 置顶的在前，再按有没有在运行、最近用过排；项目里的会话按启动时间。
 */
function mergeProjects({ devIds, store, items, multi, cache, onlineIds, folders, remotes }) {
  const map = new Map();
  const add = (device, workdir, extra) => {
    const key = projectKey(device, workdir);
    if (!map.has(key)) map.set(key, { key, device, workdir, name: baseName(workdir) || workdir, launch: '', shell: '', full_access: null, auto_start: false, pinned: false, items: [], used: 0, convs: [], total: 0, counts: {}, convAt: 0, persistent: null, ...extra });
    return map.get(key);
  };
  const seen = multi ? readJson(SEEN_KEY, {}) : {};
  for (const dev of devIds) {
    if (multi && !onlineIds.has(dev)) {
      // 离线的电脑：上一次看到的项目，灰着显示
      for (const p of cache[dev] ?? []) add(dev, p.workdir, { offline: true, count: p.count ?? 0, used: p.used ?? 0, pinned: Boolean(p.pinned), convs: Array.isArray(p.convs) ? p.convs : [], total: p.total ?? 0, remote: typeof p.remote === 'string' ? p.remote : null, seenAt: seen[dev] ?? 0 });
      continue;
    }
    for (const p of store[dev ?? ''] ?? []) add(dev, p.workdir, { launch: p.launch, shell: p.shell, full_access: typeof p.full_access === 'boolean' ? p.full_access : null, auto_start: p.auto_start, pinned: Boolean(p.pinned), used: ts(p.used_at) });
  }
  for (const i of items) {
    const dev = multi ? terminalDevice(i.id) : null;
    const p = add(dev, instDir(i), i.container ? { launch: i.launch, container: i.container, name: baseName(i.container.dir) || i.container.dir } : { launch: i.launch });
    p.items.push(i);
  }
  const list = [...map.values()];
  for (const p of list) {
    p.items.sort(bySessionOrder);
    p.active = p.items.some(isActive);
    p.used = Math.max(p.used, ...p.items.map(startedAt), 0);
    const fs = !p.offline && !p.container ? folders[p.device ?? ''] : null;
    const f = fs?.items.find((x) => dirKey(x.workdir) === dirKey(p.workdir));
    if (fs) p.persistent = fs.persistent;
    // 没有会话保持的电脑接不了以前的对话，就不列
    if (f && fs.persistent !== false) {
      p.convs = f.items;
      p.total = f.total;
      p.counts = f.counts ?? {};
      p.convAt = ts(f.updated_at);
    }
    if (!p.launch) p.launch = p.items[0]?.launch ?? f?.items[0]?.agent ?? 'claude';
    if (!p.offline && !p.container) p.remote = remotes[p.device ?? '']?.[p.workdir] ?? null;
  }
  return list.sort((a, b) => b.pinned - a.pinned || b.active - a.active || b.used - a.used || a.name.localeCompare(b.name));
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
  const [folders, reloadFolders] = useFolders(enabled ? onlineDevIds : [], `${enabled}:${multi}:${onlineKey}`);
  // 正在删除的会话：点了删除就先从列表里拿掉（标签也随之关掉），删除失败再放回来
  const [deleting, setDeleting] = useState(() => new Set());
  const hide = (id, on) =>
    setDeleting((d) => {
      const n = new Set(d);
      if (on) n.add(id);
      else n.delete(id);
      return n;
    });
  const items = (data?.items ?? []).filter((i) => !deleting.has(i.id));
  const [cache, setCache] = useState(() => readJson(CACHE_KEY, {}));
  // 远程地址只在多台电脑时用得上（合并同一个仓库）
  const dirsByDev = {};
  if (multi && enabled) {
    for (const dev of onlineDevIds) dirsByDev[dev] = [];
    const seen = new Set();
    const want = (dev, dir) => {
      if (!dir || !(dev in dirsByDev) || seen.has(`${dev}\n${dir}`)) return;
      seen.add(`${dev}\n${dir}`);
      dirsByDev[dev].push(dir);
    };
    for (const dev of onlineDevIds) for (const p of store[dev] ?? []) want(dev, p.workdir);
    for (const i of items) if (!i.container) want(terminalDevice(i.id), i.workdir);
  }
  const remotes = useRemotes(dirsByDev, multi && enabled);
  const projects = mergeProjects({ devIds, store, items, multi, cache, onlineIds: new Set(onlineDevIds), folders, remotes });
  // 记下每台在线电脑的项目，它离线后还能灰着显示
  const cacheSig = multi && data ? projects.filter((p) => !p.offline).map((p) => `${p.key}:${p.items.length}:${p.total}:${p.convAt}:${p.pinned}:${p.remote ?? ''}`).join('|') : '';
  useEffect(() => {
    if (!cacheSig) return;
    const next = { ...cache };
    for (const id of onlineDevIds) {
      if (!data.groups?.[id] || data.groups[id].error) continue;
      next[id] = projects
        .filter((p) => p.device === id)
        .slice(0, 30)
        .map((p) => ({ workdir: p.workdir, count: p.items.length, used: p.used, pinned: p.pinned, total: p.total, remote: p.remote, convs: p.convs.map((c) => ({ agent: c.agent, id: c.id, title: c.title, updated_at: c.updated_at })) }));
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
  /** 添加项目（`list`：[{ workdir, used_at }]，used_at 不给时是现在），已经有的只更新时间。 */
  const addProjects = (device, list) => {
    const cur = store[device ?? ''] ?? [];
    const now = new Date().toISOString();
    const keys = new Set(list.map((x) => dirKey(x.workdir)));
    const added = list.map((x) => ({ ...cur.find((p) => dirKey(p.workdir) === dirKey(x.workdir)), workdir: x.workdir, used_at: x.used_at || now }));
    return saveStore(device, [...added, ...cur.filter((p) => !keys.has(dirKey(p.workdir)))]).then(reloadFolders);
  };
  /** 置顶 / 取消置顶（从会话推出来、还没记着的项目这时记下）。 */
  const pinProject = (device, workdir, pinned) => {
    const cur = store[device ?? ''] ?? [];
    const next = cur.some((p) => p.workdir === workdir) ? cur.map((p) => (p.workdir === workdir ? { ...p, pinned } : p)) : [...cur, { workdir, pinned, used_at: '' }];
    return saveStore(device, next);
  };

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
  const [tabDev, setTabDev] = useState(() => {
    const v = readJson(TABDEV_KEY, {});
    return v && typeof v === 'object' && !Array.isArray(v) ? v : {};
  });
  // 单台时那台电脑的 id（网页里只有一台、或者“只看这台电脑”）；本机 App 只看本机时不知道，记成 'self'
  const selfId = devices.pinned ?? (devices.items.length === 1 ? devices.items[0].id : null);
  // 记下各个标签（和后台保持连接的会话）在哪台电脑上；只留还开着的。单台时只记新开的（已经记着的不改）
  const owned = [...new Set([...tabs, ...recent])];
  const ownerOf = (id) => (multi ? terminalDevice(id) : null) ?? tabDev[id] ?? (multi ? null : selfId ?? 'self');
  const ownerSig = owned.map((id) => `${id}=${ownerOf(id) ?? ''}`).join(',');
  useEffect(() => {
    const next = {};
    for (const id of owned) {
      const dev = ownerOf(id);
      if (dev) next[id] = dev;
    }
    if (JSON.stringify(next) === JSON.stringify(tabDev)) return;
    setTabDev(next);
    writeJson(TABDEV_KEY, next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ownerSig]);
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
  /**
   * 会话已经被删掉了（标签自动关掉）：只有拿到了它所在那台电脑这一次的会话列表、里面却没有时才算。
   * 不知道在哪台电脑上、那台电脑离线或出错、列表还没到，都不算（不然重连、切换单台 / 多台时会把别的电脑的标签误关）。
   * 单台时只看本机的标签（'self'）；多台时看记下的那台电脑。都不知道的旧标签：每台电脑的列表都拿到了才算。
   */
  const gone = (id) => {
    if (items.some((i) => i.id === id)) return false;
    if (!multi) {
      if (!single || singleError) return false;
      const dev = tabDev[id];
      return !dev || dev === 'self' || dev === selfId;
    }
    const loaded = (dev) => Boolean(grouped?.groups?.[dev] && !grouped.groups[dev].error);
    const dev = terminalDevice(id) ?? (tabDev[id] !== 'self' ? tabDev[id] : null);
    if (dev) return loaded(dev);
    return devices.items.length > 0 && devices.items.every((d) => d.online && loaded(d.id));
  };
  /**
   * 会话所在的电脑连不上：多台时那台电脑离线、或者取它的会话列表出了网络错误；单台时取列表出了网络错误。
   * 打开着的标签不关、终端不卸载，内容照样能看（只是不能输入），电脑回来后终端自己重连。
   */
  const transient = (e) => Boolean(e) && (!e.status || e.status >= 500);
  const offline = (id) => {
    if (!multi) return transient(singleError);
    const dev = terminalDevice(id) ?? (tabDev[id] !== 'self' ? tabDev[id] : null);
    if (!dev) return false;
    return devices.byId(dev)?.online === false || transient(grouped?.groups?.[dev]?.error);
  };
  // 离线电脑上的会话从列表里没了：记下最后一次看到的样子，打开着的标签照样显示
  const lastSeen = useRef(new Map());
  for (const i of items) lastSeen.current.set(i.id, i);
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
    reloadFolders,
    /** 正在删除：先从列表里拿掉（`on` 为假时放回来） */
    hide,
    projects,
    /** 项目合成的组（多台电脑上的同一个仓库一组，见 groupProjects） */
    projectGroups: groupProjects(projects, multi),
    touchProject,
    removeProject,
    addProjects,
    pinProject,
    /** 那台电脑上记着的项目已经取到了（还没取到时项目列表里可能还缺它的项目） */
    storeLoaded: (device) => store[device ?? ''] !== undefined,
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
    /** 同 byId；电脑离线时给最后一次看到的（只用来显示） */
    known: (id) => items.find((i) => i.id === id) ?? (offline(id) ? lastSeen.current.get(id) ?? null : null),
    offline,
    gone,
    projectOf: (i) => projects.find((p) => p.items.includes(i)) ?? null,
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
