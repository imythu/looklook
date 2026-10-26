// Ctrl+K 命令面板：跳到等待回复的会话、按名字或目录找会话、常用命令（新会话、切标签、以前的对话、设置…）。
// 什么都不输入时先列等待回复的，再列最近用过的（记在这个浏览器里）。状态栏的 Ctrl K 也能打开（ll-palette 事件）。

import { Clock, FolderOpen, GitBranch, History, MessagesSquare, Plus, Search, Settings as SettingsIcon, SquareTerminal } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useSessionUi } from '../pages/Sessions.jsx';
import { LaunchIcon } from './brands.jsx';
import { useRouter } from './router.jsx';
import { baseName, sessionDir, sessionState, sessionTitle, useSessions } from './sessions.jsx';

const RECENT_KEY = 'looklook.palette.recent';
const RECENT_MAX = 5;
const SESS = /^\/s\/([a-z2-7]{8})$/;

/** 打开命令面板（状态栏按钮等用） */
export const openPalette = () => window.dispatchEvent(new Event('ll-palette'));

function readRecent() {
  try {
    const v = JSON.parse(localStorage.getItem(RECENT_KEY) || '[]');
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

function pushRecent(key) {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify([key, ...readRecent().filter((k) => k !== key)].slice(0, RECENT_MAX)));
  } catch {
    /* 存不了就不记 */
  }
}

const norm = (s) => String(s ?? '').toLowerCase();

export default function Palette() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const input = useRef(null);
  const list = useRef(null);
  const s = useSessions();
  const ui = useSessionUi();
  const { path, navigate } = useRouter();
  const current = path.match(SESS)?.[1] ?? null;

  useEffect(() => {
    const show = () => {
      setQ('');
      setSel(0);
      setOpen(true);
    };
    const onKey = (e) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setOpen((o) => {
          if (!o) {
            setQ('');
            setSel(0);
          }
          return !o;
        });
      }
    };
    window.addEventListener('ll-palette', show);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('ll-palette', show);
      window.removeEventListener('keydown', onKey);
    };
  }, []);
  useEffect(() => {
    if (open) setTimeout(() => input.current?.focus(), 0);
  }, [open]);

  const items = s?.items ?? [];
  const groups = useMemo(() => {
    if (!open) return [];
    const sessionCmd = (i, sub) => ({
      key: `s:${i.id}`,
      icon: <LaunchIcon launch={i.launch} size={15} />,
      label: sessionTitle(i) || baseName(sessionDir(i)),
      sub,
      run: () => ui.openSession(i.id),
    });
    const tabCmd = (tab, Icon, label) => ({ key: `tab:${tab}`, icon: <Icon size={15} />, label: t(label), run: () => window.dispatchEvent(new CustomEvent('ll-tab', { detail: { id: current, tab } })) });
    const commands = [
      { key: 'new', icon: <Plus size={15} />, label: t('sess.new'), hint: 'Ctrl ⇧ O', run: () => ui.newSession() },
      ...(current
        ? [tabCmd('chat', MessagesSquare, 'pal.tab_chat'), tabCmd('term', SquareTerminal, 'pal.tab_term'), tabCmd('git', GitBranch, 'pal.tab_git'), tabCmd('files', FolderOpen, 'pal.tab_files')]
        : []),
      { key: 'history', icon: <History size={15} />, label: t('hist.title'), run: () => navigate('/history') },
      { key: 'settings', icon: <SettingsIcon size={15} />, label: t('nav.settings'), run: () => navigate('/settings') },
    ];
    const waiting = items.filter((i) => sessionState(i) === 'attn').map((i) => sessionCmd(i, t('pal.waiting_sub', { dir: baseName(sessionDir(i)) })));
    const query = norm(q.trim());
    if (!query) {
      const all = new Map([...commands, ...items.map((i) => sessionCmd(i, baseName(sessionDir(i))))].map((c) => [c.key, c]));
      const recent = readRecent()
        .map((k) => all.get(k))
        .filter((c) => c && !waiting.some((w) => w.key === c.key));
      return [
        { title: t('pal.waiting', { n: waiting.length }), items: waiting },
        { title: t('pal.recent'), items: recent.map((c) => ({ ...c, recent: true })) },
        { title: t('pal.commands'), items: commands.filter((c) => !recent.some((r) => r.key === c.key)) },
      ].filter((g) => g.items.length);
    }
    const hit = (...xs) => xs.some((x) => norm(x).includes(query));
    const sessions = items
      .filter((i) => hit(sessionTitle(i), i.name, sessionDir(i)))
      .slice(0, 8)
      .map((i) => sessionCmd(i, sessionDir(i)));
    return [
      { title: t('pal.waiting', { n: waiting.length }), items: waiting.filter((c) => hit(c.label, c.sub)) },
      { title: t('pal.sessions'), items: sessions.filter((c) => !waiting.some((w) => w.key === c.key)) },
      { title: t('pal.commands'), items: commands.filter((c) => hit(c.label)) },
    ].filter((g) => g.items.length);
  }, [open, q, items, current, t]);

  const flat = groups.flatMap((g) => g.items);
  useEffect(() => {
    if (sel >= flat.length) setSel(Math.max(0, flat.length - 1));
  }, [flat.length]);
  useEffect(() => {
    list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [sel, open]);

  if (!open) return null;
  const close = () => setOpen(false);
  const exec = (c) => {
    if (!c) return;
    pushRecent(c.key);
    close();
    c.run();
  };
  const onKeyDown = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSel((v) => (flat.length ? (v + 1) % flat.length : 0));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSel((v) => (flat.length ? (v - 1 + flat.length) % flat.length : 0));
    } else if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      exec(flat[sel]);
    }
  };
  let n = -1;
  return (
    <div className="pal-backdrop" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div className="pal" role="dialog" aria-modal="true" aria-label={t('pal.title')}>
        <div className="pal-search">
          <Search size={16} aria-hidden="true" />
          <input
            ref={input}
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setSel(0);
            }}
            onKeyDown={onKeyDown}
            placeholder={t('pal.placeholder')}
            aria-label={t('pal.placeholder')}
            role="combobox"
            aria-expanded="true"
            aria-controls="pal-list"
          />
          <kbd className="pal-kbd">Esc</kbd>
        </div>
        <div className="pal-list" id="pal-list" role="listbox" ref={list}>
          {groups.map((g) => (
            <div key={g.title} role="group" aria-label={g.title}>
              <div className="pal-sec">{g.title}</div>
              {g.items.map((c) => {
                n += 1;
                const i = n;
                return (
                  <div key={c.key} role="option" aria-selected={i === sel} className="pal-row" onPointerMove={() => setSel(i)} onClick={() => exec(c)}>
                    <span className="pal-icon">{c.recent ? <Clock size={15} /> : c.icon}</span>
                    <span className="pal-label ellipsis">{c.label}</span>
                    {c.sub && <span className="pal-sub ellipsis">{c.sub}</span>}
                    {c.hint && <kbd className="pal-kbd">{c.hint}</kbd>}
                  </div>
                );
              })}
            </div>
          ))}
          {!flat.length && <div className="pal-empty">{t('pal.empty')}</div>}
        </div>
        <div className="pal-foot">
          <span><kbd className="pal-kbd">↑</kbd> <kbd className="pal-kbd">↓</kbd> {t('pal.foot_pick')}</span>
          <span><kbd className="pal-kbd">Enter</kbd> {t('pal.foot_run')}</span>
        </div>
      </div>
    </div>
  );
}
