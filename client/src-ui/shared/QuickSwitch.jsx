// 快速切换（⌘K / Ctrl+K，docs/UI_REDESIGN_2026-10.md §5.1）：一个搜索框，搜开着的会话、所有会话、项目、以前的对话，
// 以及常用动作（新会话、任务看板、各个工具页、设置）。上下键选，回车打开，Esc 关掉。
// 别处要打开它时发 `ll-quick-switch` 事件（会话栏搜索框右边的提示）。
import { Activity, Bot, FolderOpen, Globe, History, ListTodo, Plus, Search, Server, Settings as SettingsIcon } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { useSessionUi } from '../pages/Sessions.jsx';
import { terminalDevice } from './api.js';
import { LaunchIcon } from './brands.jsx';
import { deviceName, useDevices } from './devices.jsx';
import { errorText } from './i18n.js';
import { isMod, keyLabel } from './hotkeys.js';
import { useRouter } from './router.jsx';
import { projectPath, sessionDir, sessionState, sessionTitle, useSessions } from './sessions.jsx';
import { useToast } from './ui.jsx';
import { imeComposing } from './ime.js';

const ACTIONS = [
  { key: 'new', icon: Plus, label: 'sess.new', hint: 'mod+shift+O', run: (c) => c.ui.newSession() },
  { key: 'tasks', icon: ListTodo, label: 'board.title', run: (c) => c.navigate('/tasks') },
  { key: 'history', icon: History, label: 'hist.title', run: (c) => c.navigate('/history') },
  { key: 'files', icon: FolderOpen, label: 'nav.files', run: (c) => c.navigate('/files') },
  { key: 'ssh', icon: Server, label: 'nav.ssh', run: (c) => c.navigate('/ssh') },
  { key: 'pages', icon: Globe, label: 'nav.pages', run: (c) => c.navigate('/pages') },
  { key: 'ai', icon: Bot, label: 'nav.ai', run: (c) => c.navigate('/ai') },
  { key: 'system', icon: Activity, label: 'nav.system', run: (c) => c.navigate('/system') },
  { key: 'settings', icon: SettingsIcon, label: 'nav.settings', run: (c) => c.navigate('/settings') },
];

/** 每个词都要出现；标题开头就对上的排前面。 */
function score(words, title, rest) {
  if (!words.length) return 1;
  const t = title.toLowerCase();
  const hay = `${t} ${rest.toLowerCase()}`;
  if (!words.every((w) => hay.includes(w))) return 0;
  return t.startsWith(words[0]) ? 3 : words.every((w) => t.includes(w)) ? 2 : 1;
}

export default function QuickSwitch() {
  const { t } = useTranslation();
  const s = useSessions();
  const ui = useSessionUi();
  const devices = useDevices();
  const { navigate } = useRouter();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const [at, setAt] = useState(0);
  const input = useRef(null);
  const list = useRef(null);
  useEffect(() => {
    const show = () => {
      setQ('');
      setAt(0);
      setOpen(true);
    };
    const key = (e) => {
      if (isMod(e, 'k')) {
        e.preventDefault();
        setOpen((o) => {
          if (!o) {
            setQ('');
            setAt(0);
          }
          return !o;
        });
      }
    };
    window.addEventListener('keydown', key);
    window.addEventListener('ll-quick-switch', show);
    return () => {
      window.removeEventListener('keydown', key);
      window.removeEventListener('ll-quick-switch', show);
    };
  }, []);
  useEffect(() => {
    if (open) requestAnimationFrame(() => input.current?.focus());
  }, [open]);
  // 以前的对话：最近还在写的（可能正开在别处）先去项目页看一眼，跟侧栏一样，不直接接着
  const openConv = (p, c) =>
    c.live && !c.instance_id
      ? navigate(`${projectPath(p)}&c=${encodeURIComponent(`${c.agent}:${c.id}`)}`)
      : ui.continueConv(p.device, c).catch((e) => toast(errorText(t, e), 'error'));
  const results = useMemo(() => {
    if (!open) return [];
    const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const out = [];
    const dev = (id) => (devices.multi ? deviceName(t, devices.byId(id)) : '');
    // 会话：开着的标签在前，等你处理的、正在干活的其次
    const order = (i) => (s.tabs.includes(i.id) ? 0 : sessionState(i) === 'attn' ? 1 : sessionState(i) === 'working' ? 2 : 3);
    for (const i of [...s.items].sort((a, b) => order(a) - order(b))) {
      const title = sessionTitle(i);
      const d = devices.multi ? dev(terminalDevice(i.id)) : '';
      const sub = [d, sessionDir(i)].filter(Boolean).join(' · ');
      const sc = score(words, title, `${sub} ${i.launch} ${i.name ?? ''}`);
      if (sc) out.push({ key: `s:${i.id}`, group: 'sessions', sc: sc + (order(i) === 0 ? 0.5 : 0), icon: <LaunchIcon launch={i.launch} size={16} />, title, sub, state: sessionState(i), run: () => ui.openSession(i.id) });
    }
    for (const p of s.projects) {
      const sub = [dev(p.device), p.container ? `${p.container.name}:${p.container.dir}` : p.workdir].filter(Boolean).join(' · ');
      const sc = score(words, p.name, sub);
      if (sc) out.push({ key: `p:${p.key}`, group: 'projects', sc, icon: <FolderOpen size={16} aria-hidden="true" />, title: p.name, sub, run: () => (p.container ? ui.draftIn(p) : navigate(projectPath(p))) });
      if (!words.length) continue;
      for (const c of p.convs ?? []) {
        const csc = score(words, c.title ?? '', `${c.last ?? ''} ${p.name}`);
        if (csc) out.push({ key: `c:${p.key}:${c.agent}:${c.id}`, group: 'convs', sc: csc, icon: <LaunchIcon launch={c.agent} size={16} />, title: c.title, sub: [p.name, dev(p.device)].filter(Boolean).join(' · '), run: () => openConv(p, c) });
      }
    }
    for (const a of ACTIONS) {
      const label = t(a.label);
      const sc = score(words, label, a.key);
      if (sc) out.push({ key: `a:${a.key}`, group: 'actions', sc, icon: <a.icon size={16} aria-hidden="true" />, title: label, hint: a.hint && keyLabel(a.hint), run: () => a.run({ ui, navigate }) });
    }
    const groupOrder = words.length ? ['sessions', 'projects', 'convs', 'actions'] : ['actions', 'sessions', 'projects'];
    const limit = { sessions: words.length ? 12 : 8, projects: words.length ? 8 : 5, convs: 8, actions: words.length ? 6 : 4 };
    return groupOrder.flatMap((g) =>
      out
        .filter((x) => x.group === g)
        .sort((a, b) => b.sc - a.sc)
        .slice(0, limit[g]),
    );
  }, [open, q, s.items, s.projects, s.tabs, devices.items]);
  useEffect(() => {
    list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [at]);
  if (!open) return null;
  const close = () => setOpen(false);
  const go = (r) => {
    close();
    Promise.resolve(r.run()).catch(() => {});
  };
  const onKey = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (results.length) setAt((x) => (x + (e.key === 'ArrowUp' ? -1 : 1) + results.length) % results.length);
    } else if (e.key === 'Enter' && !imeComposing(e)) {
      e.preventDefault();
      if (results[at]) go(results[at]);
    }
  };
  let last = null;
  return createPortal(
    <div className="qs-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="qs" role="dialog" aria-modal="true" aria-label={t('qs.title')}>
        <label className="qs-input">
          <Search size={17} aria-hidden="true" />
          <input
            ref={input}
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setAt(0);
            }}
            onKeyDown={onKey}
            placeholder={t('qs.placeholder')}
            aria-label={t('qs.title')}
            role="combobox"
            aria-expanded="true"
            aria-controls="qs-list"
            aria-activedescendant={results[at] ? `qs-${at}` : undefined}
            spellCheck={false}
            autoComplete="off"
          />
          <kbd>Esc</kbd>
        </label>
        <div className="qs-list" id="qs-list" role="listbox" ref={list}>
          {results.map((r, n) => {
            const head = r.group !== last;
            last = r.group;
            return (
              <div key={r.key} role="presentation">
                {head && <p className="qs-head">{t(`qs.group.${r.group}`)}</p>}
                <button type="button" id={`qs-${n}`} role="option" aria-selected={n === at} className="qs-row" onMouseMove={() => n !== at && setAt(n)} onClick={() => go(r)}>
                  <span className="qs-icon">{r.icon}</span>
                  <span className="qs-text">
                    <span className="ellipsis">{r.title}</span>
                    {r.sub && <span className="qs-sub ellipsis">{r.sub}</span>}
                  </span>
                  {r.state && r.state !== 'idle' && r.state !== 'stopped' && <span className={`sdot sdot-${r.state}`} aria-hidden="true" />}
                  {r.hint && <kbd>{r.hint}</kbd>}
                </button>
              </div>
            );
          })}
          {results.length === 0 && <p className="qs-empty">{t('sess.no_match')}</p>}
        </div>
        <p className="qs-foot">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> {t('qs.move')}
          </span>
          <span>
            <kbd>↵</kbd> {t('qs.open')}
          </span>
        </p>
      </div>
    </div>,
    document.body,
  );
}
